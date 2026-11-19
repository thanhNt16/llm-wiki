
use llm_wiki::deps;
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

fn txn(wiki: &Wiki) -> Txn<'_> {
    Txn::begin(wiki, &llm_wiki::txn::mint_run_id()).unwrap()
}

fn write_claim(wiki: &Wiki, subject: &str) -> String {
    let claim = json!({
        "id": llm_wiki::ids::new("claim"),
        "subject": subject,
        "predicate": "p",
        "value": "v",
        "status": "accepted",
        "version": 1,
    });
    let id = claim["id"].as_str().unwrap().to_string();
    let mut t = txn(wiki);
    llm_wiki::claims::write(wiki, &mut t, &claim).unwrap();
    t.commit().unwrap();
    id
}

#[test]
fn missing_edges_file_is_empty_graph() {
    let (_d, wiki) = new_wiki();
    let g = deps::graph(&wiki).unwrap();
    assert!(g["edges"].as_object().unwrap().is_empty());
    assert!(g["stale"].as_array().unwrap().is_empty());
    assert_eq!(deps::stale_artifacts(&wiki).unwrap().len(), 0);
}

#[test]
fn register_merges_dedupes_preserves_order() {
    let (_d, wiki) = new_wiki();
    let mut t = txn(&wiki);
    deps::register(&mut t, "wiki/a.md", &["c1@1", "c2@2"]).unwrap();
    deps::register(&mut t, "wiki/a.md", &["c2@2", "c3@1"]).unwrap();
    t.commit().unwrap();
    let g = deps::graph(&wiki).unwrap();
    assert_eq!(
        g["edges"]["wiki/a.md"],
        json!(["c1@1", "c2@2", "c3@1"]),
        "existing order kept, new appended, dups skipped"
    );
}

#[test]
fn diamond_invalidate_propagates_transitively() {
    let (_d, wiki) = new_wiki();
    // diamond: A → B,C ; B → D ; C → D  (edges = artifact → deps)
    let mut t = txn(&wiki);
    deps::register(&mut t, "wiki/a.md", &["wiki/b.md", "wiki/c.md"]).unwrap();
    deps::register(&mut t, "wiki/b.md", &["claim_D@1"]).unwrap();
    deps::register(&mut t, "wiki/c.md", &["claim_D@2"]).unwrap();
    t.commit().unwrap();

    let mut t = txn(&wiki);
    let newly = deps::invalidate(&wiki, &mut t, &["claim_D"]).unwrap();
    t.commit().unwrap();
    assert_eq!(newly, vec!["wiki/a.md", "wiki/b.md", "wiki/c.md"]);

    // stale list updated on disk, sorted
    let g = deps::graph(&wiki).unwrap();
    assert_eq!(
        g["stale"],
        json!(["wiki/a.md", "wiki/b.md", "wiki/c.md"])
    );
    assert!(deps::is_stale(&wiki, "wiki/a.md").unwrap());
}

#[test]
fn invalidate_matches_any_version_of_changed_id() {
    let (_d, wiki) = new_wiki();
    let mut t = txn(&wiki);
    deps::register(&mut t, "wiki/p.md", &["claim_x@7"]).unwrap();
    t.commit().unwrap();

    let mut t = txn(&wiki);
    // dep pinned at @7 still invalidates on changed id claim_x
    let newly = deps::invalidate(&wiki, &mut t, &["claim_x"]).unwrap();
    t.commit().unwrap();
    assert_eq!(newly, vec!["wiki/p.md"]);
}

#[test]
fn invalidate_sees_staged_overlay_in_same_txn() {
    let (_d, wiki) = new_wiki();
    // deps.py:50-55 — staged-but-uncommitted edges feed the walk
    let mut t = txn(&wiki);
    deps::register(&mut t, "wiki/one.md", &["claim_s@1"]).unwrap();
    let newly = deps::invalidate(&wiki, &mut t, &["claim_s"]).unwrap();
    assert_eq!(newly, vec!["wiki/one.md"]);
    // staged stale lands via the same txn's commit
    t.commit().unwrap();
    let g = deps::graph(&wiki).unwrap();
    assert_eq!(g["stale"], json!(["wiki/one.md"]));
}

#[test]
fn already_stale_artifacts_not_returned_again() {
    let (_d, wiki) = new_wiki();
    let mut t = txn(&wiki);
    deps::register(&mut t, "wiki/z.md", &["claim_q@1"]).unwrap();
    deps::mark_stale(&mut t, &["wiki/z.md"]).unwrap();
    t.commit().unwrap();

    let mut t = txn(&wiki);
    let newly = deps::invalidate(&wiki, &mut t, &["claim_q"]).unwrap();
    t.commit().unwrap();
    assert!(newly.is_empty(), "already-stale artifact is not newly");
}

#[test]
fn clear_and_mark_stale_round_trip() {
    let (_d, wiki) = new_wiki();
    let mut t = txn(&wiki);
    deps::mark_stale(&mut t, &["wiki/m.md", "wiki/n.md"]).unwrap();
    t.commit().unwrap();
    assert_eq!(
        deps::graph(&wiki).unwrap()["stale"],
        json!(["wiki/m.md", "wiki/n.md"])
    );

    let mut t = txn(&wiki);
    deps::clear_stale(&mut t, &["wiki/m.md"]).unwrap();
    t.commit().unwrap();
    assert_eq!(deps::graph(&wiki).unwrap()["stale"], json!(["wiki/n.md"]));
}

#[test]
fn live_ids_cover_claims_decisions_wiki_and_review() {
    let (_d, wiki) = new_wiki();
    let claim_id = write_claim(&wiki, "live subject");

    // a decision + a wiki page + a review item
    std::fs::create_dir_all(wiki.p("decisions")).unwrap();
    std::fs::write(
        wiki.p("decisions/decision_1.json"),
        serde_json::to_string_pretty(&json!({"id": "decision_1"})).unwrap(),
    )
    .unwrap();
    std::fs::create_dir_all(wiki.p("wiki/concept")).unwrap();
    std::fs::write(wiki.p("wiki/concept/page.md"), b"# page").unwrap();
    std::fs::write(
        wiki.p(".state/review-queue.jsonl"),
        "{\"id\": \"review_abc\", \"status\": \"open\"}\n",
    )
    .unwrap();

    let live = deps::live_ids(&wiki).unwrap();
    assert!(live.contains(&claim_id));
    assert!(live.contains("decision_1"));
    assert!(live.contains("wiki/concept/page.md"));
    assert!(live.contains("review_abc"));

    // edge-key exemption: artifact registered in edges resolves even
    // without an on-disk file (dream.py:185 — `dep_id in edges`)
    let mut t = txn(&wiki);
    deps::register(&mut t, "wiki/ghost.md", &[&claim_id]).unwrap();
    t.commit().unwrap();
    let live = deps::live_ids(&wiki).unwrap();
    assert!(live.contains("wiki/ghost.md"));
}

#[test]
fn dead_dep_detection_respects_prefix_exemptions() {
    let (_d, wiki) = new_wiki();
    let claim_id = write_claim(&wiki, "live dep");
    std::fs::create_dir_all(wiki.p("wiki/concept")).unwrap();
    std::fs::write(wiki.p("wiki/concept/exists.md"), b"x").unwrap();
    std::fs::write(
        wiki.p(".state/review-queue.jsonl"),
        "{\"id\": \"review_x\", \"status\": \"open\"}\n",
    )
    .unwrap();

    let live = deps::live_ids(&wiki).unwrap();
    // version suffix stripped
    assert!(deps::is_live_dep(&format!("{claim_id}@3"), &live));
    // wiki/ file exists → live
    assert!(deps::is_live_dep("wiki/concept/exists.md@1", &live));
    // review_ queue item → live
    assert!(deps::is_live_dep("review_x@1", &live));
    // missing claim, missing page, unknown review id, bare junk → dead
    assert!(!deps::is_live_dep("claim_dead@1", &live));
    assert!(!deps::is_live_dep("wiki/concept/gone.md@1", &live));
    assert!(!deps::is_live_dep("review_missing@1", &live));
    assert!(!deps::is_live_dep("whatever@2", &live));
    // unparseable dep counts as LIVE (conservative, spec §8)
    assert!(deps::is_live_dep("@", &live));
    assert!(deps::is_live_dep("", &live));
}

#[test]
fn dead_claim_id_not_live() {
    let (_d, wiki) = new_wiki();
    let live = deps::live_ids(&wiki).unwrap();
    assert!(!deps::is_live_dep("claim_nope@4", &live));
}
