//! Integration tests for `llm-wiki mcp` (spec §7) — spawn the binary
//! and speak newline-delimited JSON-RPC 2.0 over stdio.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_llm-wiki"))
}

struct Mcp {
    child: Child,
    stdin: Option<ChildStdin>,
    stdout: BufReader<ChildStdout>,
    next: u64,
}

impl Mcp {
    fn spawn(args: &[&str], cwd: &Path) -> Mcp {
        let mut child = bin()
            .args(args)
            .arg("mcp")
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Mcp {
            child,
            stdin: Some(stdin),
            stdout,
            next: 1,
        }
    }

    fn send(&mut self, v: &Value) {
        let mut s = serde_json::to_string(v).unwrap();
        s.push('\n');
        self.stdin.as_mut().unwrap().write_all(s.as_bytes()).unwrap();
        self.stdin.as_mut().unwrap().flush().unwrap();
    }

    /// A literal line — for parse-error coverage.
    fn send_raw(&mut self, line: &str) {
        self.stdin.as_mut().unwrap().write_all(line.as_bytes()).unwrap();
        self.stdin.as_mut().unwrap().write_all(b"\n").unwrap();
        self.stdin.as_mut().unwrap().flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        let got = self.read_line();
        assert_eq!(got["id"], id, "response id mismatch: {got}");
        got
    }

    fn notify(&mut self, method: &str) {
        self.send(&json!({"jsonrpc": "2.0", "method": method}));
    }

    /// One stdout line → JSON (blocks until the server writes one).
    fn read_line(&mut self) -> Value {
        let mut line = String::new();
        let n = self.stdout.read_line(&mut line).unwrap();
        assert!(n > 0, "server closed stdout unexpectedly");
        serde_json::from_str(&line)
            .unwrap_or_else(|e| panic!("bad JSON frame ({e}): {line}"))
    }

    /// Result payload of a `tools/call`; asserts no JSON-RPC error.
    fn tool(&mut self, name: &str, arguments: Value) -> Value {
        let r = self.request(
            "tools/call",
            json!({"name": name, "arguments": arguments}),
        );
        assert!(r.get("error").is_none(), "tools/call rpc error: {r}");
        r["result"].clone()
    }

    /// The parsed CLI payload inside content[0].text.
    fn payload(&mut self, name: &str, arguments: Value) -> Value {
        let r = self.tool(name, arguments);
        let text = r["content"][0]["text"].as_str().unwrap();
        serde_json::from_str(text).unwrap()
    }

    fn initialize(&mut self, version: &str) -> Value {
        self.request(
            "initialize",
            json!({
                "protocolVersion": version,
                "capabilities": {},
                "clientInfo": {"name": "test", "version": "0"},
            }),
        )
    }
}

impl Drop for Mcp {
    fn drop(&mut self) {
        // stdin drop = EOF → server exits 0 on its own.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn init_wiki(dir: &Path) {
    let out = bin().arg("init").arg(dir).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "init: {:?}", out);
}

fn write_claim(root: &Path, claim: Value) {
    let id = claim["id"].as_str().unwrap();
    std::fs::write(
        root.join(format!(".llm-wiki/claims/{id}.json")),
        serde_json::to_string(&claim).unwrap(),
    )
    .unwrap();
}

fn claim(id: &str, subject: &str) -> Value {
    json!({
        "id": id,
        "subject": subject,
        "predicate": "is tested by",
        "value": "cli_mcp",
        "status": "current",
        "version": 1,
        "recorded_at": "2026-01-15T00:00:00Z",
        "evidence": [{"source_id": "source_s1", "version": 1}],
        "authority": {"type": "authoritative_source"},
        "valid_from": "2025-06-01",
    })
}

// ------------------------------------------------------------ handshake

#[test]
fn initialize_negotiates_supported_version() {
    let dir = tempfile::tempdir().unwrap();
    let mut mcp = Mcp::spawn(&[], dir.path());
    let r = mcp.initialize("2024-11-05");
    assert_eq!(r["result"]["protocolVersion"], "2024-11-05");
    assert_eq!(r["result"]["serverInfo"]["name"], "llm-wiki");
    assert!(r["result"]["capabilities"]["tools"].is_object());
    assert!(r["result"]["capabilities"]["resources"].is_object());
    assert!(r["result"]["capabilities"]["completions"].is_object());
    assert!(r["result"]["instructions"]
        .as_str()
        .unwrap()
        .contains("wiki_open"));
    // Older negotiated version: no outputSchema on tools/list.
    let t = mcp.request("tools/list", json!({}));
    assert!(t["result"]["tools"][0].get("outputSchema").is_none());
}

#[test]
fn initialize_falls_back_to_newest_for_unknown_version() {
    let dir = tempfile::tempdir().unwrap();
    let mut mcp = Mcp::spawn(&[], dir.path());
    let r = mcp.initialize("1999-01-01");
    assert_eq!(r["result"]["protocolVersion"], "2025-06-18");
    // ≥2025-06-18: outputSchema is emitted per tool.
    let t = mcp.request("tools/list", json!({}));
    assert_eq!(t["result"]["tools"][0]["outputSchema"], json!({"type": "object"}));
}

// ------------------------------------------------------------ tools/list

#[test]
fn tools_list_has_thirteen_with_annotations() {
    let dir = tempfile::tempdir().unwrap();
    let mut mcp = Mcp::spawn(&[], dir.path());
    mcp.initialize("2025-06-18");
    let r = mcp.request("tools/list", json!({}));
    let tools = r["result"]["tools"].as_array().unwrap();
    assert_eq!(tools.len(), 13, "{tools:?}");
    for name in [
        "wiki_status", "wiki_query", "wiki_search_claims", "wiki_read_page",
        "wiki_doctor", "wiki_review", "wiki_open", "wiki_init", "wiki_ingest",
        "wiki_compile", "wiki_reconcile", "wiki_write_page", "wiki_decide",
    ] {
        let t = tools.iter().find(|t| t["name"] == name).unwrap();
        assert!(t["inputSchema"].is_object(), "{name}");
        assert!(t["annotations"].is_object(), "{name}");
    }
    assert_eq!(tools[0]["annotations"]["readOnlyHint"], true);
    // Every tool exposes the shared per-call `root` override.
    for t in tools {
        assert!(t["inputSchema"]["properties"]["root"].is_object(), "{}", t["name"]);
    }
}

// ------------------------------------------------------------ tool calls

#[test]
fn wiki_status_and_query_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    init_wiki(dir.path());
    write_claim(dir.path(), claim("claim_01ARZ3NDEKTSV4RRFFQ69G5FAV", "mcp-server"));
    let mut mcp = Mcp::spawn(&["--root", dir.path().to_str().unwrap()], dir.path());
    let init = mcp.initialize("2025-06-18");
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");

    let r = mcp.tool("wiki_status", json!({}));
    assert_eq!(r["isError"], false);
    // ≥2025-06-18 clients also get structuredContent.
    assert_eq!(r["structuredContent"]["initialized"], true);
    let p = mcp.payload("wiki_status", json!({}));
    assert_eq!(p["initialized"], true);
    assert!(p["counts"].is_object());

    let p = mcp.payload("wiki_query", json!({"question": "what is mcp-server?"}));
    for k in ["question", "matches", "superseded", "decisions", "conflicts", "gaps"] {
        assert!(p.get(k).is_some(), "wiki_query missing {k}: {p}");
    }
    let matched = p["matches"].as_array().unwrap();
    assert!(
        matched.iter().any(|m| {
            m.to_string().contains("claim_01ARZ3NDEKTSV4RRFFQ69G5FAV")
                || m["subject"] == "mcp-server"
        }),
        "query should match the seeded claim: {p}"
    );

    let p = mcp.payload("wiki_search_claims", json!({"subject": "mcp-server"}));
    assert!(
        p["claims"].as_array().unwrap().iter().any(|c| {
            c["id"] == "claim_01ARZ3NDEKTSV4RRFFQ69G5FAV" && c["subject"] == "mcp-server"
        }),
        "{p}"
    );
}

#[test]
fn unbound_tools_report_no_wiki_open() {
    let dir = tempfile::tempdir().unwrap(); // no .llm-wiki anywhere
    let mut mcp = Mcp::spawn(&[], dir.path());
    mcp.initialize("2025-06-18");
    let r = mcp.tool("wiki_status", json!({}));
    assert_eq!(r["isError"], true, "{r}");
    let p: Value = serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(p["error"]
        .as_str()
        .unwrap()
        .contains("no wiki open — call wiki_open"));
}

#[test]
fn wiki_open_binds_session_and_per_call_root_override() {
    let wiki_dir = tempfile::tempdir().unwrap();
    init_wiki(wiki_dir.path());
    let empty = tempfile::tempdir().unwrap();
    let mut mcp = Mcp::spawn(&[], empty.path());
    mcp.initialize("2025-06-18");

    // Explicit wiki_open binds the session.
    let p = mcp.payload("wiki_open", json!({"path": wiki_dir.path().to_str().unwrap()}));
    assert_eq!(p["initialized"], true, "{p}");
    assert_eq!(p["source"], "explicit");
    let s = mcp.payload("wiki_status", json!({}));
    assert_eq!(s["initialized"], true);

    // Per-call `root` overrides the binding for that call only.
    let other = tempfile::tempdir().unwrap();
    init_wiki(other.path());
    let p = mcp.payload("wiki_status", json!({"root": other.path().to_str().unwrap()}));
    assert_eq!(p["initialized"], true);
    let want = other.path().file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(p["project"], want, "{p}");
    // …and the session binding is unchanged afterwards.
    let s = mcp.payload("wiki_status", json!({}));
    let bound = wiki_dir.path().file_name().unwrap().to_string_lossy().into_owned();
    assert_eq!(s["project"], bound, "{s}");
}

#[test]
fn wiki_init_via_mcp_creates_and_binds() {
    let empty = tempfile::tempdir().unwrap();
    let target = empty.path().join("newproj");
    std::fs::create_dir_all(&target).unwrap();
    let mut mcp = Mcp::spawn(&[], empty.path());
    mcp.initialize("2025-06-18");
    let p = mcp.payload("wiki_init", json!({"path": target.to_str().unwrap()}));
    assert_eq!(p["initialized"], true, "{p}");
    // Session is now bound to the fresh wiki.
    let s = mcp.payload("wiki_status", json!({}));
    assert_eq!(s["initialized"], true);
}

#[test]
fn unknown_tool_is_iserror_not_rpc_error() {
    let dir = tempfile::tempdir().unwrap();
    let mut mcp = Mcp::spawn(&[], dir.path());
    mcp.initialize("2025-06-18");
    let r = mcp.request(
        "tools/call",
        json!({"name": "wiki_nope", "arguments": {}}),
    );
    assert!(r.get("error").is_none(), "{r}");
    assert_eq!(r["result"]["isError"], true);
    let p: Value =
        serde_json::from_str(r["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(p["error"].as_str().unwrap().contains("wiki_nope"));
}

#[test]
fn domain_error_surfaces_exit_code_and_error_type() {
    let dir = tempfile::tempdir().unwrap();
    init_wiki(dir.path());
    let mut mcp = Mcp::spawn(&["--root", dir.path().to_str().unwrap()], dir.path());
    mcp.initialize("2025-06-18");
    // page-show on a missing artifact is a domain error (exit 3 txn),
    // isError:true with the payload preserved — not an rpc error.
    let r = mcp.tool("wiki_read_page", json!({"artifact": "wiki/concepts/nope.md"}));
    assert_eq!(r["isError"], true, "{r}");
    let p: Value = serde_json::from_str(r["content"][0]["text"].as_str().unwrap()).unwrap();
    assert!(p.get("exit_code").is_some() && p.get("error_type").is_some(), "{p}");
}

// ------------------------------------------------------------ rpc layer

#[test]
fn unknown_method_is_32601_and_parse_error_is_32700() {
    let dir = tempfile::tempdir().unwrap();
    let mut mcp = Mcp::spawn(&[], dir.path());
    mcp.initialize("2025-06-18");
    let r = mcp.request("bogus/method", json!({}));
    assert_eq!(r["error"]["code"], -32601, "{r}");
    mcp.send_raw("{this is not valid json");
    let r = mcp.read_line();
    assert_eq!(r["error"]["code"], -32700, "{r}");
}

#[test]
fn notifications_get_no_response() {
    let dir = tempfile::tempdir().unwrap();
    let mut mcp = Mcp::spawn(&[], dir.path());
    mcp.initialize("2025-06-18");
    mcp.notify("notifications/initialized");
    mcp.notify("notifications/cancelled");
    mcp.notify("some/random/notification");
    // The next line on stdout must be the ping response — no reply was
    // emitted for any notification.
    let r = mcp.request("ping", json!({}));
    assert_eq!(r["result"], json!({}));
}

#[test]
fn pre_initialized_requests_are_tolerated() {
    let dir = tempfile::tempdir().unwrap();
    let mut mcp = Mcp::spawn(&[], dir.path());
    // tools/list before initialize is handled, not rejected (§7.1).
    let r = mcp.request("tools/list", json!({}));
    assert!(r.get("error").is_none());
    assert_eq!(r["result"]["tools"].as_array().unwrap().len(), 13);
}

// ------------------------------------------------------------ roots/list demux

#[test]
fn roots_list_roundtrip_with_interleaved_request() {
    let wiki_dir = tempfile::tempdir().unwrap();
    init_wiki(wiki_dir.path());
    let empty = tempfile::tempdir().unwrap();
    let mut mcp = Mcp::spawn(&[], empty.path());

    // Advertise the roots capability; don't respond yet.
    let id = mcp.next;
    mcp.next += 1;
    mcp.send(&json!({
        "jsonrpc": "2.0", "id": id, "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {"roots": {"listChanged": false}},
            "clientInfo": {"name": "test", "version": "0"},
        }
    }));
    let init = mcp.read_line();
    assert_eq!(init["id"], id);

    // A bound tool call triggers the server→client roots/list request.
    let call_id = mcp.next;
    mcp.next += 1;
    mcp.send(&json!({
        "jsonrpc": "2.0", "id": call_id, "method": "tools/call",
        "params": {"name": "wiki_status", "arguments": {}}
    }));
    let req = mcp.read_line();
    assert_eq!(req["method"], "roots/list", "{req}");
    let roots_id = req["id"].clone();
    assert!(roots_id.as_str().unwrap().starts_with("roots-"));

    // Interleave an unrelated request before answering roots/list — the
    // server must demux it rather than mis-correlate.
    let ping_id = mcp.next;
    mcp.next += 1;
    mcp.send(&json!({"jsonrpc": "2.0", "id": ping_id, "method": "ping"}));
    let pong = mcp.read_line();
    assert_eq!(pong["id"], ping_id, "{pong}");
    assert_eq!(pong["result"], json!({}));

    // Now answer roots/list with a file: URI + a non-file URI (filtered).
    mcp.send(&json!({
        "jsonrpc": "2.0", "id": roots_id,
        "result": {"roots": [
            {"uri": "vscode://ignore/me", "name": "not-file"},
            {"uri": format!("file://{}", wiki_dir.path().display()), "name": "w"},
        ]}
    }));
    let resp = mcp.read_line();
    assert_eq!(resp["id"], call_id, "{resp}");
    assert_eq!(resp["result"]["isError"], false);
    let p: Value = serde_json::from_str(
        resp["result"]["content"][0]["text"].as_str().unwrap(),
    )
    .unwrap();
    assert_eq!(p["initialized"], true, "{p}");
}

#[test]
fn no_roots_capability_means_no_roots_list_request() {
    let wiki_dir = tempfile::tempdir().unwrap();
    init_wiki(wiki_dir.path());
    let mut mcp = Mcp::spawn(&[], wiki_dir.path());
    // cwd-walk binds before the roots rung; with no roots capability the
    // server must never emit a server→client request.
    mcp.initialize("2025-06-18");
    let r = mcp.request("tools/call", json!({"name": "wiki_status", "arguments": {}}));
    assert_eq!(r["result"]["isError"], false, "{r}");
}

// ------------------------------------------------------------ resources

#[test]
fn resources_list_templates_read_and_complete() {
    let dir = tempfile::tempdir().unwrap();
    init_wiki(dir.path());
    write_claim(dir.path(), claim("claim_01ARZ3NDEKTSV4RRFFQ69G5FAV", "mcp-server"));
    std::fs::create_dir_all(dir.path().join(".llm-wiki/wiki/concepts")).unwrap();
    std::fs::write(
        dir.path().join(".llm-wiki/wiki/concepts/foo.md"),
        "---\ntype: concept\n---\nbody\n",
    )
    .unwrap();
    let mut mcp = Mcp::spawn(&["--root", dir.path().to_str().unwrap()], dir.path());
    mcp.initialize("2025-06-18");

    let r = mcp.request("resources/list", json!({}));
    let rs = r["result"]["resources"].as_array().unwrap();
    assert_eq!(rs.len(), 1);
    assert_eq!(rs[0]["uri"], "resource://llm-wiki/claims");
    assert_eq!(rs[0]["mimeType"], "application/json");

    let r = mcp.request("resources/templates/list", json!({}));
    let ts = r["result"]["resourceTemplates"].as_array().unwrap();
    assert_eq!(ts.len(), 3);
    for suffix in [
        "pages/{artifact}",
        "sources/{source_id}/manifest.json",
        "sources/{source_id}/content.md",
    ] {
        assert!(
            ts.iter()
                .any(|t| t["uriTemplate"].as_str().unwrap().ends_with(suffix)),
            "missing template {suffix}"
        );
    }

    // Synthesized claims projection — canonical scan, not sqlite.
    let r = mcp.request("resources/read", json!({"uri": "resource://llm-wiki/claims"}));
    let body: Value =
        serde_json::from_str(r["result"]["contents"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(body["claims"][0]["id"], "claim_01ARZ3NDEKTSV4RRFFQ69G5FAV");
    assert_eq!(body["claims"][0]["subject"], "mcp-server");

    // Page read via template.
    let r = mcp.request(
        "resources/read",
        json!({"uri": "resource://llm-wiki/pages/concepts/foo.md"}),
    );
    let text = r["result"]["contents"][0]["text"].as_str().unwrap();
    assert!(text.contains("type: concept"), "{text}");

    // Traversal guard: `..` escapes are refused with -32002.
    let r = mcp.request(
        "resources/read",
        json!({"uri": "resource://llm-wiki/pages/../wiki.json"}),
    );
    assert_eq!(r["error"]["code"], -32002, "{r}");
    let r = mcp.request(
        "resources/read",
        json!({"uri": "resource://llm-wiki/sources/../claims/content.md"}),
    );
    assert_eq!(r["error"]["code"], -32002, "{r}");

    // Completions over on-disk listings.
    let r = mcp.request(
        "completion/complete",
        json!({
            "ref": {"type": "ref/resource", "uri": "resource://llm-wiki/pages/{artifact}"},
            "argument": {"name": "artifact", "value": "conc"},
        }),
    );
    assert_eq!(r["result"]["completion"]["values"], json!(["concepts/foo.md"]));
}

#[test]
fn resources_read_truncates_large_files() {
    let dir = tempfile::tempdir().unwrap();
    init_wiki(dir.path());
    let big = dir.path().join(".llm-wiki/wiki/concepts");
    std::fs::create_dir_all(&big).unwrap();
    std::fs::write(big.join("big.md"), "x".repeat(300 * 1024)).unwrap();
    let mut mcp = Mcp::spawn(&["--root", dir.path().to_str().unwrap()], dir.path());
    mcp.initialize("2025-06-18");
    let r = mcp.request(
        "resources/read",
        json!({"uri": "resource://llm-wiki/pages/concepts/big.md"}),
    );
    let text = r["result"]["contents"][0]["text"].as_str().unwrap();
    assert!(text.ends_with("…[truncated]"), "len={}", text.len());
    assert!(text.len() <= 256 * 1024 + 64, "len={}", text.len());
}

// ------------------------------------------------------------ shutdown

#[test]
fn eof_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    let mut child = bin()
        .arg("mcp")
        .current_dir(dir.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    drop(child.stdin.take()); // immediate EOF
    let status = child.wait().unwrap();
    assert_eq!(status.code(), Some(0), "{status}");
}

#[test]
fn eof_after_session_exits_zero() {
    let dir = tempfile::tempdir().unwrap();
    let mut mcp = Mcp::spawn(&[], dir.path());
    mcp.initialize("2025-06-18");
    drop(mcp.stdin.take()); // EOF
    let status = mcp.child.wait().unwrap();
    assert_eq!(status.code(), Some(0), "{status}");
}
