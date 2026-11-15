mod common;

use std::fs;

use llm_wiki::evidence::cursors::{self, CURSORS_REL};
use llm_wiki::index::Index;
use llm_wiki::txn::Txn;

/// Cursor keys are project-root-relative; a dir outside the wiki root keys
/// on its absolute path.
fn cursor_keys(wiki: &llm_wiki::store::Wiki) -> Vec<String> {
    let v = wiki.load_json(CURSORS_REL).unwrap();
    let map = v.as_object().unwrap();
    map.keys().cloned().collect()
}

/// Full plan-13 lifecycle: new → changed+new → removed → clean rescan,
/// cursor JSON persisted each step, sqlite projection mirrored.
#[test]
fn triage_lifecycle() {
    let (_tmp, wiki) = common::wiki();
    let dir = _tmp.path().join("evidence");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("a.txt"), "alpha").unwrap();
    fs::write(dir.join("b.txt"), "beta").unwrap();
    fs::write(dir.join(".hidden"), "x").unwrap(); // skipped by default

    // Scan 1: everything new; cursor file persisted.
    let t1 = cursors::scan(&wiki, &dir).unwrap();
    assert_eq!(t1.new, ["a.txt", "b.txt"]);
    assert!(t1.changed.is_empty() && t1.removed.is_empty());
    let stored = wiki.load_json(CURSORS_REL).unwrap();
    let m = stored.as_object().unwrap();
    assert_eq!(m.len(), 2);
    for k in m.keys() {
        assert!(k.starts_with("evidence/"), "root-relative key: {k}");
    }
    for meta in m.values() {
        assert!(meta["mtime_ns"].is_number());
        assert!(meta["size"].is_number());
    }

    // Scan 2: modify A (size+mtime change), add C → changed/new.
    fs::write(dir.join("a.txt"), "alpha-REVISED").unwrap();
    fs::write(dir.join("c.txt"), "gamma").unwrap();
    let t2 = cursors::scan(&wiki, &dir).unwrap();
    assert_eq!(t2.changed, ["a.txt"]);
    assert_eq!(t2.new, ["c.txt"]);
    assert!(t2.removed.is_empty());

    // Scan 3: delete B → removed only.
    fs::remove_file(dir.join("b.txt")).unwrap();
    let t3 = cursors::scan(&wiki, &dir).unwrap();
    assert!(t3.changed.is_empty() && t3.new.is_empty());
    assert_eq!(t3.removed, ["b.txt"]);
    assert_eq!(cursor_keys(&wiki), ["evidence/a.txt", "evidence/c.txt"]);

    // Scan 4: nothing moved → clean.
    let t4 = cursors::scan(&wiki, &dir).unwrap();
    assert!(t4.changed.is_empty() && t4.new.is_empty() && t4.removed.is_empty());

    // Hidden file never entered the cursor map at any point.
    assert!(!wiki
        .load_json(CURSORS_REL)
        .unwrap()
        .to_string()
        .contains(".hidden"));
}

#[test]
fn sqlite_projection_mirrored() {
    let (_tmp, wiki) = common::wiki();
    let dir = _tmp.path().join("evidence");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("a.txt"), "alpha").unwrap();

    let t = cursors::scan(&wiki, &dir).unwrap();
    assert_eq!(t.new, ["a.txt"]);

    let idx = Index::open_or_rebuild(&wiki).unwrap();
    let row: (String, i64, i64) = idx
        .conn()
        .query_row(
            "SELECT path, mtime_ns, size FROM dir_cursors",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_eq!(row.0, "evidence/a.txt");
    assert_eq!(row.2, 5); // "alpha"
    assert!(row.1 > 0);

    // Removal propagates to the projection too.
    fs::remove_file(dir.join("a.txt")).unwrap();
    let t2 = cursors::scan(&wiki, &dir).unwrap();
    assert_eq!(t2.removed, ["a.txt"]);
    let n: i64 = idx
        .conn()
        .query_row("SELECT COUNT(*) FROM dir_cursors", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 0);
}

/// `.live` heartbeat markers and other txn bookkeeping inside `.state`
/// never surface as triage content.
#[test]
fn state_dir_walk_excludes_txn_transients() {
    let (_tmp, wiki) = common::wiki();
    let staging = wiki.p(".state/staging/run_xyz");
    fs::create_dir_all(&staging).unwrap();
    fs::write(staging.join(".live"), "").unwrap();
    fs::write(staging.join("probe.txt"), "p").unwrap();
    fs::write(wiki.p(".state/journal.json"), "{}").unwrap();
    fs::write(wiki.p(".state/user-state.json"), "{}").unwrap();

    // A real leftover journal trips the repair gate on commit — scan it
    // through a caller-owned txn (never committed) instead.
    let mut txn = Txn::begin(&wiki, "run_journal_scan").unwrap();
    let state_dir = wiki.p(".state");
    let t = cursors::scan_with_txn(&wiki, &state_dir, &mut txn).unwrap();
    let all: Vec<String> = [t.changed, t.new, t.removed].concat();
    assert!(all.iter().all(|p| !p.contains(".live")), "{all:?}");
    assert!(all.iter().all(|p| !p.starts_with("staging/")), "{all:?}");
    assert!(all.iter().all(|p| !p.starts_with("journal")), "{all:?}");
    assert!(all.iter().all(|p| !p.contains("dir-cursors")), "{all:?}");
    // Real state files DO surface.
    assert!(all.contains(&"user-state.json".to_string()), "{all:?}");
    // The cursor file itself must not appear.
    assert!(all.iter().all(|p| *p != "dir-cursors.json"), "{all:?}");
    drop(txn); // abort — nothing lands on disk
}

/// `scan_with_txn` composes inside one commit: two dirs merge into one
/// cursor map via the staged overlay, both land atomically.
#[test]
fn scan_with_txn_composes() {
    let (_tmp, wiki) = common::wiki();
    let d1 = _tmp.path().join("d1");
    let d2 = _tmp.path().join("d2");
    fs::create_dir_all(&d1).unwrap();
    fs::create_dir_all(&d2).unwrap();
    fs::write(d1.join("x.txt"), "x").unwrap();
    fs::write(d2.join("y.txt"), "y").unwrap();

    let mut txn = Txn::begin(&wiki, "run_cursors").unwrap();
    let t1 = cursors::scan_with_txn(&wiki, &d1, &mut txn).unwrap();
    let t2 = cursors::scan_with_txn(&wiki, &d2, &mut txn).unwrap();
    assert_eq!(t1.new, ["x.txt"]);
    assert_eq!(t2.new, ["y.txt"]);
    assert!(!wiki.p(CURSORS_REL).exists(), "nothing on disk before commit");
    txn.commit().unwrap();

    assert_eq!(cursor_keys(&wiki), ["d1/x.txt", "d2/y.txt"]);

    // A later plain scan sees both, unchanged.
    let t = cursors::scan(&wiki, &d1).unwrap();
    assert!(t.new.is_empty() && t.changed.is_empty() && t.removed.is_empty());
    let t = cursors::scan(&wiki, &d2).unwrap();
    assert!(t.new.is_empty() && t.changed.is_empty() && t.removed.is_empty());
}

/// Missing dir → empty walk: every prior key under that prefix is removed.
#[test]
fn missing_dir_reports_removed() {
    let (_tmp, wiki) = common::wiki();
    let dir = _tmp.path().join("gone");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("a.txt"), "a").unwrap();
    assert_eq!(cursors::scan(&wiki, &dir).unwrap().new, ["a.txt"]);

    fs::remove_dir_all(&dir).unwrap();
    let t = cursors::scan(&wiki, &dir).unwrap();
    assert_eq!(t.removed, ["a.txt"]);
    assert!(cursor_keys(&wiki).is_empty());
}

/// `include_hidden` opts dotfiles back in.
#[test]
fn include_hidden_opt() {
    let (_tmp, wiki) = common::wiki();
    let dir = _tmp.path().join("evidence");
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("a.txt"), "a").unwrap();
    fs::write(dir.join(".hidden"), "h").unwrap();

    let t = cursors::scan_opts(
        &wiki,
        &dir,
        &cursors::ScanOpts {
            include_hidden: true,
        },
        None,
    )
    .unwrap();
    assert_eq!(t.new, [".hidden", "a.txt"]);
}
