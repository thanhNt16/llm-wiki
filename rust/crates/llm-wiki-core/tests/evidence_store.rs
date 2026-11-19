//! Evidence-store core contract — ports of `wikicore/store.py` +
//! `transaction.py`: init scaffold idempotency, operations-log append with
//! torn-tail repair, JSONL/manifest/config readback, and optimistic
//! base_revision conflict rejection.

use std::fs;

use llm_wiki_core::err::EngineError;
use llm_wiki_core::store::{init_wiki, Wiki};
use llm_wiki_core::txn::{append_operations_line, Txn};
use serde_json::{json, Value};

fn fresh_wiki() -> (tempfile::TempDir, Wiki) {
    let dir = tempfile::tempdir().unwrap();
    let wiki = Wiki::open(dir.path()).unwrap();
    init_wiki(&wiki, "proj").unwrap();
    (dir, wiki)
}

// ---- init ------------------------------------------------------------

#[test]
fn init_scaffolds_layout_then_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    let wiki = Wiki::open(dir.path()).unwrap();
    let out = init_wiki(&wiki, "proj").unwrap();
    assert_eq!(out["initialized"], json!(true));
    assert_eq!(out["revision"], json!(0));

    // Layout: sentinel config, manifest, README, canonical + derived dirs.
    assert!(wiki.p("wiki.json").is_file());
    assert!(wiki.p("README.md").is_file());
    assert!(wiki.p(".state/manifest.json").is_file());
    for d in [
        "sources", "claims", "decisions", ".state/locks", ".state/operations",
        ".state/staging", ".state/operations.jsonl",
    ] {
        assert!(wiki.p(d).exists(), "missing scaffold entry: {}", d);
    }
    assert!(wiki.p("wiki.json").is_file());
    let cfg = wiki.config().unwrap();
    assert_eq!(cfg["project"], json!("proj"));
    // defaults merged under stored config
    assert_eq!(cfg["secret_policy"], json!("warn"));
    assert_eq!(cfg["acceptance_policy"]["corroboration_auto_accept"], json!(2));

    let manifest = wiki.state().unwrap();
    assert_eq!(manifest["revision"], json!(0));
    assert_eq!(manifest["content_hash"], json!(""));

    // Idempotent second init: no-op result, state untouched.
    let again = init_wiki(&wiki, "other").unwrap();
    assert_eq!(again["initialized"], json!(false));
    assert_eq!(again["revision"], json!(0));
    assert_eq!(wiki.config().unwrap()["project"], json!("proj"));
}

#[test]
fn init_completes_partial_scaffold_after_crash() {
    let (dir, wiki) = fresh_wiki();
    // Simulate a crash BEFORE the sentinel write: scaffold exists but
    // wiki.json (and the manifest) are gone. Re-init must complete cleanly,
    // keeping a pre-existing user file (page-kinds.json is guarded).
    fs::write(wiki.p("page-kinds.json"), b"{\"rules\": [], \"default\": \"procedures\"}\n").unwrap();
    fs::remove_file(wiki.p("wiki.json")).unwrap();
    fs::remove_file(wiki.p(".state/manifest.json")).unwrap();
    drop(wiki);
    let wiki = Wiki::open(dir.path()).unwrap();
    let out = init_wiki(&wiki, "proj").unwrap();
    assert_eq!(out["initialized"], json!(true));
    assert!(wiki.exists());
    let kinds: Value =
        serde_json::from_str(&fs::read_to_string(wiki.p("page-kinds.json")).unwrap()).unwrap();
    assert_eq!(kinds["default"], json!("procedures"), "pre-existing file kept");
}

// ---- append ----------------------------------------------------------

#[test]
fn append_operations_line_repairs_torn_tail() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("operations.jsonl");

    // Missing file → created.
    append_operations_line(&path, r#"{"n": 1}"#).unwrap();
    // Missing trailing newline + blank lines are tolerated.
    let mut seeded = String::new();
    seeded.push_str("{\"n\": 1}\n"); // existing line
    seeded.push_str("\n\n"); // trailing blank lines
    seeded.push_str("{\"n\": 2"); // torn tail (crash mid-append)
    fs::write(&path, &seeded).unwrap();

    append_operations_line(&path, r#"{"n": 3}"#).unwrap();

    let wiki = Wiki::open(dir.path()).unwrap();
    fs::create_dir_all(wiki.p(".state")).unwrap();
    fs::rename(&path, wiki.p(".state/operations.jsonl")).unwrap();
    let lines = wiki.read_jsonl(".state/operations.jsonl").unwrap();
    let nums: Vec<i64> = lines
        .iter()
        .map(|v| v["n"].as_i64().unwrap())
        .collect();
    assert_eq!(nums, vec![1, 3], "torn tail dropped, blank lines collapsed");
    let raw = fs::read_to_string(wiki.p(".state/operations.jsonl")).unwrap();
    assert!(raw.ends_with("\n"));
}

#[test]
fn commit_appends_receipt_line_and_clears_breadcrumbs() {
    let (_dir, wiki) = fresh_wiki();
    let run = {
        let mut txn = Txn::begin(&wiki, "run_a").unwrap();
        txn.stage_write("claims/claim_1.json", b"{}\n").unwrap();
        txn.commit().unwrap()
    };
    assert_eq!(run["revision"]["before"], json!(0));
    assert_eq!(run["revision"]["after"], json!(1));

    let lines = wiki.read_jsonl(".state/operations.jsonl").unwrap();
    assert_eq!(lines.len(), 1, "one canonical receipt line appended");
    assert_eq!(lines[0]["run_id"], json!("run_a"));
    assert!(!wiki.p(".state/journal.json").exists(), "journal cleared");
    assert!(!wiki.p(".state/journal-run_a.json").exists());
    assert!(!wiki.p(".state/staging/run_a").exists(), "staging cleared");
    assert!(wiki.p(".state/operations/run_a.receipt.json").is_file());
}

// ---- readback --------------------------------------------------------

#[test]
fn read_jsonl_readback_contract() {
    let dir = tempfile::tempdir().unwrap();
    let wiki = Wiki::open(dir.path()).unwrap();
    fs::create_dir_all(wiki.p(".state")).unwrap();

    // missing file → empty
    assert!(wiki.read_jsonl(".state/operations.jsonl").unwrap().is_empty());

    // a corrupt line fails unless it is the LAST non-blank one
    fs::write(
        wiki.p(".state/operations.jsonl"),
        "{\"ok\": 1}\n{\"corrupt\"\n\n",
    )
    .unwrap();
    let lines = wiki.read_jsonl(".state/operations.jsonl").unwrap();
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0]["ok"], json!(1));

    fs::write(
        wiki.p(".state/operations.jsonl"),
        "{\"ok\": 1}\n{\"corrupt\"\n{\"ok\": 2}\n",
    )
    .unwrap();
    assert!(
        wiki.read_jsonl(".state/operations.jsonl").is_err(),
        "corrupt non-trailing line is an error"
    );
}

#[test]
fn state_and_config_roundtrip() {
    let (_dir, wiki) = fresh_wiki();
    wiki.save_state(&json!({"revision": 7, "content_hash": "abc"})).unwrap();
    let state = wiki.state().unwrap();
    assert_eq!(state["revision"], json!(7));
    assert_eq!(wiki.revision().unwrap(), 7);
    assert_eq!(wiki.state().unwrap()["content_hash"], json!("abc"));

    wiki.save_config(&json!({
        "project": "proj",
        "secret_policy": "strict",
        "acceptance_policy": {"corroboration_auto_accept": 3}
    }))
    .unwrap();
    let cfg = wiki.config().unwrap();
    assert_eq!(cfg["secret_policy"], json!("strict"), "stored value wins");
    assert_eq!(cfg["context_budget_default"], json!(6000), "default kept");
    assert_eq!(
        cfg["acceptance_policy"]["corroboration_auto_accept"], json!(3),
        "nested acceptance_policy merged"
    );
    assert_eq!(
        cfg["acceptance_policy"]["auto_accept_authority"].as_array().unwrap().len(),
        3,
        "nested default kept"
    );
}

// ---- revision conflict ------------------------------------------------

#[test]
fn stale_base_revision_conflicts_and_does_not_apply() {
    let (_dir, wiki) = fresh_wiki();

    // rev 0 → 1
    let mut first = Txn::begin(&wiki, "run_1").unwrap();
    first.stage_write("claims/claim_1.json", b"{}\n").unwrap();
    first.commit().unwrap();
    assert_eq!(wiki.revision().unwrap(), 1);

    // A txn begun at rev 1 must fail once another writer moves the wiki to
    // rev 2 — optimistic concurrency, nothing applied.
    let mut stale = Txn::begin(&wiki, "run_2").unwrap();
    stale.stage_write("claims/claim_2.json", b"{}\n").unwrap();

    let mut concurrent = Txn::begin(&wiki, "run_3").unwrap();
    concurrent.stage_write("claims/claim_3.json", b"{}\n").unwrap();
    concurrent.commit().unwrap();
    assert_eq!(wiki.revision().unwrap(), 2);

    match stale.commit() {
        Err(EngineError::Conflict(msg)) => {
            assert!(
                msg.contains("base_revision=1") && msg.contains("wiki is at 2"),
                "conflict names both revisions: {msg}"
            );
        }
        other => panic!("expected ConflictError, got {other:?}"),
    }
    assert!(!wiki.p("claims/claim_2.json").exists(), "stale write not applied");
    assert_eq!(wiki.revision().unwrap(), 2, "revision untouched by failed commit");

    // Rebased txn (fresh read) commits cleanly on top.
    let mut rebased = Txn::begin(&wiki, "run_4").unwrap();
    rebased.stage_write("claims/claim_2.json", b"{}\n").unwrap();
    rebased.commit().unwrap();
    assert_eq!(wiki.revision().unwrap(), 3);
    assert!(wiki.p("claims/claim_2.json").is_file());
}

#[test]
fn begin_without_init_errors_instead_of_panicking() {
    let dir = tempfile::tempdir().unwrap();
    let wiki = Wiki::open(dir.path()).unwrap();
    assert!(matches!(
        Txn::begin(&wiki, "run_x"),
        Err(EngineError::Txn(_))
    ));
}
