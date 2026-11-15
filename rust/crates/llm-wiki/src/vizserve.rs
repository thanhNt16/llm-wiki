//! Loopback visualizer server with live reload — port of
//! `wikicore/vizserve.py`.
//!
//! Serves the prebuilt wiki-visualize UI on 127.0.0.1 only, regenerates
//! `graph-data.json` on every request (always live — the UI's shipped copy
//! is stale), and pushes `event: reload` to connected browsers over SSE —
//! the Python ThreadingHTTPServer/_watch_loop design mirrored on
//! `tiny_http`. Read-only against the wiki: regeneration uses
//! [`graphdata`], never a Transaction.
//!
//! UI dist resolution (spec §12): `--ui-dist` flag > `WIKI_UI_DIST` env >
//! repo checkout (cwd + executable ancestors probed for
//! `skill/wiki-visualize/assets/graph-ui/dist`) > embedded. The embedded
//! copy is a `rust-embed` image of `skill/wiki-visualize/assets/graph-ui/dist`
//! served from memory under `graph-ui/dist/` keys — no disk extraction.
//!
//! Shutdown: SIGINT/SIGTERM use the default disposition — process death is
//! the clean shutdown (Python parity: `serve_forever` + `server_close`,
//! which also never ran on SIGKILL/SIGTERM). No extra handler needed: the
//! server owns no resources beyond the listen socket.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use rust_embed::RustEmbed;
use serde_json::{json, Value};
use tiny_http::{Header, Method, Request, Response, Server, StatusCode};

use crate::err::EngineError;
use crate::graphdata;
use crate::store::Wiki;

const POLL_S: Duration = Duration::from_millis(500);
/// Debounce: push after the store stops changing.
const QUIET_S: Duration = Duration::from_millis(300);
/// SSE comment keep-alive — keeps half-dead connections detectable.
const PING_S: Duration = Duration::from_secs(5);
/// Concurrent request handlers (Python's ThreadingHTTPServer is unbounded;
/// 16 covers browser parallelism + a few parked EventSource streams).
const WORKERS: usize = 16;
/// Relative repo-checkout location of the prebuilt UI.
const REPO_DIST: &str = "skill/wiki-visualize/assets/graph-ui/dist";

/// Embedded `graph-ui/dist` (spec §12). Keys are `graph-ui/dist/<rel>` —
/// same shape as the installed layout `vizexport::ui_dist` probes next to
/// the executable. `debug-embed` keeps debug/test builds serving from
/// memory, not the checkout.
#[derive(RustEmbed)]
#[folder = "../../../skill/wiki-visualize/assets/graph-ui/dist"]
#[prefix = "graph-ui/dist/"]
struct EmbeddedUi;

/// Where the UI bundle is served from.
enum UiSource {
    /// Resolved on-disk dist dir (`--ui-dist`, `WIKI_UI_DIST`, repo checkout).
    Disk(PathBuf),
    /// In-memory rust-embed image.
    Embedded,
}

/// `vizexport.ui_dist` + the spec-§12 embedded fallback.
///
/// Resolution order: `--ui-dist` > `WIKI_UI_DIST` > repo checkout
/// (ancestors of cwd and of the executable) > embedded. `None` in the list
/// marks "probe for a checkout"; it only produces a candidate that
/// actually contains `index.html`, so a missing checkout falls through to
/// `Embedded` instead of failing like Python's `ui_dist`.
fn resolve_ui(explicit: Option<&Path>) -> Result<UiSource, EngineError> {
    let mut cands: Vec<Option<PathBuf>> = Vec::new();
    if let Some(e) = explicit {
        cands.push(Some(e.to_path_buf()));
    }
    if let Ok(env) = std::env::var("WIKI_UI_DIST") {
        if !env.is_empty() {
            cands.push(Some(PathBuf::from(env)));
        }
    }
    // Repo-checkout probes: cwd ancestors (test/dev launch dirs) and
    // executable ancestors (vizexport::ui_dist parity).
    if let Ok(cwd) = std::env::current_dir() {
        for anc in cwd.ancestors() {
            cands.push(Some(anc.join(REPO_DIST)));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            cands.push(Some(dir.join("graph-ui/dist")));
            for anc in dir.ancestors() {
                cands.push(Some(anc.join(REPO_DIST)));
            }
        }
    }
    cands.push(None); // embedded fallback, always last
    for c in &cands {
        match c {
            Some(dir) if dir.join("index.html").is_file() => {
                return Ok(UiSource::Disk(dir.clone()));
            }
            Some(_) => {}
            None => return Ok(UiSource::Embedded),
        }
    }
    unreachable!("embedded fallback is last and always returns")
}

/// Python `_store_signature`: (path, mtime_ns, size) for every canonical
/// file under claims/decisions/sources plus wiki.json.
fn store_signature(wiki: &Wiki) -> Vec<(PathBuf, u128, u64)> {
    fn stat_sig(p: &Path) -> Option<(PathBuf, u128, u64)> {
        let st = fs::metadata(p).ok()?;
        let mtime_ns = st
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        Some((p.to_path_buf(), mtime_ns, st.len()))
    }
    let mut sig = Vec::new();
    for sub in ["claims", "decisions", "sources"] {
        let d = wiki.p(sub);
        if !d.is_dir() {
            continue;
        }
        // os.walk parity: recursion follows the real tree; failures skip.
        let mut stack = vec![d];
        while let Some(dir) = stack.pop() {
            let mut names: Vec<PathBuf> = match fs::read_dir(&dir) {
                Ok(rd) => rd.filter_map(|e| e.ok().map(|e| e.path())).collect(),
                Err(_) => continue,
            };
            names.sort();
            for p in names {
                if p.is_dir() {
                    stack.push(p);
                } else if let Some(s) = stat_sig(&p) {
                    sig.push(s);
                }
            }
        }
    }
    let f = wiki.p("wiki.json");
    if f.is_file() {
        if let Some(s) = stat_sig(&f) {
            sig.push(s);
        }
    }
    sig
}

/// SSE frame bytes — written straight to the socket via
/// `Request::into_writer` (tiny_http's chunked `Response` body buffers
/// 8KiB before flushing, which would stall frames; Python's `wfile` has
/// no such buffer, so the raw writer is the faithful port).
type SseFrame = Vec<u8>;

struct State {
    wiki: Wiki,
    ui: UiSource,
    /// (payload_json, node_count) of the last regeneration.
    payload: Mutex<Option<(String, usize)>>,
    /// SSE client frame queues — (id, sender) so a disconnecting client
    /// can evict itself (Python removes `self.wfile` from the list).
    clients: Mutex<Vec<(u64, SyncSender<SseFrame>)>>,
    next_client_id: AtomicU64,
    /// serializes regenerations (Python `state.lock` parity).
    regen_lock: Mutex<()>,
}

impl State {
    /// Python `state.regenerate()` — rebuild the payload and swap it in.
    fn regenerate(&self) -> Result<(String, usize), EngineError> {
        let _ser = self.regen_lock.lock().unwrap();
        let payload = graphdata::graph_payload(&self.wiki, true)?;
        let nodes = payload
            .get("nodes")
            .and_then(Value::as_array)
            .map(|n| n.len())
            .unwrap_or(0);
        let text = serde_json::to_string(&payload)?;
        *self.payload.lock().unwrap() = Some((text.clone(), nodes));
        Ok((text, nodes))
    }

    fn payload_json(&self) -> Option<String> {
        self.payload.lock().unwrap().as_ref().map(|p| p.0.clone())
    }

    /// Python `broadcast_reload`: fan the reload frame out to every SSE
    /// client; drop queues that are full or disconnected.
    fn broadcast_reload(&self, data: &str) {
        let msg = format!("event: reload\ndata: {data}\n\n").into_bytes();
        let mut clients = self.clients.lock().unwrap();
        // keep senders that accept the frame; drop full or closed queues.
        clients.retain(|(_, tx)| match tx.try_send(msg.clone()) {
            Ok(()) => true,
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => false,
        });
    }
}

/// `_watch_loop` — poll the store signature; on change, debounce
/// (`QUIET_S` of quiet), regenerate, and broadcast.
fn watch_loop(state: Arc<State>) {
    let mut last = store_signature(&state.wiki);
    loop {
        thread::sleep(POLL_S);
        let cur = store_signature(&state.wiki);
        if cur == last {
            continue;
        }
        thread::sleep(QUIET_S);
        if store_signature(&state.wiki) != cur {
            continue;
        }
        last = cur;
        let data = match state.regenerate() {
            Ok((d, _)) => d,
            Err(_) => continue, // transient mid-write state; next poll retries
        };
        state.broadcast_reload(&data);
    }
}

/// `vizexport.py:140-144` extension map (keys include the dot, so build
/// `".<ext>"` from `Path::extension`).
fn content_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{}", e.to_ascii_lowercase()))
        .as_deref()
        .unwrap_or("")
    {
        ".html" => "text/html; charset=utf-8",
        ".js" => "text/javascript",
        ".css" => "text/css",
        ".json" => "application/json",
        ".svg" => "image/svg+xml",
        ".png" => "image/png",
        ".ico" => "image/x-icon",
        _ => "application/octet-stream",
    }
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).unwrap()
}

fn send(req: Request, code: u16, ctype: &str, body: Vec<u8>) {
    let len = body.len();
    let resp = Response::new(
        StatusCode(code),
        vec![
            header("Content-Type", ctype),
            header("Cache-Control", "no-store"),
        ],
        io::Cursor::new(body),
        Some(len),
        None,
    );
    // respond() errors mean the client went away — nothing to do.
    let _ = req.respond(resp);
}


/// Normalize `path.lstrip("/")` the way `os.path.normpath` does, rejecting
/// any `..` escape (vizserve.py:132-134 → 403).
fn safe_rel(path: &str) -> Option<PathBuf> {
    let mut rel = PathBuf::new();
    for part in path.trim_start_matches('/').split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if !rel.pop() {
                    return None;
                }
            }
            seg => rel.push(seg),
        }
    }
    Some(rel)
}

/// File lookup against the active UI source (`isfile` + read parity).
/// `rel` is already normalized/escape-checked by `safe_rel`.
fn read_ui_file(ui: &UiSource, rel: &Path) -> Option<Vec<u8>> {
    match ui {
        UiSource::Disk(root) => {
            let fp = root.join(rel);
            if !fp.is_file() {
                return None;
            }
            fs::read(&fp).ok()
        }
        UiSource::Embedded => {
            let key = format!("graph-ui/dist/{}", rel.to_string_lossy().replace('\\', "/"));
            EmbeddedUi::get(&key).map(|f| f.data.into_owned())
        }
    }
}

/// One request → one response (vizserve.py `_make_handler.do_GET` parity).
fn handle_request(state: &Arc<State>, req: Request) {
    let method = req.method().clone();
    if !matches!(method, Method::Get | Method::Head) {
        // BaseHTTPRequestHandler sends 501 for unimplemented verbs.
        send(req, 501, "text/plain", b"unsupported method".to_vec());
        return;
    }
    let path = req.url().split('?').next().unwrap_or("/").to_string();
    if path == "/events" {
        return serve_events(state, req);
    }
    if path == "/graph-data.json" {
        // Always live: the template's shipped copy is stale.
        let data = match state.payload_json() {
            Some(d) => d,
            None => match state.regenerate() {
                Ok((d, _)) => d,
                Err(e) => {
                    return send(req, 500, "text/plain", e.to_string().into_bytes())
                }
            },
        };
        return send(req, 200, "application/json", data.into_bytes());
    }
    let rel = match if path == "/" || path == "/index.html" {
        Some(PathBuf::from("index.html"))
    } else {
        safe_rel(&path)
    } {
        Some(r) => r,
        None => return send(req, 403, "text/plain", b"forbidden".to_vec()),
    };
    match read_ui_file(&state.ui, &rel) {
        None => send(req, 404, "text/plain", b"not found".to_vec()),
        Some(bytes) => send(req, 200, content_type(&rel), bytes),
    }
}

/// `/events` — the SSE stream (vizserve.py:148-178).
fn serve_events(state: &Arc<State>, req: Request) {
    // hello stays small: node count only, never the whole payload.
    let nodes = match state.payload.lock().unwrap().as_ref() {
        Some((_, n)) => *n,
        None => match state.regenerate() {
            Ok((_, n)) => n,
            Err(e) => {
                return send(req, 500, "text/plain", e.to_string().into_bytes())
            }
        },
    };
    let hello = format!("retry: 1000\n\nevent: hello\ndata: {{\"nodes\": {nodes}}}\n\n");
    // Raw socket writer — Python `wfile` parity. HTTP/1.0 keep-alive head:
    // tiny_http advertises no protocol version to handlers; the Python
    // server negotiates HTTP/1.1, but frames + Connection: close are
    // valid on either (no Content-Length, EOF- or disconnect-terminated).
    let mut w = req.into_writer();
    let head = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\n\
         Cache-Control: no-store\r\nConnection: close\r\n\r\n{hello}"
    );
    if w.write_all(head.as_bytes()).and_then(|_| w.flush()).is_err() {
        return;
    }
    let id = state.next_client_id.fetch_add(1, Ordering::Relaxed);
    let (tx, rx) = sync_channel::<SseFrame>(64);
    state.clients.lock().unwrap().push((id, tx));
    loop {
        // SSE comments keep half-dead connections detectable.
        let frame = match rx.recv_timeout(PING_S) {
            Ok(f) => f,
            Err(RecvTimeoutError::Timeout) => b": ping\n\n".to_vec(),
            Err(RecvTimeoutError::Disconnected) => break,
        };
        if w.write_all(&frame).and_then(|_| w.flush()).is_err() {
            break;
        }
    }
    // Python finally-block: remove self.wfile from state.clients.
    state.clients.lock().unwrap().retain(|(cid, _)| *cid != id);
    drop(w);
}

/// Python `serve()` — resolve the UI, regenerate once, bind the first free
/// port in `port..=port+20`, launch the watch loop + handler workers,
/// print `{"serving","ui"}`, open the browser, then block forever.
///
/// Returns `Err` only for pre-bind failures (bad UI, empty/invalid wiki,
/// no free port). Never returns `Ok`.
pub fn serve(
    wiki: Wiki,
    port: u16,
    ui_dist: Option<&Path>,
    no_open: bool,
) -> Result<Value, EngineError> {
    let ui = resolve_ui(ui_dist)?;
    let ui_label = match &ui {
        UiSource::Disk(d) => d.display().to_string(),
        UiSource::Embedded => "embedded:graph-ui/dist".to_string(),
    };
    let state = Arc::new(State {
        wiki,
        ui,
        payload: Mutex::new(None),
        clients: Mutex::new(Vec::new()),
        next_client_id: AtomicU64::new(1),
        regen_lock: Mutex::new(()),
    });
    state.regenerate()?;

    // Port scan: vizserve.py:191-198 — try port..port+20, any bind error
    // advances to the next candidate; all busy → error.
    let mut server = None;
    let mut bound_port = port;
    let mut last_err: Option<Box<dyn std::error::Error + Send + Sync>> = None;
    let top = (port as u32 + 20).min(65535) as u16;
    for cand in port..=top {
        match Server::http(("127.0.0.1", cand)) {
            Ok(s) => {
                server = Some(s);
                bound_port = cand;
                break;
            }
            Err(e) => last_err = Some(e),
        }
    }
    let server = match server {
        Some(s) => Arc::new(s),
        None => {
            return Err(EngineError::Txn(format!(
                "no free loopback port near {port}: {}",
                last_err.map(|e| e.to_string()).unwrap_or_default()
            )));
        }
    };
    let url = format!("http://127.0.0.1:{bound_port}/");

    thread::spawn({
        let state = state.clone();
        move || watch_loop(state)
    });

    // tiny_http is single-consumer per Server::recv queue — worker threads
    // take one request each, like ThreadingHTTPServer's per-connection
    // threads but pooled.
    for _ in 0..WORKERS {
        let server = server.clone();
        let state = state.clone();
        thread::spawn(move || loop {
            match server.recv() {
                Ok(req) => handle_request(&state, req),
                Err(_) => return, // unblocked/fatal queue error
            }
        });
    }

    // Python prints {"serving": url, "ui": ui_dir} before opening the
    // browser; the line must reach piped readers, so flush explicitly.
    let line = json!({ "serving": url, "ui": ui_label });
    println!("{line}");
    let _ = io::stdout().flush();
    if !no_open {
        open_browser(&url);
    }

    loop {
        thread::park();
    }
}

/// `webbrowser.open` without the Python stdlib — platform opener, fired
/// and forgotten (vizserve.py:204).
fn open_browser(url: &str) {
    let spawned = if cfg!(target_os = "macos") {
        std::process::Command::new("open").arg(url).spawn()
    } else if cfg!(target_os = "windows") {
        std::process::Command::new("cmd")
            .args(["/c", "start", "", url])
            .spawn()
    } else {
        std::process::Command::new("xdg-open").arg(url).spawn()
    };
    let _ = spawned;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Spec §12: the binary ships graph-ui/dist in memory — the keys must
    /// resolve under the `graph-ui/dist/` prefix, from embed (not disk).
    #[test]
    fn embedded_ui_serves_dist_from_memory() {
        let idx = EmbeddedUi::get("graph-ui/dist/index.html")
            .expect("embedded index.html missing");
        assert!(!idx.data.is_empty());
        let keys: Vec<String> = EmbeddedUi::iter().map(|k| k.into_owned()).collect();
        assert!(
            keys.iter().all(|k| k.starts_with("graph-ui/dist/")),
            "unexpected embed keys: {keys:?}"
        );
        assert!(keys.iter().any(|k| k.ends_with(".js")), "{keys:?}");
        assert!(keys.iter().any(|k| k.ends_with(".css")), "{keys:?}");
    }
}
