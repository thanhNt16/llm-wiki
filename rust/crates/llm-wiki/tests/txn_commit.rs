mod common;

use llm_wiki::err::EngineError;
use llm_wiki::txn::lock::Lock;
use llm_wiki::txn::stamps::Stamp;
use llm_wiki::txn::Txn;
use serde_json::json;

#[test]
fn commit_applies_and_bumps_revision() {
    let (_d, wiki) = common::wiki();
    let mut txn = Txn::begin(&wiki, "run_t1").unwrap();
    txn.stage_write("claims/c1.json", br#"{"id":"claim_01"}"#)
        .unwrap();
    let receipt = txn.commit().unwrap();
    assert!(wiki.p("claims/c1.json").is_file());
    assert_eq!(wiki.revision().unwrap(), 1);
    assert_eq!(receipt["revision"]["before"], 0);
    assert_eq!(receipt["revision"]["after"], 1);
    assert_eq!(receipt["run_id"], "run_t1");
    // receipt file + operations line written; breadcrumbs cleared
    assert!(wiki.p(".state/operations/run_t1.receipt.json").is_file());
    let ops = std::fs::read_to_string(wiki.p(".state/operations.jsonl")).unwrap();
    assert!(ops.contains("\"run_t1\""));
    assert!(!wiki.p(".state/journal.json").exists());
    assert!(!wiki.p(".state/.backup-0").exists());
    assert!(!wiki.p(".state/staging/run_t1").exists());
}

#[test]
fn overlay_reads_see_staged_writes() {
    let (_d, wiki) = common::wiki();
    let mut txn = Txn::begin(&wiki, "run_t2").unwrap();
    txn.stage_state(".state/foo.json", &json!({"a": 1})).unwrap();
    let v = txn.read_json(".state/foo.json").unwrap().unwrap();
    assert_eq!(v["a"], 1);
    // disk untouched pre-commit
    assert!(!wiki.p(".state/foo.json").exists());
    txn.commit().unwrap();
    assert_eq!(wiki.load_json(".state/foo.json").unwrap()["a"], 1);
}

#[test]
fn stamp_conflict_blocks_commit() {
    let (_d, wiki) = common::wiki();
    std::fs::write(wiki.p("claims/c1.json"), b"v1-0000").unwrap();
    let stamp = Stamp::capture(&wiki.p("claims/c1.json")).unwrap().unwrap();
    let mut txn = Txn::begin(&wiki, "run_t3").unwrap();
    txn.expect("claims/c1.json").unwrap();
    // external process rewrites it mid-txn (same size — mtime/inode catches it)
    common::rewrite_until_stamp_changes(&wiki.p("claims/c1.json"), &stamp, 7);
    match txn.commit() {
        Err(EngineError::Conflict(_)) => {}
        Err(e) => panic!("expected Conflict, got {e:?}"),
        Ok(_) => panic!("commit unexpectedly succeeded"),
    }
    assert_eq!(wiki.revision().unwrap(), 0);
}

#[test]
fn expect_absent_conflict() {
    let (_d, wiki) = common::wiki();
    let mut txn = Txn::begin(&wiki, "run_t4").unwrap();
    // destination does not exist → expect-absent registered
    txn.stage_write("claims/new.json", b"{}").unwrap();
    // external create before commit → Conflict
    std::fs::write(wiki.p("claims/new.json"), b"{}").unwrap();
    match txn.commit() {
        Err(e) => assert_eq!(e.exit_code(), 4),
        Ok(_) => panic!("commit unexpectedly succeeded"),
    }
    assert_eq!(wiki.revision().unwrap(), 0);
}

#[test]
fn second_concurrent_commit_locked() {
    let (_d, wiki) = common::wiki();
    // txn A's critical section in flight — commit.lock held.
    let _held = Lock::acquire(&wiki).unwrap();
    let txn = Txn::begin(&wiki, "run_b").unwrap();
    match txn.commit() {
        Err(EngineError::Locked) => {}
        Err(e) => panic!("expected Locked (exit 5), got {e:?}"),
        Ok(_) => panic!("commit unexpectedly succeeded under a held lock"),
    }
}

#[test]
fn stale_base_revision_conflicts() {
    let (_d, wiki) = common::wiki();
    // txn begins at rev 0, then another commit bumps the wiki first
    let stale = Txn::begin(&wiki, "run_c2").unwrap();
    let mut first = Txn::begin(&wiki, "run_c1").unwrap();
    first.stage_write("claims/a.json", b"{}").unwrap();
    first.commit().unwrap();
    assert_eq!(wiki.revision().unwrap(), 1);
    match stale.commit() {
        Err(e) => assert_eq!(e.exit_code(), 4),
        Ok(_) => panic!("stale base_revision committed"),
    }
}

#[test]
fn stage_delete_applies_at_commit() {
    let (_d, wiki) = common::wiki();
    std::fs::write(wiki.p("claims/gone.json"), b"{}").unwrap();
    let mut txn = Txn::begin(&wiki, "run_d1").unwrap();
    txn.stage_delete("claims/gone.json").unwrap();
    assert!(wiki.p("claims/gone.json").exists()); // untouched pre-commit
    assert!(txn.read("claims/gone.json").unwrap().is_none()); // overlay sees delete
    txn.commit().unwrap();
    assert!(!wiki.p("claims/gone.json").exists());
}
