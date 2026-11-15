mod common;

use llm_wiki::err::EngineError;
use llm_wiki::txn::Txn;

/// Plant `wiki/concepts` as a symlink to an outside dir (it exists post-init
/// as a real dir, so remove it first).
fn plant_symlinked_dir(wiki: &llm_wiki::store::Wiki) {
    let outside = wiki.root.join("outside-target");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::remove_dir(wiki.p("wiki/concepts")).unwrap();
    std::os::unix::fs::symlink(&outside, wiki.p("wiki/concepts")).unwrap();
}

#[cfg(unix)]
#[test]
fn symlinked_dir_rejected_at_stage_and_commit() {
    let (_d, wiki) = common::wiki();
    plant_symlinked_dir(&wiki);
    let mut txn = Txn::begin(&wiki, "run_s1").unwrap();
    match txn.stage_write("wiki/concepts/c1.md", b"# evil") {
        Err(EngineError::Txn(_)) => {}
        Err(e) => panic!("expected Txn error, got {e:?}"),
        Ok(()) => panic!("stage_write followed a symlinked component"),
    }
    // nothing written through the link
    assert!(!wiki.root.join("outside-target/c1.md").exists());
    assert!(!wiki.p("wiki/concepts/c1.md").exists());
}

#[cfg(unix)]
#[test]
fn commit_refuses_symlinked_path() {
    let (_d, wiki) = common::wiki();
    let mut txn = Txn::begin(&wiki, "run_s2").unwrap();
    txn.stage_write("wiki/concepts/c1.md", b"# ok").unwrap();
    // symlink planted AFTER staging — commit must refuse under the lock
    plant_symlinked_dir(&wiki);
    match txn.commit() {
        Err(EngineError::Txn(_)) => {}
        Err(e) => panic!("expected Txn error, got {e:?}"),
        Ok(_) => panic!("commit followed a symlinked component"),
    }
    assert!(!wiki.root.join("outside-target/c1.md").exists());
    assert_eq!(wiki.revision().unwrap(), 0);
}

#[cfg(unix)]
#[test]
fn expect_and_delete_reject_symlink() {
    let (_d, wiki) = common::wiki();
    plant_symlinked_dir(&wiki);
    let mut txn = Txn::begin(&wiki, "run_s3").unwrap();
    assert!(txn.expect("wiki/concepts/x.md").is_err());
    assert!(txn.stage_delete("wiki/concepts/x.md").is_err());
}

#[cfg(unix)]
#[test]
fn symlinked_leaf_rejected() {
    let (_d, wiki) = common::wiki();
    let real = wiki.root.join("real.txt");
    std::fs::write(&real, b"r").unwrap();
    std::os::unix::fs::symlink(&real, wiki.p("claims/link.json")).unwrap();
    let mut txn = Txn::begin(&wiki, "run_s4").unwrap();
    match txn.stage_write("claims/link.json", b"{}") {
        Err(EngineError::Txn(_)) => {}
        other => panic!("expected Txn error, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn parent_escape_rejected() {
    let (_d, wiki) = common::wiki();
    let mut txn = Txn::begin(&wiki, "run_s5").unwrap();
    match txn.stage_write("../escape.txt", b"x") {
        Err(EngineError::Txn(_)) => {}
        other => panic!("expected Txn error for .. escape, got {other:?}"),
    }
    assert!(!wiki.root.join("escape.txt").exists());
}
