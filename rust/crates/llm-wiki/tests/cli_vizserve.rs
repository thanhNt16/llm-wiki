mod common;

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};

use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;
use serde_json::{json, Value};

/// Port-claim tests serialize: the OS hands out ephemeral ports
/// adjacently, so one test's `port..port+20` window can overlap another's
/// occupied ports — a genuine bind conflict, not a race the server
/// should (or does) survive.
static PORT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_llm-wiki"))
}

/// A free loopback port at call time (released immediately).
fn free_port() -> u16 {
    TcpListener::bind(("127.0.0.1", 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Minimal dist fixture — index.html + one hashed asset, like
/// skill/wiki-visualize/assets/graph-ui/dist.
fn ui_dist_fixture(dir: &std::path::Path) -> std::path::PathBuf {
    let dist = dir.join("dist");
    std::fs::create_dir_all(dist.join("assets")).unwrap();
    std::fs::write(dist.join("index.html"), "<html><body>viz</body></html>").unwrap();
    std::fs::write(dist.join("assets/app.js"), "console.log(1)").unwrap();
    dist
}

/// One claim + one source + one decision → entity/source/decision nodes
/// plus evidence/decision links (same shape as cli_graph_data::seed_wiki).
fn seed_wiki(root: &std::path::Path) {
    std::fs::write(
        root.join(".llm-wiki/claims/claim_c1.json"),
        serde_json::to_string(&json!({
            "id": "claim_c1",
            "subject": "t.api",
            "predicate": "status",
            "value": "stable",
            "status": "accepted",
            "version": 1,
            "evidence": [{"source_id": "source_s1", "version": 1}],
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::create_dir_all(root.join(".llm-wiki/sources/source_s1")).unwrap();
    std::fs::write(
        root.join(".llm-wiki/sources/source_s1/manifest.json"),
        serde_json::to_string(&json!({
            "id": "source_s1",
            "title": "Spec doc",
            "origin": "docs/spec.md",
        }))
        .unwrap(),
    )
    .unwrap();
    std::fs::write(
        root.join(".llm-wiki/decisions/decision_d1.json"),
        serde_json::to_string(&json!({
            "id": "decision_d1",
            "title": "Keep api",
            "status": "accepted",
            "decided_on": "2026-01-01",
            "claims": ["claim_c1"],
        }))
        .unwrap(),
    )
    .unwrap();
}

/// Spawn `viz-serve --no-open`; read the first stdout line
/// `{"serving": url, ...}` (timeout-guarded) and return (child, url).
fn spawn_serve(root: &std::path::Path, port: u16, ui_dist: &std::path::Path) -> (Child, String) {
    let mut child = bin()
        .args([
            "--root",
            root.to_str().unwrap(),
            "viz-serve",
            "--port",
            &port.to_string(),
            "--no-open",
            "--ui-dist",
            ui_dist.to_str().unwrap(),
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    let line = rx
        .recv_timeout(Duration::from_secs(30))
        .expect("viz-serve printed no banner within 30s");
    let v: Value = serde_json::from_str(line.trim())
        .unwrap_or_else(|e| panic!("banner not JSON ({e}): {line:?}"));
    let url = v["serving"].as_str().unwrap_or_else(|| {
        panic!("no serving url in banner: {v}")
    });
    (child, url.to_string())
}

fn http_get(url: &str) -> (u16, String, String) {
    match ureq::get(url).call() {
        Ok(mut resp) => {
            let status = resp.status().as_u16();
            let ctype = resp
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let body = resp.body_mut().read_to_string().unwrap();
            (status, ctype, body)
        }
        Err(ureq::Error::StatusCode(code)) => (code, String::new(), String::new()),
        Err(e) => panic!("GET {url} failed: {e}"),
    }
}

#[test]
fn viz_serve_serves_graph_data_and_ui() {
    let _port_lock = PORT_LOCK.lock().unwrap();
    let (dir, _wiki) = common::wiki();
    let root = dir.path();
    seed_wiki(root);
    let dist = ui_dist_fixture(root);
    let port = free_port();

    let (mut child, url) = spawn_serve(root, port, &dist);

    // graph-data: live payload, same shape `graph-data --full` emits.
    let (status, ctype, body) = http_get(&format!("{url}graph-data.json"));
    assert_eq!(status, 200);
    assert!(ctype.contains("application/json"), "ctype: {ctype}");
    let payload: Value = serde_json::from_str(&body).unwrap();
    let nodes = payload["nodes"].as_array().unwrap();
    let links = payload["links"].as_array().unwrap();
    // entity + source + decision nodes (no pages seeded)
    assert_eq!(nodes.len(), 3, "{nodes:?}");
    assert_eq!(payload["claim_count"], 1);
    assert_eq!(payload["source_count"], 1);
    assert_eq!(payload["decision_count"], 1);
    // entity→source evidence + decision→entity links
    assert_eq!(links.len(), 2, "{links:?}");
    // full mode: entity nodes embed claim dicts
    assert!(nodes[0]["claims"].is_array(), "full payload expected: {nodes:?}");

    // index at /
    let (status, ctype, body) = http_get(&url);
    assert_eq!(status, 200);
    assert!(ctype.contains("text/html"), "ctype: {ctype}");
    assert!(body.contains("viz"), "{body}");

    // asset under its relative path, correct Content-Type
    let (status, ctype, body) = http_get(&format!("{url}assets/app.js"));
    assert_eq!(status, 200);
    assert!(ctype.contains("text/javascript"), "ctype: {ctype}");
    assert!(body.contains("console.log"), "{body}");

    // unknown path → 404
    let (status, _, _) = http_get(&format!("{url}nope.txt"));
    assert_eq!(status, 404);

    // traversal → 403
    let (status, _, _) = http_get(&format!("{url}../etc/passwd"));
    assert!(status == 403 || status == 404, "status: {status}");

    // SSE /events: hello frame carries the node count, then the stream
    // stays open (chunked, no Content-Length).
    let addr = url.trim_start_matches("http://").trim_end_matches('/');
    let mut sse = TcpStream::connect(addr).unwrap();
    sse.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    write!(sse, "GET /events HTTP/1.1\r\nHost: {addr}\r\n\r\n").unwrap();
    let mut buf = Vec::new();
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    while !buf.windows(b"event: hello".len()).any(|w| w == b"event: hello")
        && std::time::Instant::now() < deadline
    {
        let mut chunk = [0u8; 512];
        let n = sse.read(&mut chunk).unwrap();
        assert!(n > 0, "SSE stream closed before hello");
        buf.extend_from_slice(&chunk[..n]);
    }
    let text = String::from_utf8_lossy(&buf);
    assert!(text.contains("200"), "SSE headers: {text}");
    assert!(text.contains("text/event-stream"), "SSE headers: {text}");
    assert!(text.contains("event: hello"), "{text}");
    assert!(text.contains("\"nodes\": 3"), "{text}");

    child.kill().unwrap();
}

#[test]
fn viz_serve_port_scan_skips_busy_port() {
    let _port_lock = PORT_LOCK.lock().unwrap();
    let (dir, _wiki) = common::wiki();
    let root = dir.path();
    seed_wiki(root);
    let dist = ui_dist_fixture(root);
    let port = free_port();
    // occupy the first candidate — the server must bind the next one
    let _blocker = TcpListener::bind(("127.0.0.1", port)).unwrap();

    let (mut child, url) = spawn_serve(root, port, &dist);
    let bound: u16 = url
        .trim_end_matches('/')
        .rsplit(':')
        .next()
        .unwrap()
        .parse()
        .unwrap();
    assert!(bound > port, "expected port > {port}, got {bound} ({url})");
    assert!(bound <= port + 20, "bound {bound} outside scan window");

    let (status, _, _) = http_get(&url);
    assert_eq!(status, 200);
    child.kill().unwrap();
}

#[test]
fn viz_serve_errors_when_all_ports_busy() {
    let _port_lock = PORT_LOCK.lock().unwrap();
    let (dir, _wiki) = common::wiki();
    let root = dir.path();
    seed_wiki(root);
    let dist = ui_dist_fixture(root);
    let port = free_port();
    // occupy the whole port..=port+20 window (skip candidates that fail)
    let mut blockers = Vec::new();
    for p in port..=port + 20 {
        if let Ok(l) = TcpListener::bind(("127.0.0.1", p)) {
            blockers.push(l);
        }
    }

    let out = bin()
        .args([
            "--root",
            root.to_str().unwrap(),
            "viz-serve",
            "--port",
            &port.to_string(),
            "--no-open",
            "--ui-dist",
            dist.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    let code = out.status.code().unwrap_or(-1);
    assert_ne!(code, 0, "expected non-zero exit: {}", String::from_utf8_lossy(&out.stdout));
    let v: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout not JSON ({e}): {}", String::from_utf8_lossy(&out.stdout)));
    assert!(
        v["error"].as_str().unwrap_or("").contains("no free loopback port"),
        "{v}"
    );
}
