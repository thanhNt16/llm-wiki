//! `jev` CLI tests — the System One advisory client end to end:
//! env-gating (`--check`, disabled ask), ureq POST + answer normalization
//! against a tiny_http mock, the sqlite answer cache (hit = zero network,
//! token excluded from the key), never-raises failure modes, and the
//! §6.4 `auto_prefill` lane through reconcile-prepare →
//! reconcile-apply --use-prefill.
mod common;

use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};

use llm_wiki::compile;
use llm_wiki::evidence::sources::{self, IngestOpts};
use llm_wiki::index::Index;
use llm_wiki::store::Wiki;
use serde_json::{json, Value};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_llm-wiki"))
}

/// Build a `llm-wiki` command with every DECISION_* env scrubbed (the
/// parent environment must never leak readiness into these tests).
fn wiki_cmd() -> Command {
    let mut c = bin();
    c.env_remove("DECISION_TOKEN")
        .env_remove("DECISION_MODEL")
        .env_remove("DECISION_ENDPOINT");
    c
}

fn run_json(cmd: &mut Command, args: &[&str]) -> (i32, Value) {
    let out = cmd.args(args).output().unwrap();
    let code = out.status.code().unwrap_or(-1);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    (code, v)
}

// ---------------------------------------------------------------------------
// mock System One server
// ---------------------------------------------------------------------------

struct Captured {
    hits: usize,
    bodies: Vec<Value>,
    authorizations: Vec<String>,
}

struct MockJev {
    url: String,
    captured: Arc<Mutex<Captured>>,
}

impl MockJev {
    /// Serve `body` verbatim as JSON for every request; non-object bodies
    /// and `{"raw": "..."}` (raw text) are supported for failure lanes.
    fn start(body: Value) -> Self {
        let server = tiny_http::Server::http("127.0.0.1:0").unwrap();
        let port = server.server_addr().to_ip().unwrap().port();
        let captured = Arc::new(Mutex::new(Captured {
            hits: 0,
            bodies: Vec::new(),
            authorizations: Vec::new(),
        }));
        let cap = Arc::clone(&captured);
        std::thread::spawn(move || {
            while let Ok(Some(mut req)) =
                server.recv_timeout(std::time::Duration::from_secs(30))
            {
                let mut text = String::new();
                let _ = req.as_reader().read_to_string(&mut text);
                {
                    let mut c = cap.lock().unwrap();
                    c.hits += 1;
                    c.bodies.push(
                        serde_json::from_str(&text).unwrap_or(Value::Null),
                    );
                    c.authorizations.push(
                        req.headers()
                            .iter()
                            .find(|h| {
                                h.field.as_str().as_str().eq_ignore_ascii_case("authorization")
                            })
                            .map(|h| h.value.as_str().to_string())
                            .unwrap_or_default(),
                    );
                }
                let (status, raw) = match body.get("raw").and_then(Value::as_str) {
                    Some(r) => (
                        body["status"].as_u64().unwrap_or(200) as u32,
                        r.to_string(),
                    ),
                    None => (200, serde_json::to_string(&body).unwrap()),
                };
                let resp = tiny_http::Response::from_string(raw)
                    .with_status_code(status)
                    .with_header(
                        tiny_http::Header::from_bytes(
                            &b"Content-Type"[..],
                            &b"application/json"[..],
                        )
                        .unwrap(),
                    );
                let _ = req.respond(resp);
            }
        });
        MockJev {
            url: format!("http://127.0.0.1:{port}/v1/systemone"),
            captured,
        }
    }

    fn hits(&self) -> usize {
        self.captured.lock().unwrap().hits
    }

    fn bodies(&self) -> Vec<Value> {
        self.captured.lock().unwrap().bodies.clone()
    }

    fn authorizations(&self) -> Vec<String> {
        self.captured.lock().unwrap().authorizations.clone()
    }
}

fn wire(answers: Value) -> Value {
    json!({
        "success": true,
        "result": {"model": "m/test", "answers": answers,
                   "usage": {"tokens": 1}},
    })
}

fn jev_args<'a>(root: &'a str, rest: &'a [&'a str]) -> Vec<&'a str> {
    let mut v = vec!["--root", root, "jev"];
    v.extend_from_slice(rest);
    v
}

// ---------------------------------------------------------------------------
// --check / arg validation
// ---------------------------------------------------------------------------

#[test]
fn jev_check_reports_readiness() {
    let (_d, wiki) = common::wiki();
    let r = wiki.root.to_str().unwrap().to_string();

    let (code, v) = run_json(&mut wiki_cmd(), &jev_args(&r, &["--check"]));
    assert_eq!(code, 0);
    assert_eq!(v["ready"], json!(false));
    let reason = v["reason"].as_str().unwrap().to_string();
    assert!(
        reason.contains("DECISION_TOKEN") && reason.contains("unset"),
        "{reason}"
    );

    let (code, v) = run_json(
        &mut wiki_cmd()
            .env("DECISION_TOKEN", "sk-test")
            .env("DECISION_MODEL", "m/test"),
        &jev_args(&r, &["--check"]),
    );
    assert_eq!(code, 0);
    assert_eq!(
        v,
        json!({"ready": true, "model": "m/test",
               "endpoint": "http://localhost:20128/v1/systemone"})
    );
}

#[test]
fn jev_usage_and_question_validation() {
    let (_d, wiki) = common::wiki();
    let r = wiki.root.to_str().unwrap().to_string();

    let (code, v) = run_json(&mut wiki_cmd(), &jev_args(&r, &[]));
    assert_eq!(code, 3);
    assert_eq!(v["error"], json!("provide --check or --question <json>"));

    let (code, v) = run_json(&mut wiki_cmd(), &jev_args(&r, &["--question", "nope"]));
    assert_eq!(code, 3);
    assert!(v["error"].as_str().unwrap().contains("not valid JSON"));

    let (code, v) = run_json(&mut wiki_cmd(), &jev_args(&r, &["--question", "[1]"]));
    assert_eq!(code, 3);
    assert!(v["error"]
        .as_str()
        .unwrap()
        .contains("must be a JSON object"));
}

// ---------------------------------------------------------------------------
// disabled ask — env unset => {ok:false, disabled:true}, exit 0, no network
// ---------------------------------------------------------------------------

#[test]
fn jev_disabled_ask_never_networks() {
    let (_d, wiki) = common::wiki();
    let r = wiki.root.to_str().unwrap().to_string();
    let srv = MockJev::start(wire(json!({})));

    let (code, v) = run_json(
        // endpoint set but token/model absent => disabled, no request
        &mut wiki_cmd().env("DECISION_ENDPOINT", &srv.url),
        &jev_args(&r, &["--question", "{\"q\":{\"type\":\"noul\"}}"]),
    );
    assert_eq!(code, 0);
    assert_eq!(
        v,
        json!({"ok": false, "disabled": true, "answers": {},
               "usage": {}, "error": null})
    );
    assert_eq!(srv.hits(), 0, "disabled ask must not hit the network");
}

// ---------------------------------------------------------------------------
// happy path: normalized answers, wire translation, Authorization header
// ---------------------------------------------------------------------------

#[test]
fn jev_ask_normalizes_and_posts_verbatim_wire() {
    let (_d, wiki) = common::wiki();
    let r = wiki.root.to_str().unwrap().to_string();
    let srv = MockJev::start(wire(json!({
        "q": {"type": "noul", "noul": 0.9},
        "kind": {"type": "choice", "choice": "entities",
                 "confidence": 0.62,
                 "probabilities": {"concepts": 0.1, "entities": 0.8,
                                   "procedures": 0.05, "questions": 0.05}},
        "sev": {"type": "score", "score": 1.0,
                "probabilities": {"0": 0.0, "1": 1.0, "2": 0.0}},
    })));

    let (code, v) = run_json(
        &mut wiki_cmd()
            .env("DECISION_TOKEN", "sk-test")
            .env("DECISION_MODEL", "m/test")
            .env("DECISION_ENDPOINT", &srv.url),
        &jev_args(&r, &[
            "--state", "state text",
            "--question",
            "{\"q\":{\"type\":\"noul\",\"instructions\":\"?\"},\
              \"kind\":{\"type\":\"choice\",\"options\":[\"concepts\",\"entities\"]},\
              \"sev\":{\"type\":\"score\",\"levels\":3}}",
        ]),
    );
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["ok"], json!(true));
    assert_eq!(v["disabled"], json!(false));
    assert_eq!(v["error"], Value::Null);
    assert_eq!(v["answers"]["q"], json!({"type": "noul", "value": 0.9}));
    let kind = &v["answers"]["kind"];
    assert_eq!(kind["type"], json!("choice"));
    assert_eq!(kind["value"]["pick"], json!("entities"));
    assert_eq!(kind["value"]["distribution"]["entities"], json!(0.8));
    assert_eq!(kind["value"]["confidence"], json!(0.62));
    assert_eq!(
        v["answers"]["sev"],
        json!({"type": "score", "value": {"level": 1, "expected": 1.0}})
    );
    assert_eq!(v["usage"], json!({"tokens": 1}));

    // wire payload: bearer auth, {model,state,questions}; options->criteria
    assert_eq!(srv.hits(), 1);
    assert_eq!(srv.authorizations()[0], "Bearer sk-test");
    let req = &srv.bodies()[0];
    assert_eq!(req["model"], json!("m/test"));
    assert_eq!(req["state"], json!("state text"));
    assert_eq!(req["questions"]["q"]["type"], json!("noul"));
    assert_eq!(
        req["questions"]["kind"]["criteria"],
        json!({"concepts": "concepts", "entities": "entities"})
    );
    assert!(req["questions"]["kind"].get("options").is_none());
    assert_eq!(
        req["questions"]["sev"]["criteria"],
        json!(["level 0", "level 1", "level 2"])
    );
}

// ---------------------------------------------------------------------------
// sqlite answer cache: second identical ask is served network-free, the
// token is excluded from the key, failures are never cached
// ---------------------------------------------------------------------------

#[test]
fn jev_cache_hit_skips_network_and_ignores_token() {
    let (_d, wiki) = common::wiki();
    let r = wiki.root.to_str().unwrap().to_string();
    let srv = MockJev::start(wire(json!({"q": {"type": "noul", "noul": 0.9}})));
    let question = "{\"q\":{\"type\":\"noul\",\"instructions\":\"?\"}}";

    let ask_with = |token: &str| {
        run_json(
            &mut wiki_cmd()
                .env("DECISION_TOKEN", token)
                .env("DECISION_MODEL", "m/test")
                .env("DECISION_ENDPOINT", &srv.url),
            &jev_args(&r, &["--state", "s", "--question", question]),
        )
    };

    let (code, v1) = ask_with("sk-one");
    assert_eq!(code, 0);
    assert_eq!(v1["ok"], json!(true));
    assert_eq!(srv.hits(), 1);

    // same state+questions, different token => same cache key => no POST
    let (code, v2) = ask_with("sk-two");
    assert_eq!(code, 0);
    assert_eq!(v2, v1, "cache hit replays the stored payload");
    assert_eq!(srv.hits(), 1, "second identical ask must not network");

    // one row in jev_answers for this key
    let idx = Index::open_or_rebuild(&wiki).unwrap();
    let n: i64 = idx
        .conn()
        .query_row("SELECT COUNT(*) FROM jev_answers", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);

    // a different state is a different key -> a second request lands
    let (code, _) = run_json(
        &mut wiki_cmd()
            .env("DECISION_TOKEN", "sk-two")
            .env("DECISION_MODEL", "m/test")
            .env("DECISION_ENDPOINT", &srv.url),
        &jev_args(&r, &["--state", "other state", "--question", question]),
    );
    assert_eq!(code, 0);
    assert_eq!(srv.hits(), 2);
}

#[test]
fn jev_failures_are_not_cached() {
    let (_d, wiki) = common::wiki();
    let r = wiki.root.to_str().unwrap().to_string();
    let srv = MockJev::start(json!({"success": false, "error": "bad model"}));
    let question = "{\"q\":{\"type\":\"noul\"}}";

    for _ in 0..2 {
        let (code, v) = run_json(
            &mut wiki_cmd()
                .env("DECISION_TOKEN", "sk-test")
                .env("DECISION_MODEL", "m/test")
                .env("DECISION_ENDPOINT", &srv.url),
            &jev_args(&r, &["--question", question]),
        );
        assert_eq!(code, 0);
        assert_eq!(v["ok"], json!(false));
        assert_eq!(v["disabled"], json!(false));
        assert_eq!(v["error"], json!("bad model"));
    }
    assert_eq!(srv.hits(), 2, "failures must retry, never cache");
}

#[test]
fn jev_malformed_response_returns_ok_false() {
    let (_d, wiki) = common::wiki();
    let r = wiki.root.to_str().unwrap().to_string();
    let srv = MockJev::start(json!({"raw": "this is not json"}));

    let (code, v) = run_json(
        &mut wiki_cmd()
            .env("DECISION_TOKEN", "sk-test")
            .env("DECISION_MODEL", "m/test")
            .env("DECISION_ENDPOINT", &srv.url),
        &jev_args(&r, &["--question", "{\"q\":{\"type\":\"noul\"}}"]),
    );
    assert_eq!(code, 0, "ask never raises: {v}");
    assert_eq!(v["ok"], json!(false));
    assert_eq!(v["disabled"], json!(false));
    assert!(v["error"].is_string());
}

#[test]
fn jev_http_error_and_unreachable_are_ok_false() {
    let (_d, wiki) = common::wiki();
    let r = wiki.root.to_str().unwrap().to_string();

    // unreachable endpoint: connection refused => ok:false, exit 0
    let (code, v) = run_json(
        &mut wiki_cmd()
            .env("DECISION_TOKEN", "sk-test")
            .env("DECISION_MODEL", "m/test")
            .env("DECISION_ENDPOINT", "http://127.0.0.1:1/v1/systemone"),
        &jev_args(&r, &["--question", "{\"q\":{\"type\":\"noul\"}}"]),
    );
    assert_eq!(code, 0);
    assert_eq!(v["ok"], json!(false));

    // HTTP 500 with a detail body => "HTTP 500 <body>" like urllib's lane
    let srv = MockJev::start(json!({"raw": "upstream boom", "status": 500}));
    let (code, v) = run_json(
        &mut wiki_cmd()
            .env("DECISION_TOKEN", "sk-test")
            .env("DECISION_MODEL", "m/test")
            .env("DECISION_ENDPOINT", &srv.url),
        &jev_args(&r, &["--question", "{\"q\":{\"type\":\"noul\"}}"]),
    );
    assert_eq!(code, 0);
    assert_eq!(v["ok"], json!(false));
    let err = v["error"].as_str().unwrap_or("");
    assert!(err.starts_with("HTTP 500"), "{err}");
    assert!(err.contains("upstream boom"), "{err}");
}

// ---------------------------------------------------------------------------
// §6.4 auto_prefill: reconcile-prepare attaches a ≥0.7 Jev verdict and
// reconcile-apply --use-prefill consumes it; <0.7 leaves the block absent.
// ---------------------------------------------------------------------------

fn ingest_text_lib(wiki: &Wiki, text: &str, text_ref: &str) -> (String, i64) {
    let r = sources::ingest_text(wiki, text, text_ref, "text", &IngestOpts::default())
        .expect("ingest_text");
    (
        r["source_id"].as_str().unwrap().to_string(),
        r["version"].as_i64().unwrap(),
    )
}

fn stage_shorthand(wiki: &Wiki, sid: &str, ver: i64, subject: &str) -> String {
    compile::stage_candidates(
        wiki,
        sid,
        ver,
        vec![json!({
            "subject": subject,
            "predicate": "mode",
            "value": "on",
        })],
        &json!({"source_id": sid, "source_version": ver}),
        None,
    )
    .expect("stage_candidates")
}

fn prepare_with_jev(
    root: &Path,
    run_id: &str,
    endpoint: &str,
) -> (i32, Value) {
    let run = run_id.to_string();
    let root_s = root.to_str().unwrap().to_string();
    run_json(
        &mut wiki_cmd()
            .env("DECISION_TOKEN", "sk-test")
            .env("DECISION_MODEL", "m/test")
            .env("DECISION_ENDPOINT", endpoint),
        &["--root", &root_s, "reconcile-prepare", "--run", &run],
    )
}

#[test]
fn auto_prefill_flows_through_use_prefill() {
    let (_d, wiki) = common::wiki();
    let srv = MockJev::start(wire(json!({
        "verdict": {"type": "choice", "choice": "UNRELATED",
                    "probabilities": {"UNRELATED": 0.9, "DUPLICATE": 0.1}},
    })));

    let (sid, ver) = ingest_text_lib(&wiki, "doc", "d");
    let run_id = stage_shorthand(&wiki, &sid, ver, "eps.zeta");
    let (code, prep) =
        prepare_with_jev(&wiki.root, &run_id, &srv.url);
    assert_eq!(code, 0, "{prep}");
    let pre = &prep["comparisons"][0]["auto_prefill"];
    assert_eq!(
        *pre,
        json!({"verdict": "UNRELATED", "confidence": 0.9, "jev": true}),
        "{prep}"
    );

    // on-disk comparisons.json carries the block for --use-prefill
    let rundir = wiki.p(&format!(".state/staging/{run_id}"));
    let disk: Value = serde_json::from_str(
        &std::fs::read_to_string(rundir.join("comparisons.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(disk["comparisons"][0]["auto_prefill"]["jev"], json!(true));

    let receipt = compile::reconcile_apply(
        &wiki,
        &run_id,
        vec![],
        wiki.revision().unwrap(),
        None,
        None,
        true,
    )
    .unwrap();
    assert_eq!(receipt["changes"]["claims_created"], 1, "{receipt}");
    assert_eq!(receipt["auto"]["prefill"], 1, "{receipt}");
}

#[test]
fn auto_prefill_below_threshold_absent_and_ask_failure_absent() {
    let (_d, wiki) = common::wiki();
    // 0.4 confidence -> block absent entirely
    let srv = MockJev::start(wire(json!({
        "verdict": {"type": "choice", "choice": "UNRELATED",
                    "probabilities": {"UNRELATED": 0.4, "DUPLICATE": 0.6}},
    })));
    let (sid, ver) = ingest_text_lib(&wiki, "doc", "d");
    let run_id = stage_shorthand(&wiki, &sid, ver, "eps.zeta");
    let (code, prep) = prepare_with_jev(&wiki.root, &run_id, &srv.url);
    assert_eq!(code, 0, "{prep}");
    assert!(
        prep["comparisons"][0].get("auto_prefill").is_none(),
        "sub-0.7 verdict must leave no block: {prep}"
    );

    // ask failure (unreachable endpoint) -> absent, prepare still succeeds
    let (sid2, ver2) = ingest_text_lib(&wiki, "doc2", "d2");
    let run2 = stage_shorthand(&wiki, &sid2, ver2, "eta.theta");
    let (code, prep2) =
        prepare_with_jev(&wiki.root, &run2, "http://127.0.0.1:1/v1/systemone");
    assert_eq!(code, 0, "{prep2}");
    assert!(prep2["comparisons"][0].get("auto_prefill").is_none());
}
