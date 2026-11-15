mod common;
use llm_wiki::err::EngineError;
use llm_wiki::store::Wiki;
use llm_wiki::txn::recovery;
use llm_wiki::txn::{Txn, TxnOpts};
use serde_json::{json, Value};

fn plant_journal(wiki: &Wiki, run: &str, ops: Value) {
    let payload = serde_json::to_string_pretty(&json!({
        "receipt": {"run_id": run, "revision": {"before": 0, "after": 1}},
        "ops": ops,
    }))
    .unwrap()
        + "\n";
    std::fs::write(wiki.p(".state/journal.json"), &payload).unwrap();
    std::fs::write(
        wiki.p(&format!(".state/journal-{run}.json")),
        &payload,
    )
    .unwrap();
}

#[test]
fn leftover_journal_blocks_commit_with_repair_required() {
    let (_d, wiki) = common::wiki();
    plant_journal(&wiki, "run_x", json!([]));
    let txn = Txn::begin(&wiki, "run_y").unwrap();
    match txn.commit() {
        Err(EngineError::Repair(_)) => {}
        Err(e) => panic!("expected Repair, got {e:?}"),
        Ok(_) => panic!("commit bypassed the repair gate"),
    }
    // still locked out; nothing applied
    assert_eq!(wiki.revision().unwrap(), 0);
}

#[test]
fn leftover_journal_error_type_and_exit() {
    let (_d, wiki) = common::wiki();
    plant_journal(&wiki, "run_x", json!([]));
    let txn = Txn::begin(&wiki, "run_y").unwrap();
    let e = txn.commit().unwrap_err();
    assert_eq!(e.exit_code(), 3);
    assert_eq!(e.error_type(), "repair_required");
}

#[test]
fn allow_repair_bypasses_gate() {
    let (_d, wiki) = common::wiki();
    plant_journal(&wiki, "run_x", json!([]));
    let mut txn = Txn::begin_with(
        &wiki,
        "run_z",
        TxnOpts { allow_repair: true, command: None },
    )
    .unwrap();
    txn.stage_write("claims/ok.json", b"{}").unwrap();
    let receipt = txn.commit().unwrap();
    assert_eq!(receipt["revision"]["after"], 1);
    assert!(wiki.p("claims/ok.json").is_file());
    assert!(!wiki.p(".state/journal.json").exists());
}

#[test]
fn crash_mid_apply_restored_by_repair() {
    let (_d, wiki) = common::wiki();
    std::fs::write(wiki.p("claims/c1.json"), b"old-bytes").unwrap();
    // crash mid-apply: backup has the original, dst was already overwritten
    let bdir = wiki.p(".state/.backup-0");
    std::fs::create_dir_all(bdir.join("claims")).unwrap();
    std::fs::write(bdir.join("claims/c1.json"), b"old-bytes").unwrap();
    std::fs::write(wiki.p("claims/c1.json"), b"new-bytes").unwrap();
    plant_journal(&wiki, "run_crash", json!([["write", "claims/c1.json"]]));

    let report = recovery::repair(&wiki).unwrap();
    assert_eq!(report["repaired"], true);
    assert_eq!(
        std::fs::read(wiki.p("claims/c1.json")).unwrap(),
        b"old-bytes"
    );
    assert!(!wiki.p(".state/journal.json").exists());
    assert!(!wiki.p(".state/journal-run_crash.json").exists());
    assert!(!bdir.exists());
    assert!(!recovery::repair_needed(&wiki));
}

#[test]
fn journal_without_backup_means_nothing_applied() {
    let (_d, wiki) = common::wiki();
    // crash between journal write and backup completion — nothing applied
    plant_journal(&wiki, "run_j", json!([["write", "claims/x.json"]]));
    std::fs::create_dir_all(wiki.p(".state/staging/run_j")).unwrap();
    // a dst file that coincidentally matches the journaled write must NOT
    // be unlinked — the journal-only branch never touches destinations.
    std::fs::write(wiki.p("claims/x.json"), b"keep").unwrap();

    recovery::repair(&wiki).unwrap();
    assert_eq!(
        std::fs::read(wiki.p("claims/x.json")).unwrap(),
        b"keep"
    );
    assert!(!wiki.p(".state/journal.json").exists());
    assert!(!wiki.p(".state/staging/run_j").exists());
}

#[test]
fn multiple_backups_restore_in_revision_order() {
    let (_d, wiki) = common::wiki();
    // .backup-3 and .backup-5 → restore 3 then 5; highest rev wins per file
    for (rev, bytes) in [(3, b"rev3"), (5, b"rev5")] {
        let d = wiki.p(&format!(".state/.backup-{rev}"));
        std::fs::create_dir_all(d.join("claims")).unwrap();
        std::fs::write(d.join("claims/c.json"), &bytes[..]).unwrap();
    }
    plant_journal(&wiki, "run_mb", json!([]));
    recovery::repair(&wiki).unwrap();
    assert_eq!(std::fs::read(wiki.p("claims/c.json")).unwrap(), b"rev5");
    assert!(!wiki.p(".state/.backup-3").exists());
    assert!(!wiki.p(".state/.backup-5").exists());
}

#[test]
fn newly_written_destination_unlinked_by_repair() {
    // crash mid-apply: backup holds files that existed; a journaled write
    // whose dst had no prior file must be removed, not left half-applied.
    let (_d, wiki) = common::wiki();
    std::fs::write(wiki.p("claims/existed.json"), b"orig").unwrap();
    let bdir = wiki.p(".state/.backup-0");
    std::fs::create_dir_all(bdir.join("claims")).unwrap();
    std::fs::write(bdir.join("claims/existed.json"), b"orig").unwrap();
    // crash left the new file behind with no backup entry for it
    std::fs::write(wiki.p("claims/fresh.json"), b"half-applied").unwrap();
    plant_journal(
        &wiki,
        "run_nw",
        json!([["write", "claims/existed.json"], ["write", "claims/fresh.json"]]),
    );
    recovery::repair(&wiki).unwrap();
    assert!(!wiki.p("claims/fresh.json").exists());
    assert_eq!(std::fs::read(wiki.p("claims/existed.json")).unwrap(), b"orig");
}

#[test]
fn preexisting_backup_dir_renamed_not_merged() {
    let (_d, wiki) = common::wiki();
    // retry after crash under --allow-repair: an existing .backup-<cur> is
    // renamed .backup-<cur>.prev-<ts> before the new backup is written.
    let old = wiki.p(".state/.backup-0");
    std::fs::create_dir_all(old.join("claims")).unwrap();
    std::fs::write(old.join("claims/old.json"), b"gen1").unwrap();
    std::fs::write(wiki.p("claims/c.json"), b"cur").unwrap();
    let mut txn = Txn::begin_with(
        &wiki,
        "run_prev",
        TxnOpts { allow_repair: true, command: None },
    )
    .unwrap();
    txn.stage_write("claims/c.json", b"v2").unwrap();
    txn.commit().unwrap();

    assert!(!old.exists(), "leftover .backup-0 not cleared or not renamed");
    let prev: Vec<_> = std::fs::read_dir(wiki.p(".state"))
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with(".backup-0.prev-"))
        .collect();
    assert_eq!(prev.len(), 1, "expected one .backup-0.prev-* dir, got {prev:?}");
    assert_eq!(std::fs::read(wiki.p("claims/c.json")).unwrap(), b"v2");
    // the renamed dir is a fresh repair artifact — the next commit gates on it
    assert!(recovery::repair_needed(&wiki));
    // and repair restores the OLD generation from it
    recovery::repair(&wiki).unwrap();
    assert_eq!(std::fs::read(wiki.p("claims/old.json")).unwrap(), b"gen1");
}

#[test]
fn staging_leftover_detected_but_not_gated() {
    let (_d, wiki) = common::wiki();
    std::fs::create_dir_all(wiki.p(".state/staging/run_dead")).unwrap();
    assert!(recovery::scan(&wiki)
        .iter()
        .any(|l| matches!(l, recovery::Leftover::Staging(_))));
    // staging alone is not a repair-required state — a parked run dir is legit
    assert!(!recovery::repair_needed(&wiki));
}
