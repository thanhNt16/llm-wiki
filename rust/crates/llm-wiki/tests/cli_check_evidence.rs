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
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    (code, v)
}

/// Ingest `data` as `kind=file` and return (source_id, raw_path, sha256).
fn ingest_file(dir: &std::path::Path, name: &str, data: &[u8]) -> (String, String, String) {
    let src = dir.join(name);
    std::fs::write(&src, data).unwrap();
    let (code, v) = run_json(&[
        "--root",
        dir.to_str().unwrap(),
        "ingest",
        "--file",
        src.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    (
        v["source_id"].as_str().unwrap().to_string(),
        v["raw_path"].as_str().unwrap().to_string(),
        v["sha256"].as_str().unwrap().to_string(),
    )
}

fn paths(list: &Value) -> Vec<String> {
    list.as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["path"].as_str().map(str::to_string))
        .collect()
}

#[test]
fn check_evidence_uninitialized_wiki() {
    let dir = tempfile::tempdir().unwrap();
    let (code, v) = run_json(&[
        "--root",
        dir.path().to_str().unwrap(),
        "check-evidence",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v, serde_json::json!({"initialized": false}));
}

#[test]
fn check_evidence_healthy_wiki_reports_needs_repair_false() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    ingest_file(dir.path(), "note.txt", b"hello evidence");
    let (code, v) = run_json(&[
        "--root",
        dir.path().to_str().unwrap(),
        "check-evidence",
        "--json",
        "--verify",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["needs_repair"], false, "{v}");
    assert_eq!(v["verified"], true, "{v}");
    assert_eq!(v["ok"], true, "{v}");
    assert!(v["checked"].as_i64().unwrap() >= 1, "{v}");
    assert_eq!(v["missing"], serde_json::json!([]));
    assert_eq!(v["orphans"], serde_json::json!([]));
    assert_eq!(v["hash_mismatches"], serde_json::json!([]));
    assert!(v["manifest_revision"].as_i64().unwrap() >= 1, "{v}");
}

#[test]
fn check_evidence_missing_manifest_flags_needs_repair() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let (sid, _, _) = ingest_file(dir.path(), "note.txt", b"hello evidence");
    std::fs::remove_file(
        dir.path()
            .join(format!(".llm-wiki/sources/{sid}/manifest.json")),
    )
    .unwrap();
    let (code, v) = run_json(&[
        "--root",
        dir.path().to_str().unwrap(),
        "check-evidence",
    ]);
    assert_eq!(code, 0, "{v}"); // repair state is data, not failure
    assert_eq!(v["needs_repair"], true, "{v}");
    let missing = paths(&v["missing"]);
    assert!(
        missing
            .iter()
            .any(|p| p == &format!("sources/{sid}/manifest.json")),
        "{v}"
    );
}

#[test]
fn check_evidence_missing_raw_lands_in_missing() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let (_, raw_rel, _) = ingest_file(dir.path(), "note.txt", b"hello evidence");
    std::fs::remove_file(dir.path().join(".llm-wiki").join(&raw_rel)).unwrap();
    let (code, v) = run_json(&[
        "--root",
        dir.path().to_str().unwrap(),
        "check-evidence",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["needs_repair"], true, "{v}");
    assert!(paths(&v["missing"]).contains(&raw_rel), "{v}");
}

#[test]
fn check_evidence_orphaned_raw_payload_flagged() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    ingest_file(dir.path(), "note.txt", b"hello evidence");
    let orphan = dir.path().join(".llm-wiki/raw/files/0123456789abcdef/junk.bin");
    std::fs::create_dir_all(orphan.parent().unwrap()).unwrap();
    std::fs::write(&orphan, b"unreferenced").unwrap();
    let (code, v) = run_json(&[
        "--root",
        dir.path().to_str().unwrap(),
        "check-evidence",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["needs_repair"], true, "{v}");
    assert!(
        paths(&v["orphans"]).contains(&"raw/files/0123456789abcdef/junk.bin".to_string()),
        "{v}"
    );
}

#[test]
fn check_evidence_verify_detects_corrupted_raw_sha256() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let (_, raw_rel, sha) = ingest_file(dir.path(), "note.txt", b"hello evidence");
    let raw = dir.path().join(".llm-wiki").join(&raw_rel);
    std::fs::write(&raw, b"tampered payload").unwrap();

    // Plain audit sees the file present: clean without the hash lane.
    let (code, v) = run_json(&[
        "--root",
        dir.path().to_str().unwrap(),
        "check-evidence",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["needs_repair"], false, "{v}");

    // --verify re-hashes and reports the mismatch.
    let (code, v) = run_json(&[
        "--root",
        dir.path().to_str().unwrap(),
        "check-evidence",
        "--verify",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["needs_repair"], true, "{v}");
    assert_eq!(v["verified"], false, "{v}");
    let mm = v["hash_mismatches"].as_array().unwrap();
    assert_eq!(mm.len(), 1, "{v}");
    assert_eq!(mm[0]["type"], "hash_mismatch");
    assert_eq!(mm[0]["error_type"], "hash_mismatch");
    assert_eq!(mm[0]["path"].as_str().unwrap(), raw_rel);
    assert_eq!(mm[0]["expected_sha256"].as_str().unwrap(), sha);
}

#[test]
fn check_evidence_details_caps_issues_list() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    for i in 0..3 {
        let p = dir
            .path()
            .join(format!(".llm-wiki/raw/files/aaaaaaaaaaaaaaa{i}/junk.bin"));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, b"x").unwrap();
    }
    let (code, v) = run_json(&[
        "--root",
        dir.path().to_str().unwrap(),
        "check-evidence",
        "--details",
        "2",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["orphans"].as_array().unwrap().len(), 3, "{v}");
    assert_eq!(v["issues"].as_array().unwrap().len(), 2, "{v}");
}

#[test]
fn verify_healthy_wiki_ok() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    ingest_file(dir.path(), "note.txt", b"hello evidence");
    let (code, v) = run_json(&["--root", dir.path().to_str().unwrap(), "verify"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["ok"], true, "{v}");
    let names: Vec<&str> = v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["name"].as_str().unwrap())
        .collect();
    for n in ["manifest_revision", "dep_edges", "claims_parse"] {
        assert!(names.contains(&n), "missing check {n}: {v}");
    }
    for c in v["checks"].as_array().unwrap() {
        assert_eq!(c["status"], "pass", "{v}");
    }
}

#[test]
fn verify_uninitialized_wiki() {
    let dir = tempfile::tempdir().unwrap();
    let (code, v) = run_json(&["--root", dir.path().to_str().unwrap(), "verify"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v, serde_json::json!({"initialized": false}));
}

#[test]
fn verify_corrupt_claim_fails_named_check_exit3() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    std::fs::write(
        dir.path().join(".llm-wiki/claims/claim_bad.json"),
        b"{not json",
    )
    .unwrap();
    let (code, v) = run_json(&["--root", dir.path().to_str().unwrap(), "verify"]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["ok"], false, "{v}");
    let cp = v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "claims_parse")
        .unwrap();
    assert_eq!(cp["status"], "fail", "{v}");
}

#[test]
fn verify_dead_dep_edge_fails_named_check_exit3() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    std::fs::write(
        dir.path().join(".llm-wiki/.state/dependencies.json"),
        serde_json::json!({
            "edges": {"wiki/concepts/x.md": ["claim_ghost@1"]},
            "stale": []
        })
        .to_string(),
    )
    .unwrap();
    let (code, v) = run_json(&["--root", dir.path().to_str().unwrap(), "verify"]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["ok"], false, "{v}");
    let de = v["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["name"] == "dep_edges")
        .unwrap();
    assert_eq!(de["status"], "fail", "{v}");
}
