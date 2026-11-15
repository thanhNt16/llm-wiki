mod common;

use std::process::Command;

use serde_json::{json, Value};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_llm-wiki"))
}

fn run_json(args: &[&str]) -> (i32, Value) {
    let out = bin().args(args).output().unwrap();
    let code = out.status.code().unwrap_or(-1);
    let v: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout not JSON ({e}): {}", String::from_utf8_lossy(&out.stdout)));
    (code, v)
}

fn write_claim(dir: &std::path::Path, id: &str, subject: &str, predicate: &str, status: &str, sources: &[&str]) {
    let evidence: Vec<Value> = sources
        .iter()
        .map(|s| json!({"source_id": s, "version": 1}))
        .collect();
    let claim = json!({
        "id": id,
        "subject": subject,
        "predicate": predicate,
        "value": "v",
        "status": status,
        "version": 1,
        "evidence": evidence,
    });
    std::fs::write(
        dir.join(format!(".llm-wiki/claims/{id}.json")),
        serde_json::to_string(&claim).unwrap(),
    )
    .unwrap();
}

#[test]
fn subjects_rollup_keys_and_prefix() {
    let (dir, _wiki) = common::wiki();
    let root = dir.path();
    write_claim(root, "claim_a1", "api.orders", "status", "accepted", &["source_s1"]);
    write_claim(root, "claim_a2", "api.orders", "feature", "provisional", &["source_s1", "source_s2"]);
    write_claim(root, "claim_b1", "db.schema", "status", "rejected", &[]);
    let (code, v) = run_json(&["--root", root.to_str().unwrap(), "subjects"]);
    assert_eq!(code, 0, "{v}");
    let rows = v["subjects"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{v}");
    assert_eq!(rows[0]["subject"], "api.orders");
    assert_eq!(rows[0]["claim_count"], 2);
    assert_eq!(rows[0]["predicates"], json!(["feature", "status"]));
    assert_eq!(rows[0]["statuses"], json!({"accepted": 1, "provisional": 1}));
    assert_eq!(rows[0]["source_count"], 2);
    assert_eq!(rows[0]["stale_pages"], json!([]));
    assert_eq!(rows[1]["subject"], "db.schema");
    assert_eq!(rows[1]["statuses"], json!({"rejected": 1}));
    assert_eq!(rows[1]["source_count"], 0);

    // --prefix filter
    let (code, v) = run_json(&["--root", root.to_str().unwrap(), "subjects", "--prefix", "api."]);
    assert_eq!(code, 0, "{v}");
    let rows = v["subjects"].as_array().unwrap();
    assert_eq!(rows.len(), 1, "{v}");
    assert_eq!(rows[0]["subject"], "api.orders");
}

#[test]
fn subjects_stale_page_subject_and_deps_fallback() {
    let (dir, _wiki) = common::wiki();
    let root = dir.path();
    write_claim(root, "claim_k1", "ui.search", "status", "accepted", &[]);
    write_claim(root, "claim_k2", "ui.login", "status", "accepted", &[]);
    let concepts = root.join(".llm-wiki/wiki/concepts");
    std::fs::create_dir_all(&concepts).unwrap();
    // page with declared subject
    std::fs::write(
        concepts.join("search.md"),
        "---\nsubject: ui.search\nstale: true\n---\nbody\n",
    )
    .unwrap();
    // page attributed only via deps fallback (claim id before @version)
    std::fs::write(
        concepts.join("login.md"),
        "---\ndeps:\n  - claim_k2@1\nstale: true\n---\nbody\n",
    )
    .unwrap();
    // non-stale page ignored
    std::fs::write(concepts.join("fresh.md"), "---\nsubject: ui.search\n---\nbody\n").unwrap();
    // index.md never counts
    std::fs::write(concepts.join("index.md"), "---\nstale: true\n---\n").unwrap();

    let (code, v) = run_json(&["--root", root.to_str().unwrap(), "subjects"]);
    assert_eq!(code, 0, "{v}");
    let rows = v["subjects"].as_array().unwrap();
    assert_eq!(rows.len(), 2, "{v}");
    assert_eq!(rows[0]["subject"], "ui.login");
    assert_eq!(rows[0]["stale_pages"], json!(["wiki/concepts/login.md"]));
    assert_eq!(rows[1]["subject"], "ui.search");
    assert_eq!(rows[1]["stale_pages"], json!(["wiki/concepts/search.md"]));
}
