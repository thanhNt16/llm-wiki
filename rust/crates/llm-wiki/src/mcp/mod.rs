//! MCP stdio server — spec §7.
//!
//! JSON-RPC 2.0 over newline-delimited stdio, hand-rolled (no SDK).
//! Transport rules (§7.1): one compact JSON object per line on stdout;
//! logs go to stderr; requests before `notifications/initialized` are
//! tolerated; notifications never get a response; stdin EOF shuts the
//! server down with `exit(0)` — stdio has no shutdown method.
//!
//! Session state (§7.3): the wiki root binds through
//! `--root` flag > `LLM_WIKI_ROOT` > cwd-ancestor walk > client
//! `roots/list` (capability-gated, 30 s timeout, `file:`-only, cached
//! until `notifications/roots/list_changed` or `wiki_open`) > unbound.
//! Canonical state is never cached across calls — every tool call opens
//! a fresh `Wiki` and every resource read re-scans canonical files.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError};
use std::time::Duration;

use serde_json::{json, Value};

pub mod resources;
pub mod tools;

use crate::err::EngineError;
use crate::store::Wiki;

/// Protocol versions the server can speak (§7.2), newest first.
const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// `roots/list` roundtrip cap (§7.3).
const ROOTS_TIMEOUT: Duration = Duration::from_secs(30);

// JSON-RPC error codes (§7.1).
pub(crate) const PARSE_ERROR: i64 = -32700;
pub(crate) const INVALID_REQUEST: i64 = -32600;
pub(crate) const METHOD_NOT_FOUND: i64 = -32601;
pub(crate) const INVALID_PARAMS: i64 = -32602;
pub(crate) const INTERNAL_ERROR: i64 = -32603;

/// The message "no wiki open" wording is contractual — the same clause
/// appears in `instructions` (§7.7).
pub(crate) const NO_WIKI_MSG: &str = "no wiki open — call wiki_open";

pub(crate) struct RpcError {
    pub code: i64,
    pub message: String,
}

impl RpcError {
    pub fn new(code: i64, message: impl Into<String>) -> Self {
        RpcError {
            code,
            message: message.into(),
        }
    }
    pub fn invalid_params(message: impl Into<String>) -> Self {
        Self::new(INVALID_PARAMS, message)
    }
    pub fn internal(message: impl Into<String>) -> Self {
        Self::new(INTERNAL_ERROR, message)
    }
}

/// `serve` is the `llm-wiki mcp` entry point: it runs until stdin EOF
/// and then exits the process — the function diverges.
pub fn serve(flag_root: Option<PathBuf>) -> ! {
    set_binary_stdio();
    let stdin = std::io::stdin();
    let stdout = std::io::stdout();
    // A reader thread isolates line-reading so `roots/list` can bound its
    // wait on the channel with a 30 s timeout (§7.3). The thread ends on
    // stdin EOF, disconnecting the channel.
    let (tx, rx) = channel::<String>();
    std::thread::spawn(move || {
        let mut reader = BufReader::new(stdin.lock());
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {
                    if tx.send(line.clone()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let mut server = Server {
        flag_root,
        bound: None,
        bound_source: None,
        protocol: PROTOCOL_VERSIONS[0].to_string(),
        client_roots_cap: false,
        roots_tried: false,
        roots_in_flight: false,
        client_root_files: Vec::new(),
        next_id: 1,
        out: Box::new(stdout),
        rx,
    };
    server.run()
}

pub(crate) struct Server {
    flag_root: Option<PathBuf>,
    bound: Option<PathBuf>,
    /// §7.5 wiki_open `source` tag: arg|env|cwd-walk|roots|explicit.
    bound_source: Option<&'static str>,
    protocol: String,
    client_roots_cap: bool,
    /// Roots rung already attempted (cached-root semantics, §7.3).
    roots_tried: bool,
    /// A `roots/list` request is awaiting its response; nested calls do
    /// not re-enter the wait (single-flight).
    roots_in_flight: bool,
    client_root_files: Vec<PathBuf>,
    next_id: u64,
    out: Box<dyn Write>,
    rx: Receiver<String>,
}

impl Server {
    fn run(&mut self) -> ! {
        loop {
            match self.rx.recv() {
                Err(_) => std::process::exit(0), // stdin EOF (§7.1)
                Ok(line) => {
                    let line = line.trim_end_matches(['\n', '\r']);
                    if line.trim().is_empty() {
                        continue;
                    }
                    self.handle_frame(line);
                }
            }
        }
    }

    fn send_line(&mut self, v: &Value) {
        let mut s = serde_json::to_string(v).unwrap_or_else(|_| "{}".to_string());
        s.push('\n');
        if self.out.write_all(s.as_bytes()).is_err() || self.out.flush().is_err() {
            // The client is gone; nothing useful remains.
            std::process::exit(0);
        }
    }

    fn reply_result(&mut self, id: &Value, result: Value) {
        self.send_line(&json!({"jsonrpc": "2.0", "id": id, "result": result}));
    }

    fn reply_error(&mut self, id: &Value, code: i64, message: impl Into<String>) {
        self.send_line(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": code, "message": message.into()},
        }));
    }

    fn handle_frame(&mut self, line: &str) {
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                self.reply_error(&Value::Null, PARSE_ERROR, "parse error");
                return;
            }
        };
        let Some(obj) = v.as_object() else {
            self.reply_error(&Value::Null, INVALID_REQUEST, "request must be an object");
            return;
        };
        let id = obj.get("id").cloned().unwrap_or(Value::Null);
        let is_notification = !obj.contains_key("id") || id.is_null();
        if is_notification {
            // Never respond to notifications (§7.1).
            if let Some(method) = obj.get("method").and_then(Value::as_str) {
                self.handle_notification(method);
            }
            return;
        }
        // A response-shaped message (id + result/error, no method) isn't
        // ours — ignore it (§7.3 demux tolerates stray replies).
        if obj.get("method").is_none()
            && (obj.contains_key("result") || obj.contains_key("error"))
        {
            return;
        }
        let Some(method) = obj.get("method").and_then(Value::as_str) else {
            self.reply_error(&id, INVALID_REQUEST, "missing method");
            return;
        };
        let params = obj.get("params").cloned().unwrap_or(Value::Null);
        match self.dispatch(method, &params) {
            Ok(result) => self.reply_result(&id, result),
            Err(e) => self.reply_error(&id, e.code, e.message),
        }
    }

    fn handle_notification(&mut self, method: &str) {
        match method {
            "notifications/roots/list_changed" => {
                // Client roots may have changed: re-run the rung on the
                // next unbound call (§7.3 cache reset).
                self.roots_tried = false;
                self.client_root_files.clear();
            }
            // initialized / cancelled / anything else: tolerated, ignored.
            _ => {}
        }
    }

    fn dispatch(&mut self, method: &str, params: &Value) -> Result<Value, RpcError> {
        match method {
            "initialize" => self.initialize(params),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(tools::list(self.protocol_supported())),
            "tools/call" => tools::call(self, params),
            "resources/list" => Ok(resources::list()),
            "resources/templates/list" => Ok(resources::templates()),
            "resources/read" => resources::read(self, params),
            "completion/complete" => resources::complete(self, params),
            _ => Err(RpcError::new(
                METHOD_NOT_FOUND,
                format!("method not found: {method}"),
            )),
        }
    }

    /// §7.2: echo the client's version when supported, else the newest.
    /// `structuredContent`/`outputSchema` ride on the negotiated version.
    pub(crate) fn protocol_supported(&self) -> bool {
        self.protocol.as_str() >= "2025-06-18"
    }

    fn initialize(&mut self, params: &Value) -> Result<Value, RpcError> {
        let requested = params["protocolVersion"].as_str().unwrap_or("");
        self.protocol = if PROTOCOL_VERSIONS.contains(&requested) {
            requested.to_string()
        } else {
            PROTOCOL_VERSIONS[0].to_string()
        };
        self.client_roots_cap = params["capabilities"]["roots"].is_object();
        // Bind the flag/env/cwd rungs now — the roots rung waits for the
        // first bound call (the initialize response must precede any
        // server→client request).
        if self.bound.is_none() {
            self.auto_bind(false);
        }
        Ok(json!({
            "protocolVersion": self.protocol,
            "capabilities": {"tools": {}, "resources": {}, "completions": {}},
            "serverInfo": {"name": "llm-wiki", "version": env!("CARGO_PKG_VERSION")},
            "instructions": self.instructions(),
        }))
    }

    /// §7.7 instructions: layout orientation + answer contract + the
    /// discovery clause, plus a ≤4096-byte CONVENTIONS.md excerpt.
    fn instructions(&self) -> String {
        let mut s = String::from(
            "llm-wiki: evidence-backed project memory. Canonical data lives \
             under .llm-wiki/ (sources/, claims/, decisions/, topics.json); \
             wiki/ pages are derived and rebuildable. Answer contract: cite \
             claim ids (claim_*) and surface conflicts/gaps from query \
             payloads rather than flattening them. If a wiki tool reports \
             \"no wiki open — call wiki_open\", bind the session root with \
             the wiki_open tool (or wiki_init for a fresh project) and retry. \
             Every tool also accepts a per-call `root` override.",
        );
        if let Some(root) = self.candidate_root() {
            let conv = root.join(".llm-wiki/CONVENTIONS.md");
            if let Ok(text) = std::fs::read_to_string(&conv) {
                // §7.7 bound is BYTES — take chars until the byte cap so
                // multibyte text can't overflow or split a codepoint.
                let mut excerpt = String::new();
                for ch in text.chars() {
                    if excerpt.len() + ch.len_utf8() > 4096 {
                        break;
                    }
                    excerpt.push(ch);
                }
                s.push_str("\n\nProject conventions (.llm-wiki/CONVENTIONS.md):\n");
                s.push_str(&excerpt);
            }
        }
        s
    }

    /// Non-binding candidate for instructions only (no roots/list).
    fn candidate_root(&self) -> Option<PathBuf> {
        if let Some(r) = &self.bound {
            return Some(r.clone());
        }
        if let Some(r) = &self.flag_root {
            return Some(r.clone());
        }
        if let Ok(p) = std::env::var("LLM_WIKI_ROOT") {
            if !p.is_empty() {
                return Some(PathBuf::from(p));
            }
        }
        cwd_walk()
    }

    /// §7.3 resolution chain. With `include_roots` the client `roots/list`
    /// rung is attempted once (cached afterwards).
    pub(crate) fn auto_bind(&mut self, include_roots: bool) {
        if self.bound.is_some() {
            return;
        }
        if let Some(r) = self.flag_root.clone() {
            self.bound = Some(r);
            self.bound_source = Some("arg");
            return;
        }
        if let Ok(p) = std::env::var("LLM_WIKI_ROOT") {
            if !p.is_empty() {
                self.bound = Some(PathBuf::from(p));
                self.bound_source = Some("env");
                return;
            }
        }
        if let Some(r) = cwd_walk() {
            self.bound = Some(r);
            self.bound_source = Some("cwd-walk");
            return;
        }
        if include_roots {
            if let Some(r) = self.roots_rung() {
                self.bound = Some(r);
                self.bound_source = Some("roots");
            }
        }
    }

    /// Client `roots/list` rung — capability-gated, single-flight, cached.
    /// Returns the chosen root: the first client root containing
    /// `.llm-wiki/`, else the first `file:` root at all.
    fn roots_rung(&mut self) -> Option<PathBuf> {
        if self.roots_tried {
            return None;
        }
        self.roots_tried = true;
        if self.client_roots_cap {
            self.query_client_roots();
        }
        self.client_root_files
            .iter()
            .find(|p| p.join(".llm-wiki").is_dir())
            .cloned()
            .or_else(|| self.client_root_files.first().cloned())
    }

    /// Server→client `roots/list` with demux: the response is correlated
    /// by id while interleaved requests/notifications dispatch normally
    /// (§7.3). Timeout / -32601 / any error ⇒ "no roots".
    fn query_client_roots(&mut self) {
        if self.roots_in_flight {
            return; // single-flight: nested calls do not re-enter
        }
        self.roots_in_flight = true;
        let id = json!(format!("roots-{}", self.next_id));
        self.next_id += 1;
        self.send_line(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "roots/list",
            "params": {},
        }));
        let deadline = std::time::Instant::now() + ROOTS_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                break;
            }
            let line = match self.rx.recv_timeout(remaining) {
                Ok(l) => l,
                Err(RecvTimeoutError::Timeout) => break, // ⇒ "no roots" (§7.3)
                Err(RecvTimeoutError::Disconnected) => std::process::exit(0), // stdin EOF
            };
            let line = line.trim_end_matches(['\n', '\r']);
            if line.trim().is_empty() {
                continue;
            }
            let v: Value = match serde_json::from_str(line) {
                Ok(v) => v,
                Err(_) => {
                    self.reply_error(&Value::Null, PARSE_ERROR, "parse error");
                    continue;
                }
            };
            let obj = match v.as_object() {
                Some(o) => o,
                None => {
                    self.reply_error(&Value::Null, INVALID_REQUEST, "request must be an object");
                    continue;
                }
            };
            let is_response = obj.contains_key("id")
                && !obj["id"].is_null()
                && (obj.contains_key("result") || obj.contains_key("error"))
                && obj.get("method").is_none();
            if is_response && v["id"] == id {
                if let Some(roots) = v["result"]["roots"].as_array() {
                    for r in roots {
                        if let Some(p) = r["uri"].as_str().and_then(file_uri_path) {
                            self.client_root_files.push(p);
                        }
                    }
                }
                break;
            }
            if is_response {
                // A response to some other server→client request (none in
                // v1): drop it, keep waiting for ours.
                continue;
            }
            // Interleaved request/notification: demux — handle it now.
            let mid = obj.get("id").cloned().unwrap_or(Value::Null);
            if !obj.contains_key("id") || mid.is_null() {
                if let Some(m) = obj.get("method").and_then(Value::as_str) {
                    self.handle_notification(m);
                }
                continue;
            }
            match obj.get("method").and_then(Value::as_str) {
                Some(m) => {
                    let params = obj.get("params").cloned().unwrap_or(Value::Null);
                    match self.dispatch(m, &params) {
                        Ok(result) => self.reply_result(&mid, result),
                        Err(e) => self.reply_error(&mid, e.code, e.message),
                    }
                }
                None => self.reply_error(&mid, INVALID_REQUEST, "missing method"),
            }
        }
        self.roots_in_flight = false;
    }

    /// §7.3 path semantics: absolute or server-cwd-relative; `~` is not
    /// expanded (shell expansion does not apply over MCP).
    pub(crate) fn resolve_path_arg(&self, s: &str) -> PathBuf {
        let p = PathBuf::from(s);
        if p.is_absolute() {
            p
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(p)
        }
    }

    /// Fresh `Wiki` handle per call (§7.3: no cross-call canonical
    /// caching; `Wiki::open` performs no state reads itself).
    pub(crate) fn open_wiki_at(&self, root: &Path) -> Result<Wiki, EngineError> {
        Wiki::open(root)
    }

    /// §7.4 tool envelope: the CLI payload serializes into
    /// `content[0].text`; `structuredContent` mirrors it on
    /// ≥2025-06-18 clients. Domain errors set `isError:true` with the
    /// error/exit_code/error_type payload preserved.
    pub(crate) fn tool_ok(&self, payload: Value) -> Value {
        let mut r = json!({
            "content": [{"type": "text", "text": payload.to_string()}],
            "isError": false,
        });
        if self.protocol_supported() {
            r["structuredContent"] = payload;
        }
        r
    }

    pub(crate) fn tool_err(&self, payload: Value) -> Value {
        let mut r = json!({
            "content": [{"type": "text", "text": payload.to_string()}],
            "isError": true,
        });
        if self.protocol_supported() {
            r["structuredContent"] = payload;
        }
        r
    }

    pub(crate) fn domain_err(&self, e: &EngineError) -> Value {
        self.tool_err(json!({
            "error": e.to_string(),
            "exit_code": e.exit_code(),
            "error_type": e.error_type(),
        }))
    }

    /// The unbound-state error payload every bound tool shares (§7.3).
    pub(crate) fn no_wiki_err(&self) -> Value {
        self.tool_err(json!({
            "error": NO_WIKI_MSG,
            "exit_code": 2,
            "error_type": "usage",
        }))
    }
}

/// cwd-ancestor walk for `.llm-wiki/` (§7.3).
fn cwd_walk() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        if dir.join(".llm-wiki").is_dir() {
            return Some(dir);
        }
        if !dir.pop() {
            return None;
        }
    }
}

/// `file:` URI → host path; non-`file:` URIs are ignored (§7.3).
fn file_uri_path(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // Strip authority (usually empty or localhost).
    let path = match rest.strip_prefix("localhost") {
        Some(p) => p,
        None => rest,
    };
    let path = path.strip_prefix('/').map(|p| format!("/{p}"))?;
    percent_decode(&path).map(PathBuf::from)
}

/// Minimal percent-decoding for `file:` URIs (spaces etc.).
fn percent_decode(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return None;
            }
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            let b = u8::from_str_radix(hex, 16).ok()?;
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).ok()
}

/// §7.1: on Windows, stdin/stdout must be binary so `\r\n` translation
/// cannot corrupt the newline framing. No-op elsewhere.
fn set_binary_stdio() {
    #[cfg(windows)]
    unsafe {
        extern "C" {
            fn _setmode(fd: i32, mode: i32) -> i32;
        }
        const O_BINARY: i32 = 0x8000;
        _setmode(0, O_BINARY);
        _setmode(1, O_BINARY);
    }
}

#[cfg(test)]
mod tests {
    //! Wire-level tests: frame parse, error codes, notification silence,
    //! version negotiation, envelope shape and the roots/list demux —
    //! everything below the binary's stdin/stdout boundary.
    //!
    //! Hermeticity: every server constructed here pins `flag_root` to a
    //! tempdir so the cwd-ancestor walk (which can reach a real
    //! `.llm-wiki` on this machine) and `LLM_WIKI_ROOT` never leak in.
    use std::io::Write;
    use std::path::Path;
    use std::sync::mpsc::{channel, Sender};
    use std::sync::{Arc, Mutex};

    use serde_json::{json, Value};

    use super::*;

    /// Shared capture buffer standing in for stdout.
    #[derive(Clone)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);

    impl Write for SharedBuf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// A server over a fake stdin channel; `root` may be a nonexistent
    /// path (bound-but-uninitialized is a domain result, not an error).
    fn test_server(root: &Path) -> (Server, Sender<String>, SharedBuf) {
        let (tx, rx) = channel::<String>();
        let buf = SharedBuf(Arc::new(Mutex::new(Vec::new())));
        let srv = Server {
            flag_root: Some(root.to_path_buf()),
            bound: None,
            bound_source: None,
            protocol: PROTOCOL_VERSIONS[0].to_string(),
            client_roots_cap: false,
            roots_tried: false,
            roots_in_flight: false,
            client_root_files: Vec::new(),
            next_id: 1,
            out: Box::new(buf.clone()),
            rx,
        };
        (srv, tx, buf)
    }

    fn written(buf: &SharedBuf) -> Vec<Value> {
        let bytes = buf.0.lock().unwrap().clone();
        let text = String::from_utf8(bytes).unwrap();
        text.lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn frame(method: &str, params: Value, id: u64) -> String {
        json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string()
    }

    #[test]
    fn frames_are_single_lines_of_json() {
        let dir = tempfile::tempdir().unwrap();
        let (mut srv, _tx, buf) = test_server(dir.path());
        srv.reply_result(&json!(1), json!({"a": [1, 2, 3], "b": "x\ty"}));
        let out = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
        // Exactly one newline-terminated line, valid JSON, no literal \n
        // inside the frame (§7.1).
        assert!(out.ends_with('\n'));
        assert_eq!(out.trim_end().lines().count(), 1);
        assert_eq!(serde_json::from_str::<Value>(out.trim_end()).unwrap()["result"]["a"],
                   json!([1, 2, 3]));
    }

    #[test]
    fn parse_error_yields_32700_with_null_id() {
        let dir = tempfile::tempdir().unwrap();
        let (mut srv, _tx, buf) = test_server(dir.path());
        srv.handle_frame("this is not json");
        let msgs = written(&buf);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["id"], Value::Null);
        assert_eq!(msgs[0]["error"]["code"], PARSE_ERROR);
    }

    #[test]
    fn notifications_never_write() {
        let dir = tempfile::tempdir().unwrap();
        let (mut srv, _tx, buf) = test_server(dir.path());
        for f in [
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "method": "notifications/cancelled",
                   "params": {"requestId": 3}}),
            json!({"jsonrpc": "2.0", "method": "bogus/notification"}),
            // A request-shaped message with `id: null` is still a
            // notification — no response either way (§7.1).
            json!({"jsonrpc": "2.0", "id": null, "method": "ping"}),
        ] {
            srv.handle_frame(&f.to_string());
        }
        assert!(buf.0.lock().unwrap().is_empty(), "notifications wrote output");
    }

    #[test]
    fn unknown_method_is_32601_and_missing_method_32600() {
        let dir = tempfile::tempdir().unwrap();
        let (mut srv, _tx, buf) = test_server(dir.path());
        srv.handle_frame(&frame("nope/method", json!({}), 1));
        srv.handle_frame(&json!({"jsonrpc": "2.0", "id": 2}).to_string());
        let msgs = written(&buf);
        assert_eq!(msgs[0]["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(msgs[0]["id"], 2 - 1); // id 1
        assert_eq!(msgs[1]["error"]["code"], INVALID_REQUEST);
        assert_eq!(msgs[1]["id"], 2);
    }

    #[test]
    fn initialize_negotiates_and_older_version_drops_structured() {
        let dir = tempfile::tempdir().unwrap();
        let (mut srv, _tx, buf) = test_server(dir.path());
        // Unknown client version → newest supported.
        srv.handle_frame(&frame(
            "initialize",
            json!({"protocolVersion": "1999-01-01", "capabilities": {}}),
            1,
        ));
        let msgs = written(&buf);
        let r = &msgs[0]["result"];
        assert_eq!(r["protocolVersion"], "2025-06-18");
        assert_eq!(r["serverInfo"]["name"], "llm-wiki");
        assert!(r["capabilities"]["tools"].is_object());
        assert!(r["capabilities"]["resources"].is_object());
        assert!(r["capabilities"]["completions"].is_object());
        assert!(r["instructions"].as_str().unwrap().contains("wiki_open"));
        assert!(srv.protocol_supported());

        // A second initialize on an allowlisted version echoes it back
        // and disables structuredContent.
        srv.handle_frame(&frame(
            "initialize",
            json!({"protocolVersion": "2025-03-26", "capabilities": {}}),
            2,
        ));
        let msgs = written(&buf);
        assert_eq!(msgs[1]["result"]["protocolVersion"], "2025-03-26");
        assert!(!srv.protocol_supported());
        let env = srv.tool_ok(json!({"x": 1}));
        assert!(env.get("structuredContent").is_none());
    }

    #[test]
    fn tool_envelope_preserves_domain_error_fields() {
        let dir = tempfile::tempdir().unwrap();
        let (srv, _tx, _buf) = test_server(dir.path());
        let ok = srv.tool_ok(json!({"initialized": true}));
        assert_eq!(ok["isError"], false);
        assert_eq!(ok["content"][0]["type"], "text");
        assert_eq!(ok["structuredContent"]["initialized"], true);
        let e = srv.domain_err(&EngineError::Conflict("stamp moved".into()));
        assert_eq!(e["isError"], true);
        let payload: Value =
            serde_json::from_str(e["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(payload["exit_code"], 4);
        assert_eq!(payload["error_type"], "conflict");
        // The unbound clause is contractual (§7.3/§7.7).
        let n = srv.no_wiki_err();
        let payload: Value =
            serde_json::from_str(n["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(payload["error"], NO_WIKI_MSG);
    }

    #[test]
    fn tools_list_and_call_over_frames() {
        let dir = tempfile::tempdir().unwrap();
        init_scaffold(dir.path());
        let (mut srv, _tx, buf) = test_server(dir.path());
        srv.handle_frame(&frame("tools/list", json!({}), 1));
        srv.handle_frame(&frame(
            "tools/call",
            json!({"name": "wiki_status", "arguments": {}}),
            2,
        ));
        srv.handle_frame(&frame(
            "tools/call",
            json!({"name": "wiki_bogus", "arguments": {}}),
            3,
        ));
        let msgs = written(&buf);
        assert_eq!(msgs[0]["result"]["tools"].as_array().unwrap().len(), 13);
        // flag_root bound at initialize-free state: status runs.
        let status_payload: Value = serde_json::from_str(
            msgs[1]["result"]["content"][0]["text"].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(status_payload["initialized"], true);
        // Unknown tool → isError:true, never an rpc error (§7.1).
        assert!(msgs[2].get("error").is_none());
        assert_eq!(msgs[2]["result"]["isError"], true);
    }

    #[test]
    fn wiki_open_and_init_bind_the_session() {
        let dir = tempfile::tempdir().unwrap();
        let (mut srv, _tx, buf) = test_server(dir.path());
        srv.handle_frame(&frame(
            "tools/call",
            json!({"name": "wiki_init", "arguments": {"name": "proj"}}),
            1,
        ));
        srv.handle_frame(&frame(
            "tools/call",
            json!({"name": "wiki_status", "arguments": {}}),
            2,
        ));
        srv.handle_frame(&frame(
            "tools/call",
            json!({"name": "wiki_open", "arguments": {}}),
            3,
        ));
        let msgs = written(&buf);
        let init_p: Value = serde_json::from_str(
            msgs[0]["result"]["content"][0]["text"].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(init_p["initialized"], true);
        assert_eq!(msgs[1]["result"]["isError"], false);
        let open_p: Value = serde_json::from_str(
            msgs[2]["result"]["content"][0]["text"].as_str().unwrap(),
        )
        .unwrap();
        assert_eq!(open_p["initialized"], true);
        assert_eq!(open_p["source"], "arg"); // bound via the --root flag rung
    }

    #[test]
    fn roots_list_demux_handles_interleaved_requests() {
        let dir = tempfile::tempdir().unwrap();
        init_scaffold(dir.path());
        // Unbound session with a roots-capable client: the first bound
        // call must emit roots/list, demux a mid-flight ping, and bind
        // from the file: root that answers it.
        let (mut srv, tx, buf) = test_server(dir.path());
        srv.flag_root = None;
        srv.client_roots_cap = true;
        // Drive the roots wait directly: queue the client's interleaved
        // ping + roots response, then assert outbound ordering and the
        // file:-URI filter.
        tx.send(json!({"jsonrpc": "2.0", "id": 77, "method": "ping"}).to_string() + "\n")
            .unwrap();
        tx.send(
            json!({
                "jsonrpc": "2.0", "id": "roots-1",
                "result": {"roots": [
                    {"uri": "vscode://skip/me"},
                    {"uri": format!("file://{}", dir.path().display())},
                ]}
            })
            .to_string()
                + "\n",
        )
        .unwrap();
        srv.query_client_roots();
        let msgs = written(&buf);
        // Outbound order: roots/list request, ping response — the ping
        // was demuxed, not mistaken for the roots reply.
        assert_eq!(msgs[0]["method"], "roots/list");
        assert_eq!(msgs[0]["id"], "roots-1");
        assert_eq!(msgs[1]["id"], 77);
        assert_eq!(msgs[1]["result"], json!({}));
        // The vscode: URI was filtered; the file: root was kept.
        assert_eq!(srv.client_root_files, vec![dir.path().to_path_buf()]);
    }

    #[test]
    fn roots_timeout_and_error_responses_yield_no_roots() {
        let dir = tempfile::tempdir().unwrap();
        let (mut srv, tx, buf) = test_server(dir.path());
        srv.flag_root = None;
        // Client answers with a JSON-RPC error → treated as "no roots".
        tx.send(
            json!({"jsonrpc": "2.0", "id": "roots-1",
                   "error": {"code": -32601, "message": "unsupported"}})
                .to_string()
                + "\n",
        )
        .unwrap();
        srv.query_client_roots();
        assert!(srv.client_root_files.is_empty());
        assert_eq!(written(&buf)[0]["method"], "roots/list");
    }

    /// Minimal `.llm-wiki` scaffold for bound-tool unit tests — real
    /// `init_wiki` rather than a fixture string.
    fn init_scaffold(root: &Path) {
        let wiki = Wiki::open(root).unwrap();
        crate::store::init_wiki(&wiki, "t").unwrap();
    }
}
