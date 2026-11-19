use llm_wiki::claims;
use llm_wiki::index::Index;
use llm_wiki::ids;
use llm_wiki::store::{init_wiki, Wiki};
use llm_wiki::txn::Txn;
use serde_json::json;

fn new_wiki() -> (tempfile::TempDir, Wiki) {
    llm_wiki::install_commit_hook();
    let dir = tempfile::tempdir().unwrap();
    let wiki = Wiki::open(dir.path()).unwrap();
    init_wiki(&wiki, "t").unwrap();
    (dir, wiki)
}

fn claim(subject: &str, value: &str) -> serde_json::Value {
    json!({
        "id": ids::new("claim"),
        "subject": subject,
        "predicate": "states",
        "value": value,
        "status": "candidate",
        "authority": {"type": "agent"},
        "scope": {"project": "t"},
        "validity": {"valid_from": "2026-01-01"},
        "evidence": [{"source_id": "source_x", "locator": "l1"}],
        "version": 1,
        "recorded_at": "2026-01-01T00:00:00Z"
    })
}

#[test]
fn claims_write_list_get_archive() {
    let (_d, wiki) = new_wiki();
    let c1 = claim("alice", "v1");
    let c2 = claim("bob", "v2");
    let id1 = c1["id"].as_str().unwrap().to_string();
    let id2 = c2["id"].as_str().unwrap().to_string();

    let mut txn = Txn::begin(&wiki, &llm_wiki::txn::mint_run_id()).unwrap();
    claims::write(&wiki, &mut txn, &c1).unwrap();
    claims::write(&wiki, &mut txn, &c2).unwrap();
    txn.commit().unwrap();

    assert!(wiki.p(&format!("claims/{id1}.json")).is_file());
    let listed = claims::list(&wiki).unwrap();
    assert_eq!(listed.len(), 2);
    let ids_listed: Vec<&str> = listed.iter().map(|c| c["id"].as_str().unwrap()).collect();
    assert!(ids_listed.contains(&id1.as_str()));
    assert!(ids_listed.contains(&id2.as_str()));

    let got = claims::get(&wiki, &id1).unwrap().unwrap();
    assert_eq!(got["subject"], "alice");

    // archive moves the file to claims/_archive/ and get() still resolves
    let mut txn = Txn::begin(&wiki, &llm_wiki::txn::mint_run_id()).unwrap();
    claims::archive(&wiki, &mut txn, &id1).unwrap();
    txn.commit().unwrap();
    assert!(!wiki.p(&format!("claims/{id1}.json")).exists());
    assert!(wiki.p(&format!("claims/_archive/{id1}.json")).is_file());
    assert_eq!(claims::list(&wiki).unwrap().len(), 1);
    let got = claims::get(&wiki, &id1).unwrap().unwrap();
    assert_eq!(got["subject"], "alice");
}

#[test]
fn index_rows_via_fresh_index() {
    let (_d, wiki) = new_wiki();
    let c1 = claim("alice", "v1");
    let mut txn = Txn::begin(&wiki, &llm_wiki::txn::mint_run_id()).unwrap();
    claims::write(&wiki, &mut txn, &c1).unwrap();
    txn.commit().unwrap();
    let rev = wiki.revision().unwrap();

    // rebuild index → fresh; rows come from claims_index
    Index::rebuild_under_index_lock(&wiki, rev).unwrap();
    let rows = claims::index_rows(&wiki).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, c1["id"].as_str().unwrap());
    assert_eq!(rows[0].subject, "alice");
    assert_eq!(rows[0].path, format!("claims/{}.json", c1["id"].as_str().unwrap()));
}

#[test]
fn index_rows_fallback_identical_when_stale() {
    let (_d, wiki) = new_wiki();
    let c1 = claim("alice", "v1");
    let c2 = claim("bob", "v2");
    let mut txn = Txn::begin(&wiki, &llm_wiki::txn::mint_run_id()).unwrap();
    claims::write(&wiki, &mut txn, &c1).unwrap();
    claims::write(&wiki, &mut txn, &c2).unwrap();
    txn.commit().unwrap();
    let rev = wiki.revision().unwrap();

    Index::rebuild_under_index_lock(&wiki, rev).unwrap();
    let fresh_rows = claims::index_rows(&wiki).unwrap();

    // poison watermark → fallback path must return identical rows
    let idx = Index::open_or_rebuild(&wiki).unwrap();
    idx.set_built_at_revision(0).unwrap();
    assert!(idx.is_stale(&wiki).unwrap());
    drop(idx);

    let fallback_rows = claims::index_rows(&wiki).unwrap();
    assert_eq!(fallback_rows, fresh_rows);
}

#[test]
fn index_rows_fallback_repopulates_table() {
    let (_d, wiki) = new_wiki();
    let c1 = claim("alice", "v1");
    let mut txn = Txn::begin(&wiki, &llm_wiki::txn::mint_run_id()).unwrap();
    claims::write(&wiki, &mut txn, &c1).unwrap();
    txn.commit().unwrap();

    // stale index with empty claims_index
    let idx = Index::open_or_rebuild(&wiki).unwrap();
    idx.set_built_at_revision(0).unwrap();
    idx.conn().execute("DELETE FROM claims_index", []).unwrap();
    drop(idx);

    let rows = claims::index_rows(&wiki).unwrap();
    assert_eq!(rows.len(), 1);
    // best-effort repopulate happened
    let idx = Index::open_or_rebuild(&wiki).unwrap();
    let n: i64 = idx
        .conn()
        .query_row("SELECT count(*) FROM claims_index", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 1);
}

#[test]
fn body_text_matches_ftx_composition() {
    let c = json!({
        "subject": "subj", "predicate": "pred", "value": "val",
        "summary": "sum", "title": "ttl",
        "scope": {"b": "2", "a": "1"}
    });
    assert_eq!(claims::body_text(&c), "subj pred val sum ttl a 1 b 2");
}
