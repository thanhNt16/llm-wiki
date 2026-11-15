//! Spec §10.2 gates — index.sqlite is LIVE inside the commit critical
//! section: claim-touching commits patch (apply_txn) or rebuild+stamp,
//! claim-free commits restamp the watermark so readers never fall back
//! after a no-op revision bump (transaction.py:320-328).
mod common;

use llm_wiki::claims;
use llm_wiki::index::Index;
use llm_wiki::ids;
use llm_wiki::store::Wiki;
use llm_wiki::txn::Txn;
use serde_json::json;

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

fn begin(wiki: &Wiki) -> Txn<'_> {
    Txn::begin(wiki, &llm_wiki::txn::mint_run_id()).unwrap()
}

fn watermark(wiki: &Wiki) -> i64 {
    Index::open_or_rebuild(wiki)
        .unwrap()
        .built_at_revision()
        .unwrap()
}

/// claim_id of the first `claims_fts` row MATCHing `term`.
fn fts_hit(wiki: &Wiki, term: &str) -> Option<String> {
    let idx = Index::open_or_rebuild(wiki).unwrap();
    idx.conn()
        .query_row(
            "SELECT claim_id FROM claims_fts WHERE claims_fts MATCH ?1 LIMIT 1",
            [format!("\"{term}\"")],
            |r| r.get(0),
        )
        .ok()
}

fn fts_count(wiki: &Wiki) -> i64 {
    let idx = Index::open_or_rebuild(wiki).unwrap();
    idx.conn()
        .query_row("SELECT COUNT(*) FROM claims_fts", [], |r| r.get(0))
        .unwrap()
}

fn index_row_count(wiki: &Wiki, claim_id: &str) -> i64 {
    let idx = Index::open_or_rebuild(wiki).unwrap();
    idx.conn()
        .query_row(
            "SELECT COUNT(*) FROM claims_index WHERE id = ?1",
            [claim_id],
            |r| r.get(0),
        )
        .unwrap()
}

#[test]
fn claim_commit_patches_index_without_rebuild() {
    let (_d, wiki) = common::wiki();
    // usable index at revision 0
    Index::rebuild_under_index_lock(&wiki, 0).unwrap();
    // sentinel a whole rebuild would delete but a patch must keep
    {
        let idx = Index::open_or_rebuild(&wiki).unwrap();
        idx.conn()
            .execute(
                "INSERT INTO claims_index(id, path) VALUES ('sentinel_keep', 'claims/sentinel_keep.json')",
                [],
            )
            .unwrap();
        idx.conn()
            .execute(
                "INSERT INTO claims_fts(claim_id, body) VALUES ('sentinel_keep', 'zebraquark')",
                [],
            )
            .unwrap();
    }

    let c = claim("quokka", "lives on rottnest");
    let cid = c["id"].as_str().unwrap().to_string();
    let mut txn = begin(&wiki);
    claims::write(&wiki, &mut txn, &c).unwrap();
    txn.commit().unwrap();

    // watermark tracks the manifest exactly
    assert_eq!(wiki.revision().unwrap(), 1);
    assert_eq!(watermark(&wiki), 1);
    let idx = Index::open_or_rebuild(&wiki).unwrap();
    assert!(!idx.is_stale(&wiki).unwrap());
    drop(idx);

    // new claim visible via index rows AND fts — no rebuild needed
    assert_eq!(index_row_count(&wiki, &cid), 1);
    assert_eq!(fts_hit(&wiki, "rottnest").as_deref(), Some(cid.as_str()));

    // sentinel survived → the commit patched, it did not rebuild
    assert_eq!(index_row_count(&wiki, "sentinel_keep"), 1);
    assert_eq!(fts_hit(&wiki, "zebraquark").as_deref(), Some("sentinel_keep"));
}

#[test]
fn claim_delete_removes_index_and_fts_rows() {
    let (_d, wiki) = common::wiki();
    Index::rebuild_under_index_lock(&wiki, 0).unwrap();

    let c = claim("bilby", "digs burrows");
    let cid = c["id"].as_str().unwrap().to_string();
    let mut txn = begin(&wiki);
    claims::write(&wiki, &mut txn, &c).unwrap();
    txn.commit().unwrap();
    assert_eq!(fts_hit(&wiki, "bilby").as_deref(), Some(cid.as_str()));

    let mut txn = begin(&wiki);
    txn.stage_delete(&format!("claims/{cid}.json")).unwrap();
    txn.commit().unwrap();

    assert_eq!(wiki.revision().unwrap(), 2);
    assert_eq!(watermark(&wiki), 2);
    assert_eq!(index_row_count(&wiki, &cid), 0);
    assert_eq!(fts_count(&wiki), 0);
}

#[test]
fn claim_free_commit_restamps_watermark() {
    let (_d, wiki) = common::wiki();
    Index::rebuild_under_index_lock(&wiki, 0).unwrap();

    let c = claim("wombat", "has cube scats");
    let cid = c["id"].as_str().unwrap().to_string();
    let mut txn = begin(&wiki);
    claims::write(&wiki, &mut txn, &c).unwrap();
    txn.commit().unwrap();
    assert_eq!(watermark(&wiki), 1);

    // claim-free commit — e.g. a review triage updating .state only
    let mut txn = begin(&wiki);
    txn.stage_state(".state/aliases.json", &json!({"wombat": "vombatus"}))
        .unwrap();
    txn.commit().unwrap();

    assert_eq!(wiki.revision().unwrap(), 2);
    assert_eq!(watermark(&wiki), 2);
    let idx = Index::open_or_rebuild(&wiki).unwrap();
    assert!(!idx.is_stale(&wiki).unwrap());
    drop(idx);
    // indexed rows still served — the restamp kept them live
    assert_eq!(index_row_count(&wiki, &cid), 1);
    assert_eq!(fts_hit(&wiki, "vombatus"), None); // alias is not a claim
    assert_eq!(fts_hit(&wiki, "wombat").as_deref(), Some(cid.as_str()));
}

#[test]
fn claim_commit_on_unusable_index_rebuilds() {
    let (_d, wiki) = common::wiki();
    // index never built (built_at_revision = -1 ≠ revision 0) AND claim A
    // already on disk outside any commit — only a whole rebuild can see it.
    let a = claim("echidna", "lays eggs");
    let a_id = a["id"].as_str().unwrap().to_string();
    std::fs::create_dir_all(wiki.p("claims")).unwrap();
    std::fs::write(
        wiki.p(&format!("claims/{a_id}.json")),
        serde_json::to_string_pretty(&a).unwrap(),
    )
    .unwrap();

    let b = claim("platypus", "senses electric fields");
    let b_id = b["id"].as_str().unwrap().to_string();
    let mut txn = begin(&wiki);
    claims::write(&wiki, &mut txn, &b).unwrap();
    txn.commit().unwrap();

    assert_eq!(wiki.revision().unwrap(), 1);
    assert_eq!(watermark(&wiki), 1);
    // rebuild at new_rev pulled BOTH claims in — a patch would miss A
    assert_eq!(index_row_count(&wiki, &a_id), 1);
    assert_eq!(index_row_count(&wiki, &b_id), 1);
    assert_eq!(fts_hit(&wiki, "echidna").as_deref(), Some(a_id.as_str()));
    assert_eq!(fts_hit(&wiki, "platypus").as_deref(), Some(b_id.as_str()));
}

#[test]
fn watermark_tracks_manifest_on_every_commit_kind() {
    let (_d, wiki) = common::wiki();
    Index::rebuild_under_index_lock(&wiki, 0).unwrap();

    let c = claim("quoll", "spotted marsupial");
    let cid = c["id"].as_str().unwrap().to_string();

    let mut txn = begin(&wiki);
    claims::write(&wiki, &mut txn, &c).unwrap();
    txn.commit().unwrap();
    assert_eq!(watermark(&wiki), wiki.revision().unwrap());

    let mut txn = begin(&wiki);
    txn.stage_state(".state/aliases.json", &json!({"q": "quoll"}))
        .unwrap();
    txn.commit().unwrap();
    assert_eq!(watermark(&wiki), wiki.revision().unwrap());

    let mut txn = begin(&wiki);
    txn.stage_delete(&format!("claims/{cid}.json")).unwrap();
    txn.commit().unwrap();
    assert_eq!(watermark(&wiki), wiki.revision().unwrap());
}

#[test]
fn stale_index_readers_fall_back_without_error() {
    let (_d, wiki) = common::wiki();
    let c = claim("numbat", "eats termites");
    let cid = c["id"].as_str().unwrap().to_string();
    let mut txn = begin(&wiki);
    claims::write(&wiki, &mut txn, &c).unwrap();
    txn.commit().unwrap();
    assert_eq!(index_row_count(&wiki, &cid), 1);

    // manifest bumps past the index watermark (out-of-band commit)
    let mut state = wiki.state().unwrap();
    state["revision"] = json!(wiki.revision().unwrap() + 5);
    wiki.save_state(&state).unwrap();

    let idx = Index::open_or_rebuild(&wiki).unwrap();
    assert!(idx.is_stale(&wiki).unwrap());
    drop(idx);

    // readers still answer — canonical fallback, never an error
    let rows = claims::index_rows(&wiki).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, cid);
    let got = claims::get(&wiki, &cid).unwrap();
    assert!(got.is_some());
}

#[test]
fn corrupt_index_recovers_then_next_claim_commit_rebuilds() {
    let (_d, wiki) = common::wiki();
    let a = claim("dunnart", "tiny predator");
    let a_id = a["id"].as_str().unwrap().to_string();
    let mut txn = begin(&wiki);
    claims::write(&wiki, &mut txn, &a).unwrap();
    txn.commit().unwrap();
    assert_eq!(fts_hit(&wiki, "dunnart").as_deref(), Some(a_id.as_str()));

    // corrupt the file wholesale — disposable projection, must recover
    std::fs::write(wiki.p(".state/index.sqlite"), b"not sqlite").unwrap();
    let idx = Index::open_or_rebuild(&wiki).unwrap();
    let uv: i64 = idx
        .conn()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(uv, llm_wiki::index::SCHEMA_VERSION);
    assert_eq!(idx.built_at_revision().unwrap(), -1); // recreated → unusable
    drop(idx);

    // next claim-touching commit rebuilds at current+1, not patch
    let b = claim("potoroo", "fungus forager");
    let b_id = b["id"].as_str().unwrap().to_string();
    let mut txn = begin(&wiki);
    claims::write(&wiki, &mut txn, &b).unwrap();
    txn.commit().unwrap();

    assert_eq!(wiki.revision().unwrap(), 2);
    assert_eq!(watermark(&wiki), 2);
    assert_eq!(index_row_count(&wiki, &a_id), 1);
    assert_eq!(index_row_count(&wiki, &b_id), 1);
    assert_eq!(fts_hit(&wiki, "potoroo").as_deref(), Some(b_id.as_str()));
}
