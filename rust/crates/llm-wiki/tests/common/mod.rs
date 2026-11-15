//! Shared test fixtures — not a test target (subdir of tests/).

use std::path::Path;

use llm_wiki::store::{init_wiki, Wiki};
use tempfile::TempDir;

/// Fresh initialized wiki in a tempdir — `(dir, wiki)`, keep `dir` alive.
#[allow(dead_code)]
pub fn wiki() -> (TempDir, Wiki) {
    let dir = tempfile::tempdir().unwrap();
    let wiki = Wiki::open(dir.path()).unwrap();
    init_wiki(&wiki, "t").unwrap();
    (dir, wiki)
}

/// Rewrite `path` with same-length bytes until the captured stamp's tuple
/// differs (mtime granularity-proof external-rewrite simulation).
#[allow(dead_code)]
pub fn rewrite_until_stamp_changes(path: &Path, stamp: &llm_wiki::txn::stamps::Stamp, len: usize) {
    for _ in 0..300 {
        std::fs::write(path, vec![b'b'; len]).unwrap();
        if !stamp.matches(path).unwrap() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(15));
    }
    panic!("rewrite never moved the stamp tuple");
}
