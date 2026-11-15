mod common;

use llm_wiki::err::EngineError;
use llm_wiki::txn::lock::Lock;

#[test]
fn lock_excludes_second_holder() {
    let (_d, wiki) = common::wiki();
    let _a = Lock::acquire(&wiki).unwrap();
    match Lock::acquire(&wiki) {
        Err(EngineError::Locked) => {}
        Err(e) => panic!("expected Locked, got {e:?}"),
        Ok(_) => panic!("second acquire unexpectedly succeeded"),
    }
}

#[test]
fn lock_released_on_drop() {
    let (_d, wiki) = common::wiki();
    {
        let _a = Lock::acquire(&wiki).unwrap();
    }
    let _b = Lock::acquire(&wiki).unwrap();
}

#[test]
fn lock_file_created_and_never_deleted() {
    let (_d, wiki) = common::wiki();
    {
        let _a = Lock::acquire(&wiki).unwrap();
    }
    // §5.6: commit.lock persists after release — kernel flock cannot be stale.
    assert!(wiki.p(".state/locks/commit.lock").is_file());
}
