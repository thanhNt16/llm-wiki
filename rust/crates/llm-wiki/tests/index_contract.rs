use llm_wiki::index::Index;
use llm_wiki::store::{init_wiki, Wiki};
use llm_wiki::txn::Txn;
use serde_json::json;

fn new_wiki() -> (tempfile::TempDir, Wiki) {
    let dir = tempfile::tempdir().unwrap();
    let wiki = Wiki::open(dir.path()).unwrap();
    init_wiki(&wiki, "t").unwrap();
    (dir, wiki)
}

#[test]
fn index_created_with_schema_and_watermark() {
    let (_d, wiki) = new_wiki();
    let idx = Index::open_or_rebuild(&wiki).unwrap();
    assert!(wiki.p(".state/index.sqlite").is_file());
    let uv: i64 = idx
        .conn()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(uv, llm_wiki::index::SCHEMA_VERSION);
    // never built → negative watermark (stale vs revision 0 only when
    // something was built; readers fall back to canonical scan)
    assert!(idx.built_at_revision().unwrap() <= 0);
    // spec §10.1 tables exist
    for t in [
        "meta",
        "claims_fts",
        "claims_index",
        "dep_edges",
        "jev_answers",
        "dir_cursors",
    ] {
        let n: i64 = idx
            .conn()
            .query_row(
                "SELECT count(*) FROM sqlite_master WHERE name = ?1",
                [t],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(n, 1, "missing {t}");
    }
    let tok: String = idx
        .conn()
        .query_row(
            "SELECT sql FROM sqlite_master WHERE name = 'claims_fts'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert!(tok.contains("porter unicode61"), "{tok}");
    assert!(tok.contains("claim_id UNINDEXED"), "{tok}");
}

#[test]
fn claim_free_commit_on_unusable_index_stays_stale() {
    // ftx.health unusable (never built → built_at=-1 ≠ revision) →
    // transaction.py:325-328 takes neither apply_txn nor restamp; stamping
    // would lie about content the index does not hold.
    let (_d, wiki) = new_wiki();
    Index::open_or_rebuild(&wiki).unwrap();
    let mut txn = Txn::begin(&wiki, &llm_wiki::txn::mint_run_id()).unwrap();
    txn.stage_state(".state/aliases.json", &json!({"k": "v"})).unwrap();
    let receipt = txn.commit().unwrap();
    assert_eq!(receipt["revision"]["after"], 1);
    let idx = Index::open_or_rebuild(&wiki).unwrap();
    assert_eq!(idx.built_at_revision().unwrap(), -1);
    assert!(idx.is_stale(&wiki).unwrap());
}

#[test]
fn claim_free_commit_restamps_usable_index() {
    // usable (built_at == manifest rev) claim-free commit → restamp only.
    let (_d, wiki) = new_wiki();
    Index::rebuild_under_index_lock(&wiki, 0).unwrap();
    let mut txn = Txn::begin(&wiki, &llm_wiki::txn::mint_run_id()).unwrap();
    txn.stage_state(".state/aliases.json", &json!({"k": "v"})).unwrap();
    txn.commit().unwrap();
    let idx = Index::open_or_rebuild(&wiki).unwrap();
    assert_eq!(idx.built_at_revision().unwrap(), 1);
    assert!(!idx.is_stale(&wiki).unwrap());
}

#[test]
fn stale_flag_when_revision_behind() {
    let (_d, wiki) = new_wiki();
    let idx = Index::open_or_rebuild(&wiki).unwrap();
    idx.set_built_at_revision(0).unwrap();
    // manifest revision is 0 → not stale
    assert!(!idx.is_stale(&wiki).unwrap());
    // index behind manifest → stale
    idx.set_built_at_revision(-1).unwrap();
    assert!(idx.is_stale(&wiki).unwrap());
}

#[test]
fn rebuild_recovers_watermark() {
    let (_d, wiki) = new_wiki();
    Index::open_or_rebuild(&wiki).unwrap();
    let mut txn = Txn::begin(&wiki, &llm_wiki::txn::mint_run_id()).unwrap();
    txn.stage_write("claims/_junk_keep.json", b"{}").unwrap();
    txn.commit().unwrap();
    let rev = wiki.revision().unwrap();
    // poison watermark then rebuild
    let idx = Index::open_or_rebuild(&wiki).unwrap();
    idx.set_built_at_revision(0).unwrap();
    drop(idx);
    let report = Index::rebuild_under_index_lock(&wiki, rev).unwrap();
    assert_eq!(report["revision"], rev);
    let idx = Index::open_or_rebuild(&wiki).unwrap();
    assert_eq!(idx.built_at_revision().unwrap(), rev);
    assert!(!idx.is_stale(&wiki).unwrap());
}

#[test]
fn schema_version_mismatch_drops_and_rebuilds() {
    let (_d, wiki) = new_wiki();
    let idx = Index::open_or_rebuild(&wiki).unwrap();
    idx.set_built_at_revision(7).unwrap();
    idx.conn()
        .pragma_update(None, "user_version", 99i64)
        .unwrap();
    idx.conn()
        .execute_batch("DROP TABLE claims_index")
        .unwrap();
    drop(idx);
    let idx = Index::open_or_rebuild(&wiki).unwrap();
    let uv: i64 = idx
        .conn()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(uv, llm_wiki::index::SCHEMA_VERSION);
    let n: i64 = idx
        .conn()
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE name = 'claims_index'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(n, 1);
    // watermark reset by recreate
    assert!(idx.built_at_revision().unwrap() <= 0);
}

#[test]
fn corrupt_index_file_recovers() {
    let (_d, wiki) = new_wiki();
    Index::open_or_rebuild(&wiki).unwrap();
    std::fs::write(wiki.p(".state/index.sqlite"), b"not sqlite").unwrap();
    let idx = Index::open_or_rebuild(&wiki).unwrap();
    let uv: i64 = idx
        .conn()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(uv, llm_wiki::index::SCHEMA_VERSION);
}
