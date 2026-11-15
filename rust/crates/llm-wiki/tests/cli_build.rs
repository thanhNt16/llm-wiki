//! `build` umbrella command (P3 unit E1) — compile until agent input is
//! required → build-pages → verify, with honest blocking (wiki.py
//! cmd_build parity): `{phase:"blocked",step:"extract"|"classify",…}`
//! exits 3; `{phase:"done"}` exits 0 iff verify.ok.

mod common;

use std::path::Path;
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

fn init(dir: &Path) {
    let out = bin().arg("init").arg(dir).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "init: {:?}", out);
}

fn ingest(dir: &Path, text: &str, text_ref: &str) -> String {
    let r = dir.to_str().unwrap();
    let (code, v) = run_json(&[
        "--root", r, "ingest", "--text", text, "--text-ref", text_ref,
    ]);
    assert_eq!(code, 0, "ingest {text_ref}: {v}");
    v["source_id"].as_str().unwrap().to_string()
}

/// Drive one source through build until blocked-extract, then return the
/// pending entry (asserting the exit-3 blocked payload contract).
fn blocked_extract_pending(dir: &Path) -> Value {
    let r = dir.to_str().unwrap();
    let (code, v) = run_json(&["--root", r, "build"]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["phase"], json!("blocked"), "{v}");
    assert_eq!(v["step"], json!("extract"), "{v}");
    assert_eq!(
        v["next"],
        json!("write shorthand candidates to each candidates_path \
              (wiki-compile), then re-run build"),
        "{v}"
    );
    let pending = v["pending"].as_array().unwrap();
    assert_eq!(pending.len(), 1, "{v}");
    pending[0].clone()
}

// ---------------------------------------------------------------------------
// fresh wiki, no sources: resume finds nothing staged → waiting →
// build-pages + verify → done, exit 0 (wiki.py:192-196)
// ---------------------------------------------------------------------------

#[test]
fn build_fresh_wiki_waiting_done_exit_0() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let r = dir.path().to_str().unwrap();

    let (code, v) = run_json(&["--root", r, "build"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["phase"], json!("done"), "{v}");
    assert_eq!(v["compile"]["phase"], json!("waiting"), "{v}");
    assert!(
        v["pages"].is_object() && v["pages"]["note"].is_null(),
        "waiting path rebuilds pages itself: {v}"
    );
    assert_eq!(v["verify"]["ok"], json!(true), "{v}");
}

// ---------------------------------------------------------------------------
// uncompiled source, no candidates written → blocked-extract, exit 3
// (wiki.py:178-184)
// ---------------------------------------------------------------------------

#[test]
fn build_blocked_extract_exit_3() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let sid = ingest(dir.path(), "alpha doc body", "doc-a");

    let pending = blocked_extract_pending(dir.path());
    assert_eq!(pending["source_id"], json!(sid));
    let cand = pending["candidates_path"].as_str().unwrap();
    assert!(!Path::new(cand).is_file(), "unstaged: {cand}");
    assert!(pending["run_id"].as_str().unwrap().starts_with("run_"));
}

// ---------------------------------------------------------------------------
// candidates staged into the pending run dir → resume applies all-auto →
// phase "done" with pages rebuilt during apply, exit 0 (wiki.py:197-200)
// ---------------------------------------------------------------------------

#[test]
fn build_candidates_applied_done_exit_0() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let r = dir.path().to_str().unwrap();
    ingest(dir.path(), "alpha doc body", "doc-a");

    let pending = blocked_extract_pending(dir.path());
    std::fs::write(
        pending["candidates_path"].as_str().unwrap(),
        "{\"subject\":\"alpha.beta\",\"predicate\":\"status\",\"value\":\"active\"}\n",
    )
    .unwrap();
    std::fs::write(
        pending["topics_path"].as_str().unwrap(),
        "{\"subject\":\"alpha.beta\",\"kind\":\"concept\",\"title\":\"Alpha\"}\n",
    )
    .unwrap();

    let (code, v) = run_json(&["--root", r, "build"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["phase"], json!("done"), "{v}");
    assert_eq!(v["compile"]["phase"], json!("done"), "{v}");
    assert_eq!(v["compile"]["changes"]["claims_created"], json!(1), "{v}");
    assert_eq!(
        v["pages"],
        json!({"note": "rebuilt during apply"}),
        "{v}"
    );
    assert_eq!(v["verify"]["ok"], json!(true), "{v}");

    // claim landed; source version marked compiled → plan empty
    let claims_dir = dir.path().join(".llm-wiki/claims");
    assert_eq!(std::fs::read_dir(&claims_dir).unwrap().count(), 1);
    let (code, plan) = run_json(&["--root", r, "compile-plan"]);
    assert_eq!(code, 0, "{plan}");
    assert_eq!(plan["pending"], json!([]), "{plan}");
}

// ---------------------------------------------------------------------------
// matched claim needing review → blocked-classify, exit 3
// (wiki.py:186-191); classifications.json then unblocks to done.
// ---------------------------------------------------------------------------

#[test]
fn build_blocked_classify_then_done() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let r = dir.path().to_str().unwrap();

    // cycle 1: apply alpha.beta/status=active
    ingest(dir.path(), "doc one", "d1");
    let pending = blocked_extract_pending(dir.path());
    std::fs::write(
        pending["candidates_path"].as_str().unwrap(),
        "{\"subject\":\"alpha.beta\",\"predicate\":\"status\",\"value\":\"active\"}\n",
    )
    .unwrap();
    let (code, v) = run_json(&["--root", r, "build"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["compile"]["changes"]["claims_created"], json!(1), "{v}");

    // cycle 2: same key, different value → match with auto:null
    ingest(dir.path(), "doc two", "d2");
    let pending = blocked_extract_pending(dir.path());
    let run2 = pending["run_id"].as_str().unwrap().to_string();
    std::fs::write(
        pending["candidates_path"].as_str().unwrap(),
        "{\"subject\":\"alpha.beta\",\"predicate\":\"status\",\"value\":\"retired\"}\n",
    )
    .unwrap();

    let (code, v) = run_json(&["--root", r, "build"]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["phase"], json!("blocked"), "{v}");
    assert_eq!(v["step"], json!("classify"), "{v}");
    assert_eq!(
        v["next"],
        json!("write classifications.json for the flagged runs \
              (wiki-review), then re-run build"),
        "{v}"
    );
    let review = v["needs_review"].as_array().unwrap();
    assert_eq!(review.len(), 1, "{v}");
    assert_eq!(review[0]["run_id"], json!(run2), "{v}");
    let cid = review[0]["matches"][0]["claim_id"]
        .as_str()
        .unwrap()
        .to_string();

    // agent resolves the flagged row → classifications.json unblocks
    let cls_path = Path::new(pending["classifications_path"].as_str().unwrap());
    std::fs::write(
        cls_path,
        serde_json::to_string(&json!([{
            "index": 0,
            "relationship": "CORRECTION",
            "target_claim_id": cid,
        }]))
        .unwrap(),
    )
    .unwrap();
    let (code, v) = run_json(&["--root", r, "build"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["phase"], json!("done"), "{v}");
    assert_eq!(v["compile"]["phase"], json!("done"), "{v}");
    assert_eq!(v["verify"]["ok"], json!(true), "{v}");
}

// ---------------------------------------------------------------------------
// `build` is a _MUTATING_COMMANDS member → --wait-lock is accepted
// ---------------------------------------------------------------------------

#[test]
fn build_accepts_wait_lock() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let r = dir.path().to_str().unwrap();
    let (code, v) = run_json(&["--root", r, "--wait-lock", "build"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["phase"], json!("done"), "{v}");
}
