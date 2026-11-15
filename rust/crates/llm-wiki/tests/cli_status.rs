use std::process::Command;

use serde_json::Value;

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

#[test]
fn status_uninitialized_wiki() {
    let dir = tempfile::tempdir().unwrap();
    let (code, v) = run_json(&["--root", dir.path().to_str().unwrap(), "status"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v, serde_json::json!({"initialized": false}));
}

#[test]
fn status_fresh_init_key_set_and_zeroes() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let (code, v) = run_json(&["--root", dir.path().to_str().unwrap(), "status"]);
    assert_eq!(code, 0, "{v}");

    // Exact key set per wiki.py:222-242 + plan counts.
    let keys: Vec<&str> = v.as_object().unwrap().keys().map(|k| k.as_str()).collect();
    for k in ["initialized", "project", "skill_version", "revision",
              "counts", "pending_versions", "stale_artifacts"] {
        assert!(keys.contains(&k), "missing key {k}: {v}");
    }
    assert_eq!(v["initialized"], true);
    // init names the project after the root basename.
    assert_eq!(v["project"].as_str().unwrap(), dir.path().file_name().unwrap().to_string_lossy().as_ref());
    assert_eq!(v["revision"], 0);
    assert_eq!(v["counts"]["sources"], 0);
    assert_eq!(v["counts"]["claims"], 0);
    assert_eq!(v["counts"]["decisions"], 0);
    assert_eq!(v["counts"]["pages"], 0);
    assert_eq!(v["counts"]["pending"], 0);
    assert_eq!(v["pending_versions"], 0);
    assert_eq!(v["stale_artifacts"], serde_json::json!([]));
    assert_eq!(v.as_object().unwrap().len(), 7, "{v}");
}

#[test]
fn status_after_ingest_counts_source_and_pending() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let (code, v) = run_json(&[
        "--root", dir.path().to_str().unwrap(),
        "ingest", "--text", "some evidence", "--text-ref", "t1",
    ]);
    assert_eq!(code, 0, "{v}");

    let (code, v) = run_json(&["--root", dir.path().to_str().unwrap(), "status"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["counts"]["sources"], 1, "{v}");
    assert_eq!(v["counts"]["pending"], 1, "{v}");
    assert_eq!(v["pending_versions"], 1, "{v}");
    assert!(v["revision"].as_i64().unwrap() >= 1, "{v}");
}
