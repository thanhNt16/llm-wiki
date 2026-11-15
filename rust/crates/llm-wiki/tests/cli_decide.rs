mod common;

use std::path::Path;
use std::process::Command;

use llm_wiki::claims;
use llm_wiki::ids;
use llm_wiki::schema;
use llm_wiki::txn::Txn;
use llm_wiki::util;
use serde_json::{json, Value};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_llm-wiki"))
}

fn init(dir: &Path) {
    let out = bin().arg("init").arg(dir).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "init: {:?}", out);
}

fn run_json(args: &[&str]) -> (i32, Value) {
    let out = bin().args(args).output().unwrap();
    let code = out.status.code().unwrap_or(-1);
    let v: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout not JSON ({e}): {}", String::from_utf8_lossy(&out.stdout)));
    (code, v)
}

fn seed_claim(dir: &Path, status: &str) -> Value {
    let wiki = llm_wiki::store::Wiki::open(dir).unwrap();
    let mut c = json!({
        "id": ids::new("claim"),
        "subject": "s",
        "predicate": "is",
        "value": "v",
        "status": status,
        "authority": {"type": "agent_inference"},
        "evidence": [],
        "recorded_at": util::now_iso(),
        "version": 1,
    });
    if claims::TERMINAL.contains(&status) {
        c["acceptance"] = json!({"reason": "seed fixture"});
    }
    let mut txn = Txn::begin(&wiki, &ids::new_run()).unwrap();
    claims::save_claim(&wiki, &mut txn, &mut c).unwrap();
    txn.commit().unwrap();
    c
}

/// Seed a source manifest with one version (for --evidence refs).
fn seed_source(dir: &Path) -> String {
    let sid = ids::new("source");
    let mdir = dir.join(format!(".llm-wiki/sources/{sid}"));
    std::fs::create_dir_all(&mdir).unwrap();
    std::fs::write(
        mdir.join("manifest.json"),
        serde_json::to_string(&json!({
            "id": sid,
            "origin": "file:///tmp/x.md",
            "versions": [{"version": 1}],
        }))
        .unwrap(),
    )
    .unwrap();
    sid
}

#[test]
fn decide_creates_schema_valid_decision_and_receipt() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let claim = seed_claim(dir.path(), "candidate");
    let cid = claim["id"].as_str().unwrap();
    let sid = seed_source(dir.path());
    let root = dir.path().to_str().unwrap().to_string();

    let (code, v) = run_json(&[
        "--root", &root, "decide",
        "--title", "use JSONL",
        "--claims", cid,
        "--evidence", &format!("{sid}@1"),
        "--authority", "explicit_project_decision",
        "--decided-on", "2026-11-15",
        "--body", "keeps append-only receipts cheap",
    ]);
    assert_eq!(code, 0, "{v}");
    let d = &v["decision"];
    assert!(d["id"].as_str().unwrap().starts_with("decision_"));
    assert_eq!(d["title"], "use JSONL");
    assert_eq!(d["status"], "accepted");
    assert_eq!(d["decided_on"], "2026-11-15");
    assert_eq!(d["claims"], json!([cid]));
    assert_eq!(d["evidence"][0]["source_id"], sid);
    assert_eq!(d["evidence"][0]["source_version"], 1);
    assert_eq!(d["evidence"][0]["locator"]["type"], "text");
    assert_eq!(d["evidence"][0]["locator"]["value"], "file:///tmp/x.md");
    assert_eq!(d["authority"]["type"], "explicit_project_decision");
    assert_eq!(d["body"], "keeps append-only receipts cheap");
    assert_eq!(d["version"], 1);
    assert!(d["recorded_at"].is_string());
    assert!(v["run_id"].as_str().unwrap().starts_with("run_"));
    assert_eq!(v["revision"]["before"], 1);
    assert_eq!(v["revision"]["after"], 2);

    // schema-valid canonical file
    let schema_v: Value = serde_json::from_str(schema::DECISION_SCHEMA).unwrap();
    assert!(schema::validate(d, &schema_v).is_empty());
    let file = dir
        .path()
        .join(format!(".llm-wiki/decisions/{}.json", d["id"].as_str().unwrap()));
    assert!(file.is_file());
    let on_disk: Value =
        serde_json::from_str(&std::fs::read_to_string(file).unwrap()).unwrap();
    assert_eq!(on_disk["id"], d["id"]);
}

#[test]
fn decide_defaults_status_and_decided_on() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let root = dir.path().to_str().unwrap().to_string();
    let (code, v) = run_json(&["--root", &root, "decide", "--title", "t"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["decision"]["status"], "accepted");
    assert_eq!(v["decision"]["decided_on"], util::today());
    assert_eq!(v["decision"]["claims"], json!([]));
    assert_eq!(v["decision"]["evidence"], json!([]));
}

#[test]
fn decide_rejects_bad_claim_and_evidence_refs() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let root = dir.path().to_str().unwrap().to_string();

    let (code, v) = run_json(&[
        "--root", &root, "decide", "--title", "t",
        "--claims", "not-a-claim",
    ]);
    assert_eq!(code, 3, "{v}");
    assert!(v["error"].as_str().unwrap().contains("claim_<id>"));

    let (code, v) = run_json(&[
        "--root", &root, "decide", "--title", "t",
        "--evidence", "no-at-sign",
    ]);
    assert_eq!(code, 3, "{v}");
    assert!(v["error"].as_str().unwrap().contains("source_<id>@<version>"));

    let (code, v) = run_json(&[
        "--root", &root, "decide", "--title", "t",
        "--evidence", &format!("{}@9", ids::new("source")),
    ]);
    assert_eq!(code, 3, "{v}");
    assert!(v["error"].as_str().unwrap().contains("unknown evidence source"));
}

#[test]
fn decide_rejects_unknown_claim_id() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let root = dir.path().to_str().unwrap().to_string();
    let (code, v) = run_json(&[
        "--root", &root, "decide", "--title", "t",
        "--claims", &ids::new("claim"),
    ]);
    assert_eq!(code, 3, "{v}");
    assert!(v["error"].as_str().unwrap().contains("unknown claim"));
}

#[test]
fn record_decision_commits_file_input() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let claim = seed_claim(dir.path(), "accepted");
    let cid = claim["id"].as_str().unwrap();
    let f = dir.path().join("decision.json");
    std::fs::write(
        &f,
        serde_json::to_string(&json!({
            "title": "ship it",
            "status": "proposed",
            "decided_on": "2026-11-14",
            "claims": [cid],
            "evidence": [],
            "body": "rationale",
        }))
        .unwrap(),
    )
    .unwrap();
    let root = dir.path().to_str().unwrap().to_string();
    let (code, v) = run_json(&[
        "--root", &root, "record-decision",
        "--file", f.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    let did = v["id"].as_str().unwrap();
    assert!(did.starts_with("decision_"));
    let d = &v["decision"];
    assert_eq!(d["id"], v["id"]);
    assert_eq!(d["title"], "ship it");
    assert_eq!(d["status"], "proposed");
    assert_eq!(d["version"], 1);
    assert!(d["recorded_at"].is_string());
    assert_eq!(v["receipt"]["command"], "wiki-record-decision");
    assert_eq!(v["receipt"]["changes"]["decisions_created"], 1);

    let schema_v: Value = serde_json::from_str(schema::DECISION_SCHEMA).unwrap();
    assert!(schema::validate(d, &schema_v).is_empty());
    let file = dir.path().join(format!(".llm-wiki/decisions/{did}.json"));
    assert!(file.is_file());
    // index gains the decision id
    let idx: Value = serde_json::from_str(
        &std::fs::read_to_string(
            dir.path().join(".llm-wiki/.state/claims-index.json"),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(idx["decisions"].as_array().unwrap().contains(&json!(did)));
}

#[test]
fn record_decision_rejects_engine_stamped_fields() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    for banned in ["id", "recorded_at", "version"] {
        let f = dir.path().join(format!("d-{banned}.json"));
        let mut data = json!({
            "title": "t", "status": "accepted", "decided_on": "2026-11-14",
            "claims": [], "evidence": [],
        });
        data[banned] = match banned {
            "id" => json!(ids::new("decision")),
            "version" => json!(1),
            _ => json!("2026-11-14T00:00:00Z"),
        };
        std::fs::write(&f, serde_json::to_string(&data).unwrap()).unwrap();
        let root = dir.path().to_str().unwrap().to_string();
        let (code, v) = run_json(&[
            "--root", &root, "record-decision", "--file", f.to_str().unwrap(),
        ]);
        assert_eq!(code, 3, "{v}");
        assert!(v["error"]
            .as_str()
            .unwrap()
            .contains(&format!("must not include '{banned}'")));
    }
}

#[test]
fn record_decision_rejects_terminal_claim_and_unknown_source() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let rejected = seed_claim(dir.path(), "rejected");
    let cid = rejected["id"].as_str().unwrap();
    let root = dir.path().to_str().unwrap().to_string();

    let f = dir.path().join("d1.json");
    std::fs::write(
        &f,
        serde_json::to_string(&json!({
            "title": "t", "status": "accepted", "decided_on": "2026-11-14",
            "claims": [cid], "evidence": [],
        }))
        .unwrap(),
    )
    .unwrap();
    let (code, v) = run_json(&[
        "--root", &root, "record-decision", "--file", f.to_str().unwrap(),
    ]);
    assert_eq!(code, 3, "{v}");
    assert!(v["error"].as_str().unwrap().contains("cannot support a decision"));

    let f = dir.path().join("d2.json");
    std::fs::write(
        &f,
        serde_json::to_string(&json!({
            "title": "t", "status": "accepted", "decided_on": "2026-11-14",
            "claims": [],
            "evidence": [{"source_id": ids::new("source"), "source_version": 1,
                          "locator": {"type": "text"}}],
        }))
        .unwrap(),
    )
    .unwrap();
    let (code, v) = run_json(&[
        "--root", &root, "record-decision", "--file", f.to_str().unwrap(),
    ]);
    assert_eq!(code, 3, "{v}");
    assert!(v["error"].as_str().unwrap().contains("unknown source"));
}
