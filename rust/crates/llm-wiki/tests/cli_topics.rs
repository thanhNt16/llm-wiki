mod common;

use std::process::Command;

use llm_wiki::topics;
use llm_wiki::txn::Txn;
use serde_json::{json, Value};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_llm-wiki"))
}

fn init(dir: &std::path::Path) {
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

fn claim_file(subject: &str, predicate: &str) -> Value {
    json!({
        "id": llm_wiki::ids::new("claim"),
        "subject": subject,
        "predicate": predicate,
        "value": "v",
        "status": "active",
        "version": 1,
    })
}

#[test]
fn topics_list_empty_registry() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let (code, v) = run_json(&["--root", dir.path().to_str().unwrap(), "topics"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["version"], 0);
    assert_eq!(v["topics"], json!([]));
}

#[test]
fn merge_rows_validates_and_keeps_extras() {
    let (_d, wiki) = common::wiki();
    let mut txn = Txn::begin(&wiki, "run_tm1").unwrap();
    let declared_by = json!({"source_id": "source_t1", "version": 1});
    let out = topics::merge_rows(
        &mut txn,
        &[json!({
            "subject": "graph.traversal",
            "kind": "concept",
            "title": "Graph Traversal",
            "links": ["wiki/concepts/other.md"],
            "aliases": ["graph-walk"],
        })],
        &declared_by,
    )
    .unwrap();
    assert_eq!(out["declared"], 1);
    assert_eq!(out["registered"], 1);
    txn.commit().unwrap();

    let reg = topics::load(&wiki).unwrap();
    let ent = &reg.topics["graph.traversal"];
    assert_eq!(ent["kind"], "concept");
    assert_eq!(ent["status"], "resolved");
    assert_eq!(ent["title"], "Graph Traversal");
    assert_eq!(ent["links"], json!(["wiki/concepts/other.md"]));
    assert_eq!(ent["aliases"], json!(["graph-walk"]));
    assert_eq!(ent["declared_by"], json!([declared_by]));
    assert_eq!(reg.version, 1);
}

#[test]
fn merge_rows_rejects_missing_subject_with_row_index() {
    let (_d, wiki) = common::wiki();
    let mut txn = Txn::begin(&wiki, "run_tm2").unwrap();
    let declared_by = json!({"source_id": "source_t1", "version": 1});
    let err = topics::merge_rows(
        &mut txn,
        &[
            json!({"subject": "ok.subj", "kind": "concept", "title": "Ok"}),
            json!({"kind": "concept", "title": "No Subject"}),
        ],
        &declared_by,
    )
    .unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("row 1"), "{msg}");
    assert!(msg.contains("subject"), "{msg}");
}

#[test]
fn merge_rows_rejects_bad_kind() {
    let (_d, wiki) = common::wiki();
    let mut txn = Txn::begin(&wiki, "run_tm3").unwrap();
    let err = topics::merge_rows(
        &mut txn,
        &[json!({"subject": "s", "kind": "widget", "title": "S"})],
        &json!({}),
    )
    .unwrap_err();
    assert!(err.to_string().contains("bad kind"), "{err}");
}

#[test]
fn topics_backfill_populates_from_claims() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let dot = dir.path().join(".llm-wiki");
    // Write a claim directly — no claim-write command exists yet.
    let claim = claim_file("feature.flags", "enables");
    std::fs::write(
        dot.join(format!("claims/{}.json", claim["id"].as_str().unwrap())),
        serde_json::to_string_pretty(&claim).unwrap(),
    )
    .unwrap();

    let (code, v) = run_json(&[
        "--root", dir.path().to_str().unwrap(),
        "topics", "--backfill",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["topics_registered"], 1, "{v}");

    let (code, v) = run_json(&["--root", dir.path().to_str().unwrap(), "topics"]);
    assert_eq!(code, 0, "{v}");
    let topics = v["topics"].as_array().unwrap();
    assert_eq!(topics.len(), 1, "{v}");
    assert_eq!(topics[0]["subject"], "feature.flags");
    assert_eq!(topics[0]["kind"], "concept");
    assert_eq!(topics[0]["title"], "Flags");
    assert_eq!(topics[0]["claim_count"], 1);
    assert_eq!(topics[0]["auto_registered"], true);
    assert_eq!(topics[0]["declared_by"], 1);

    // Idempotent — second backfill registers nothing.
    let (code, v) = run_json(&[
        "--root", dir.path().to_str().unwrap(),
        "topics", "--backfill",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["topics_registered"], 0, "{v}");
}
