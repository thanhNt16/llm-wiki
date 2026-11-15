//! `dream` — lifecycle maintenance engine tests (dream.py parity + spec
//! §8 ghost pinning). Seeded wikis via tests/common helpers; the CLI
//! surface is exercised through the compiled binary.
mod common;

use std::fs;
use std::process::Command;

use llm_wiki::claims;
use llm_wiki::dream::{self, Date};
use llm_wiki::deps;
use llm_wiki::store::Wiki;
use serde_json::{json, Value};

const NOW: &str = "2026-10-25";
const SID: &str = "source_AAAAAAAAAAAAAAAAAAAAAAAAAA";

fn now() -> Date {
    Date::parse(NOW).unwrap()
}

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_llm-wiki"))
}

fn run_json(args: &[&str]) -> (i32, Value) {
    let out = bin().args(args).output().unwrap();
    let code = out.status.code().unwrap_or(-1);
    let v: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout not JSON ({e}): {}", String::from_utf8_lossy(&out.stdout)));
    (code, v)
}

fn claim(id: &str) -> Value {
    json!({
        "id": id,
        "subject": "s",
        "predicate": "p",
        "value": 1,
        "scope": {},
        "status": "candidate",
        "authority": {"type": "agent_inference"},
        "evidence": [{"source_id": SID, "source_version": 1,
                      "locator": {"type": "text", "value": "l"}}],
        "supersedes": [],
        "version": 1,
        "recorded_at": "2026-09-15T00:00:00Z",
    })
}

fn seed_claim(wiki: &Wiki, c: &Value) {
    let id = c["id"].as_str().unwrap();
    let path = wiki.p(&format!("claims/{id}.json"));
    fs::write(&path, serde_json::to_string_pretty(c).unwrap()).unwrap();
}

fn set_edges(wiki: &Wiki, edges: &Value) {
    fs::create_dir_all(wiki.p(".state")).unwrap();
    fs::write(
        wiki.p(deps::DEPS_FILE),
        serde_json::to_string_pretty(&json!({"edges": edges, "stale": []})).unwrap(),
    )
    .unwrap();
}

fn write_page(wiki: &Wiki, rel: &str) {
    let path = wiki.p(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, "x\n").unwrap();
}

fn add_review_item(wiki: &Wiki, affected: &[&str]) {
    let line = json!({
        "id": "review_x", "status": "open",
        "kind": "possible_contradiction",
        "affected": affected,
    });
    fs::create_dir_all(wiki.p(".state")).unwrap();
    fs::write(
        wiki.p(deps::REVIEW_QUEUE),
        format!("{}\n", serde_json::to_string(&line).unwrap()),
    )
    .unwrap();
}

fn edges(wiki: &Wiki) -> Value {
    deps::graph(wiki).unwrap()["edges"].clone()
}

fn archive_items<'v>(out: &'v Value, phase: &str) -> &'v Vec<Value> {
    out["phases"][phase]["archive"].as_array().unwrap()
}

// ---------------------------------------------------------------- claims

#[test]
fn superseded_claim_planned_and_archived() {
    let (_d, wiki) = common::wiki();
    let mut old = claim("claim_old");
    old["status"] = json!("superseded");
    old["superseded_by"] = json!("claim_zz");
    seed_claim(&wiki, &old);
    seed_claim(&wiki, &claim("claim_new"));

    let out = dream::run(&wiki, false, None, Some(now())).unwrap();
    assert_eq!(out["applied"], false);
    assert_eq!(archive_items(&out, "supersede")[0]["id"], "claim_old");
    assert_eq!(
        archive_items(&out, "supersede")[0]["reason"],
        "superseded_by claim_zz"
    );

    let rev = wiki.revision().unwrap();
    let out = dream::run(&wiki, true, None, Some(now())).unwrap();
    assert_eq!(out["applied"], true);
    assert_eq!(out["archived"], json!(["claim_old"]));
    assert_eq!(wiki.revision().unwrap(), rev + 1);

    let live: Vec<String> = claims::list(&wiki)
        .unwrap()
        .iter()
        .filter_map(|c| c["id"].as_str().map(String::from))
        .collect();
    assert_eq!(live, ["claim_new"]);
    let arc = wiki.load_json("claims/_archive/claim_old.json").unwrap();
    assert_eq!(arc["dream_reason"], "superseded_by claim_zz");
    assert!(arc["dreamed_at"].as_str().unwrap().starts_with(NOW));
}

#[test]
fn expired_claim_archived_future_kept() {
    let (_d, wiki) = common::wiki();
    let mut dead = claim("claim_dead");
    dead["status"] = json!("accepted");
    dead["valid_to"] = json!("2026-01-01");
    let mut alive = claim("claim_alive");
    alive["status"] = json!("accepted");
    alive["valid_to"] = json!("2027-01-01");
    seed_claim(&wiki, &dead);
    seed_claim(&wiki, &alive);

    let out = dream::run(&wiki, true, None, Some(now())).unwrap();
    assert_eq!(out["archived"], json!(["claim_dead"]));
    assert_eq!(
        archive_items(&out, "expire")[0]["reason"],
        "valid_to 2026-01-01 passed"
    );
    let live: Vec<String> = claims::list(&wiki)
        .unwrap()
        .iter()
        .filter_map(|c| c["id"].as_str().map(String::from))
        .collect();
    assert_eq!(live, ["claim_alive"]);
}

#[test]
fn unparseable_valid_to_skipped() {
    let (_d, wiki) = common::wiki();
    let mut c = claim("claim_bad");
    c["status"] = json!("accepted");
    c["valid_to"] = json!("not-a-date");
    seed_claim(&wiki, &c);
    let out = dream::run(&wiki, false, None, Some(now())).unwrap();
    assert_eq!(out["phases"]["expire"]["archive"], json!([]));
    assert!(out["phases"]["expire"]["skip"][0]["reason"]
        .as_str()
        .unwrap()
        .contains("unparseable valid_to"));
}

#[test]
fn duplicates_keep_newest() {
    let (_d, wiki) = common::wiki();
    let mut old = claim("claim_dup_old");
    old["status"] = json!("accepted");
    old["recorded_at"] = json!("2026-01-01T00:00:00Z");
    let mut new = claim("claim_dup_new");
    new["status"] = json!("accepted");
    new["recorded_at"] = json!("2026-09-01T00:00:00Z");
    seed_claim(&wiki, &old);
    seed_claim(&wiki, &new);

    let out = dream::run(&wiki, true, None, Some(now())).unwrap();
    assert_eq!(out["archived"], json!(["claim_dup_old"]));
    assert_eq!(
        archive_items(&out, "dedupe")[0]["reason"],
        "duplicate of claim_dup_new"
    );
}

#[test]
fn evidenceless_candidate_archived_review_open_skipped() {
    let (_d, wiki) = common::wiki();
    let mut naked = claim("claim_naked");
    naked["evidence"] = json!([]);
    let mut reviewed = claim("claim_reviewed");
    reviewed["evidence"] = json!([]);
    seed_claim(&wiki, &naked);
    seed_claim(&wiki, &reviewed);
    add_review_item(&wiki, &["claim_reviewed"]);

    let out = dream::run(&wiki, true, None, Some(now())).unwrap();
    assert_eq!(out["archived"], json!(["claim_naked"]));
    let skipped: Vec<&str> = out["skipped"]["orphan"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["id"].as_str())
        .collect();
    assert_eq!(skipped, ["claim_reviewed"]);
}

// ---------------------------------------------------------------- junk

#[test]
fn junk_staging_and_locks() {
    let (_d, wiki) = common::wiki();
    let staging = wiki.p(".state/staging/run_x");
    fs::create_dir_all(&staging).unwrap();
    let locks = wiki.p(".state/locks");
    fs::create_dir_all(&locks).unwrap();
    fs::write(locks.join("orphan.lock"), "").unwrap();

    let out = dream::run(&wiki, true, None, Some(now())).unwrap();
    assert!(!staging.is_dir());
    assert!(!locks.join("orphan.lock").exists());
    assert_eq!(out["deleted"].as_array().unwrap().len(), 2);
}

#[test]
fn journals_and_backups_defer_to_repair_skip() {
    let (_d, wiki) = common::wiki();
    fs::write(wiki.p(".state/journal.json"), "{}").unwrap();
    fs::create_dir_all(wiki.p(".state/.backup-0")).unwrap();

    let out = dream::run(&wiki, false, None, Some(now())).unwrap();
    let delete = out["phases"]["junk"]["delete"].as_array().unwrap();
    assert!(delete.is_empty(), "journals/backups never delete: {delete:?}");
    let reasons: Vec<&str> = out["phases"]["junk"]["skip"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["reason"].as_str())
        .collect();
    assert_eq!(reasons.len(), 2);
    assert!(reasons.iter().all(|r| *r == "repair-required"));
}

#[test]
fn live_staging_run_skipped() {
    let (_d, wiki) = common::wiki();
    let live_dir = wiki.p(".state/staging/run_live");
    fs::create_dir_all(&live_dir).unwrap();
    // live phase: pending.json present without compile.json (§5.5).
    fs::write(live_dir.join("pending.json"), "{}").unwrap();
    fs::write(live_dir.join(".live"), "").unwrap();
    let dead_dir = wiki.p(".state/staging/run_dead");
    fs::create_dir_all(&dead_dir).unwrap();

    let out = dream::run(&wiki, false, None, Some(now())).unwrap();
    let skip_paths: Vec<&str> = out["phases"]["junk"]["skip"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["path"].as_str())
        .collect();
    assert!(skip_paths.contains(&".state/staging/run_live"), "{skip_paths:?}");
    let del_paths: Vec<&str> = out["phases"]["junk"]["delete"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["path"].as_str())
        .collect();
    assert_eq!(del_paths, [".state/staging/run_dead"]);
}

#[test]
fn held_commit_lock_skipped_never_deleted() {
    let (_d, wiki) = common::wiki();
    fs::create_dir_all(wiki.p(".state/locks")).unwrap();
    let _lock = llm_wiki::txn::lock::Lock::acquire(&wiki).unwrap();

    let out = dream::run(&wiki, false, None, Some(now())).unwrap();
    let delete = out["phases"]["junk"]["delete"].as_array().unwrap();
    assert!(delete.is_empty());
    let skip = out["phases"]["junk"]["skip"].as_array().unwrap();
    assert_eq!(skip[0]["path"], ".state/locks/commit.lock");
    assert_eq!(skip[0]["reason"], "commit.lock is held");
}

// ---------------------------------------------------------------- derived

#[test]
fn derived_dead_dep_edge_dropped() {
    let (_d, wiki) = common::wiki();
    seed_claim(&wiki, &claim("claim_live"));
    write_page(&wiki, "wiki/concepts/s.md");
    set_edges(&wiki, &json!({"wiki/concepts/s.md": ["claim_ghost", "claim_live"]}));

    let out = dream::run(&wiki, true, None, Some(now())).unwrap();
    assert_eq!(out["edge_drops"], 1);
    assert_eq!(
        edges(&wiki)["wiki/concepts/s.md"],
        json!(["claim_live"])
    );
}

// ---------------------------------------------------------------- ghost

#[test]
fn ghost_all_dead_deps_deleted_survivors_first() {
    let (_d, wiki) = common::wiki();
    seed_claim(&wiki, &claim("claim_live"));
    write_page(&wiki, "wiki/concepts/ghost.md");
    write_page(&wiki, "wiki/concepts/survivor.md");
    set_edges(
        &wiki,
        &json!({
            "wiki/concepts/ghost.md": ["claim_dead"],
            "wiki/concepts/survivor.md": ["wiki/concepts/ghost.md", "claim_live"],
        }),
    );

    let plan = dream::collect_plan(&wiki, None, Some(now())).unwrap();
    let del: Vec<&str> = plan["phases"]["ghost"]["delete"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["path"].as_str())
        .collect();
    assert_eq!(del, ["wiki/concepts/ghost.md"]);

    let rev = wiki.revision().unwrap();
    let out = dream::run(&wiki, true, None, Some(now())).unwrap();
    // one commit: page gone, own edges entry removed, survivors' dep
    // lists cleaned — all atomically in a single revision bump.
    assert_eq!(wiki.revision().unwrap(), rev + 1);
    assert_eq!(out["ghost_deleted"], json!(["wiki/concepts/ghost.md"]));
    assert!(!wiki.p("wiki/concepts/ghost.md").exists());
    assert!(wiki.p("wiki/concepts/survivor.md").exists());

    let e = edges(&wiki);
    assert!(
        e.get("wiki/concepts/ghost.md").is_none(),
        "ghost's own edges entry must be removed: {e}"
    );
    assert_eq!(
        e["wiki/concepts/survivor.md"],
        json!(["claim_live"]),
        "dead wiki/ dep must be dropped from surviving artifacts first"
    );
}

#[test]
fn ghost_zero_dep_page_never_eligible() {
    let (_d, wiki) = common::wiki();
    write_page(&wiki, "wiki/concepts/zero.md");
    set_edges(&wiki, &json!({}));
    let plan = dream::collect_plan(&wiki, None, Some(now())).unwrap();
    assert_eq!(plan["phases"]["ghost"]["delete"], json!([]));

    // even an empty-but-present edges entry does not qualify (≥1 dep rule).
    set_edges(&wiki, &json!({"wiki/concepts/zero.md": []}));
    let plan = dream::collect_plan(&wiki, None, Some(now())).unwrap();
    assert_eq!(plan["phases"]["ghost"]["delete"], json!([]));
    assert!(wiki.p("wiki/concepts/zero.md").exists());
}

#[test]
fn ghost_live_dep_page_kept() {
    let (_d, wiki) = common::wiki();
    seed_claim(&wiki, &claim("claim_live"));
    write_page(&wiki, "wiki/concepts/mixed.md");
    set_edges(
        &wiki,
        &json!({"wiki/concepts/mixed.md": ["claim_dead", "claim_live"]}),
    );
    let plan = dream::collect_plan(&wiki, None, Some(now())).unwrap();
    assert_eq!(plan["phases"]["ghost"]["delete"], json!([]));
}

#[test]
fn ghost_under_review_page_skipped() {
    let (_d, wiki) = common::wiki();
    write_page(&wiki, "wiki/concepts/r1.md");
    write_page(&wiki, "wiki/concepts/r2.md");
    set_edges(
        &wiki,
        &json!({
            "wiki/concepts/r1.md": ["claim_dead"],
            "wiki/concepts/r2.md": ["claim_dead"],
        }),
    );
    // path hit for r1; dep-id hit for r2 — both under review.
    add_review_item(&wiki, &["wiki/concepts/r1.md", "claim_dead"]);

    let plan = dream::collect_plan(&wiki, None, Some(now())).unwrap();
    assert_eq!(plan["phases"]["ghost"]["delete"], json!([]));
    let skipped: Vec<&str> = plan["phases"]["ghost"]["skip"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| s["path"].as_str())
        .collect();
    assert_eq!(skipped, ["wiki/concepts/r1.md", "wiki/concepts/r2.md"]);
}

#[test]
fn ghost_unparseable_dep_counts_live() {
    let (_d, wiki) = common::wiki();
    write_page(&wiki, "wiki/concepts/odd.md");
    set_edges(&wiki, &json!({"wiki/concepts/odd.md": ["@3"]}));
    let plan = dream::collect_plan(&wiki, None, Some(now())).unwrap();
    assert_eq!(plan["phases"]["ghost"]["delete"], json!([]));
}

#[test]
fn ghost_phase_filter_runs_subset() {
    let (_d, wiki) = common::wiki();
    let mut old = claim("claim_old");
    old["superseded_by"] = json!("claim_zz");
    seed_claim(&wiki, &old);
    write_page(&wiki, "wiki/concepts/ghost.md");
    set_edges(&wiki, &json!({"wiki/concepts/ghost.md": ["claim_dead"]}));

    let out = dream::run(&wiki, true, Some(&["ghost"]), Some(now())).unwrap();
    assert_eq!(out["archived"], json!([])); // supersede not run
    assert!(wiki.p("claims/claim_old.json").exists());
    assert!(!wiki.p("wiki/concepts/ghost.md").exists());
}

#[test]
fn empty_phase_slice_means_all_phases() {
    let (_d, wiki) = common::wiki();
    let mut old = claim("claim_old");
    old["superseded_by"] = json!("claim_zz");
    seed_claim(&wiki, &old);
    // dream.py: `phases or PHASES` — an explicit empty slice is "all".
    let plan = dream::collect_plan(&wiki, Some(&[]), Some(now())).unwrap();
    assert_eq!(archive_items(&plan, "supersede")[0]["id"], "claim_old");
}


#[test]
fn ghost_dry_run_mutates_nothing() {
    let (_d, wiki) = common::wiki();
    write_page(&wiki, "wiki/concepts/ghost.md");
    set_edges(&wiki, &json!({"wiki/concepts/ghost.md": ["claim_dead"]}));
    let rev = wiki.revision().unwrap();

    let out = dream::run(&wiki, false, None, Some(now())).unwrap();
    assert_eq!(out["applied"], false);
    assert_eq!(wiki.revision().unwrap(), rev);
    assert!(wiki.p("wiki/concepts/ghost.md").exists());
    assert_eq!(
        edges(&wiki)["wiki/concepts/ghost.md"],
        json!(["claim_dead"])
    );
}

#[test]
fn ghost_apply_rebuilds_index() {
    let (_d, wiki) = common::wiki();
    let mut old = claim("claim_old");
    old["superseded_by"] = json!("claim_zz");
    seed_claim(&wiki, &old);
    let out = dream::run(&wiki, true, None, Some(now())).unwrap();
    assert_eq!(out["applied"], true);
    // post-commit rebuild: archived claims leave the claims_index table.
    let idx = llm_wiki::index::Index::open_or_rebuild(&wiki).unwrap();
    assert_eq!(idx.built_at_revision().unwrap(), wiki.revision().unwrap());
    let rows = claims::index_rows(&wiki).unwrap();
    assert!(rows.iter().all(|r| r.id != "claim_old"));
}

// ---------------------------------------------------------------- purge

#[test]
fn purge_old_unreferenced_deleted() {
    let (_d, wiki) = common::wiki();
    let mut old = claim("claim_old");
    old["superseded_by"] = json!("claim_zz");
    seed_claim(&wiki, &old);
    dream::run(&wiki, true, None, Some(now())).unwrap(); // archives @ 2026-10-25

    let out = dream::purge(&wiki, true, Date::parse("2026-11-24"), 90).unwrap();
    assert_eq!(out["purge"]["skipped"][0]["reason"], "too_recent");

    let out = dream::purge(&wiki, true, Date::parse("2027-01-24"), 90).unwrap();
    assert_eq!(out["purge"]["purged"], json!(["claims/_archive/claim_old.json"]));

    let out = dream::purge(&wiki, false, Date::parse("2027-01-24"), 90).unwrap();
    assert_eq!(out["purge"]["purged"], json!(["claims/_archive/claim_old.json"]));
    assert!(!wiki.p("claims/_archive/claim_old.json").exists());
}

#[test]
fn purge_still_referenced_skipped() {
    let (_d, wiki) = common::wiki();
    let mut old = claim("claim_old");
    old["superseded_by"] = json!("claim_zz");
    seed_claim(&wiki, &old);
    dream::run(&wiki, true, None, Some(now())).unwrap();

    let mut keeper = claim("claim_keep");
    keeper["supersedes"] = json!(["claim_old"]);
    seed_claim(&wiki, &keeper);

    let out = dream::purge(&wiki, true, Date::parse("2027-01-24"), 90).unwrap();
    assert_eq!(out["purge"]["purged"], json!([]));
    assert_eq!(out["purge"]["skipped"][0]["reason"], "still_referenced");
}

// ---------------------------------------------------------------- cli

#[test]
fn cli_dream_dry_run_and_apply() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap();
    let (code, _) = run_json(&["--root", root, "init"]);
    assert_eq!(code, 0);
    let wiki = Wiki::open(dir.path()).unwrap();
    let mut old = claim("claim_old");
    old["superseded_by"] = json!("claim_zz");
    seed_claim(&wiki, &old);

    let (code, out) = run_json(&["--root", root, "dream"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out["applied"], false);
    assert!(out["phases"].get("ghost").is_some());
    assert!(wiki.p("claims/claim_old.json").exists());

    let (code, out) = run_json(&["--root", root, "dream", "--apply"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out["applied"], true);
    assert_eq!(out["archived"], json!(["claim_old"]));
    assert!(!wiki.p("claims/claim_old.json").exists());
}

#[test]
fn cli_dream_phase_ghost_and_invalid_phase() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap();
    run_json(&["--root", root, "init"]);
    let wiki = Wiki::open(dir.path()).unwrap();
    write_page(&wiki, "wiki/concepts/ghost.md");
    set_edges(&wiki, &json!({"wiki/concepts/ghost.md": ["claim_dead"]}));

    let (code, out) = run_json(&["--root", root, "dream", "--phase", "ghost", "--apply"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out["ghost_deleted"], json!(["wiki/concepts/ghost.md"]));

    let (code, _) = run_json(&["--root", root, "dream", "--phase", "bogus"]);
    assert_eq!(code, 2);
}

#[test]
fn cli_dream_purge_dry_run() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_str().unwrap();
    run_json(&["--root", root, "init"]);
    let wiki = Wiki::open(dir.path()).unwrap();
    let mut old = claim("claim_old");
    old["superseded_by"] = json!("claim_zz");
    seed_claim(&wiki, &old);
    let out = dream::run(&wiki, true, None, Some(now())).unwrap();
    assert_eq!(out["applied"], true);

    let (code, out) = run_json(&["--root", root, "dream", "--purge", "--dry-run"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(out["applied"], false);
    assert_eq!(out["purge"]["retention_days"], 90);
    // archive entry is fresh → skipped, nothing deleted.
    assert_eq!(out["purge"]["purged"], json!([]));
    assert!(wiki.p("claims/_archive/claim_old.json").exists());
}
