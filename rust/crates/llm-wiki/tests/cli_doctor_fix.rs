//! `doctor --fix` — spec §9: FIXABLE execution, scope/code filters,
//! legacy flag reconciliation, --dry-run simulation.

use std::process::Command;

use serde_json::{json, Value};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_llm-wiki"))
}

fn init(dir: &std::path::Path) {
    let out = bin().arg("init").arg(dir).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "init: {:?}", out);
}

fn run_json(args: &[&str]) -> (i32, Value) {
    let out = bin().args(args).output().unwrap();
    let code = out.status.code().unwrap_or(-1);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    (code, v)
}

fn doctor(dir: &std::path::Path, extra: &[&str]) -> (i32, Value) {
    let mut args = vec!["--root", dir.to_str().unwrap(), "doctor"];
    args.extend_from_slice(extra);
    run_json(&args)
}

fn dot(dir: &std::path::Path) -> std::path::PathBuf {
    dir.join(".llm-wiki")
}

fn codes(v: &Value) -> Vec<String> {
    v["findings"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(|f| f["code"].as_str().unwrap().to_string())
        .collect()
}

fn remaining_codes(v: &Value) -> Vec<String> {
    v["remaining"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(|f| f["code"].as_str().unwrap().to_string())
        .collect()
}

fn fixed_actions(v: &Value) -> Vec<(String, String)> {
    v["fixed"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .map(|f| {
            (
                f["code"].as_str().unwrap_or("").to_string(),
                f["action"].as_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

/// Crockford-safe claim id (`[a-z]+_[0-9A-HJKMNP-TV-Z]{26}`).
fn cid(n: u64) -> String {
    format!("claim_{:026}", n)
}

/// Crockford-safe decision id.
fn did(n: u64) -> String {
    format!("decision_{:026}", n)
}

fn claim_json(id: &str, version: i64) -> Value {
    json!({
        "id": id,
        "subject": "subject one",
        "predicate": "pred",
        "value": "v",
        "status": "candidate",
        "authority": {"type": "authoritative_source"},
        "evidence": [],
        "recorded_at": "2026-11-01T00:00:00Z",
        "version": version,
    })
}

fn decision_json(id: &str, claims: &[&str], version: i64) -> Value {
    json!({
        "id": id,
        "title": "Some decision",
        "status": "accepted",
        "decided_on": "2026-11-01",
        "recorded_at": "2026-11-01T00:00:00Z",
        "version": version,
        "claims": claims,
        "evidence": [],
    })
}

fn write_json(dir: &std::path::Path, rel: &str, v: &Value) {
    let path = dot(dir).join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, serde_json::to_string_pretty(v).unwrap()).unwrap();
}

fn write_claim(dir: &std::path::Path, id: &str, version: i64) {
    write_json(dir, &format!("claims/{id}.json"), &claim_json(id, version));
}

fn write_deps(dir: &std::path::Path, edges: &Value) {
    write_json(
        dir,
        ".state/dependencies.json",
        &json!({"edges": edges, "stale": []}),
    );
}

fn read_deps(dir: &std::path::Path) -> Value {
    let p = dot(dir).join(".state/dependencies.json");
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

fn manifest_revision(dir: &std::path::Path) -> i64 {
    let p = dot(dir).join(".state/manifest.json");
    let m: Value = serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap();
    m["revision"].as_i64().unwrap_or(-1)
}

fn set_manifest_revision(dir: &std::path::Path, rev: i64) {
    let p = dot(dir).join(".state/manifest.json");
    let mut m: Value = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    m["revision"] = json!(rev);
    std::fs::write(&p, serde_json::to_string_pretty(&m).unwrap()).unwrap();
}

fn write_page(dir: &std::path::Path, kind: &str, name: &str, body: &str) {
    let rel = format!("wiki/{kind}/{name}");
    let text = format!("---\ntitle: {name}\ntype: {}\n---\n\n{body}\n", &kind[..kind.len() - 1]);
    let path = dot(dir).join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}

// ---------------------------------------------------------------------

#[test]
fn fix_unlisted_page_rebuilds_section_index() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    write_page(dir.path(), "concepts", "lonely-page.md", "A page with no links.");

    let (code, v) = doctor(dir.path(), &[]);
    assert_eq!(code, 0, "{v}");
    assert!(codes(&v).contains(&"unlisted_page".to_string()), "{v}");

    let (code, v) = doctor(dir.path(), &["--fix"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["ok"], json!(true), "{v}");
    assert!(
        fixed_actions(&v)
            .contains(&("unlisted_page".to_string(), "rebuild_section_indexes".to_string())),
        "{v}"
    );
    // Deferred codes are reported, not fixed.
    let rem = remaining_codes(&v);
    assert!(rem.contains(&"unregistered_page".to_string()), "{v}");

    // Re-lint: finding gone; index actually lists the page.
    let (code, v) = doctor(dir.path(), &[]);
    assert_eq!(code, 0, "{v}");
    assert!(!codes(&v).contains(&"unlisted_page".to_string()), "{v}");
    let idx = std::fs::read_to_string(dot(dir.path()).join("wiki/concepts/index.md")).unwrap();
    assert!(idx.contains("lonely-page.md"), "{idx}");
}

#[test]
fn fix_section_index_unlinked_relinks_overview() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    write_page(dir.path(), "concepts", "alpha.md", "Alpha page.");
    // A section index that already lists alpha but is not linked by overview.
    std::fs::write(
        dot(dir.path()).join("wiki/concepts/index.md"),
        "---\ntitle: Concepts\n---\n\n| Page | Title |\n|---|---|\n| [alpha](alpha.md) | Alpha |\n",
    )
    .unwrap();

    let (_, v) = doctor(dir.path(), &[]);
    assert!(
        codes(&v).contains(&"section_index_unlinked".to_string()),
        "{v}"
    );

    let (code, v) = doctor(dir.path(), &["--fix"]);
    assert_eq!(code, 0, "{v}");
    assert!(
        fixed_actions(&v)
            .contains(&("section_index_unlinked".to_string(), "rebuild_section_indexes".to_string())),
        "{v}"
    );
    let (_, v) = doctor(dir.path(), &[]);
    assert!(
        !codes(&v).contains(&"section_index_unlinked".to_string()),
        "{v}"
    );
    let overview = std::fs::read_to_string(dot(dir.path()).join("wiki/overview.md")).unwrap();
    assert!(overview.contains("concepts/index.md"), "{overview}");
}

#[test]
fn fix_unresolved_dependency_drops_dead_edge() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    write_page(dir.path(), "concepts", "p.md", "Depends on gone things.");
    write_deps(
        dir.path(),
        &json!({"wiki/concepts/p.md": ["wiki/concepts/gone.md", cid(999)]}),
    );

    let (_, v) = doctor(dir.path(), &[]);
    assert_eq!(
        codes(&v)
            .iter()
            .filter(|c| *c == "unresolved_dependency")
            .count(),
        2,
        "{v}"
    );

    let (code, v) = doctor(dir.path(), &["--fix"]);
    assert_eq!(code, 0, "{v}");
    assert!(
        fixed_actions(&v).iter().any(|(c, a)| c == "unresolved_dependency" && a == "drop_edge"),
        "{v}"
    );
    let deps = read_deps(dir.path());
    let list = deps["edges"]["wiki/concepts/p.md"].as_array().unwrap();
    assert!(list.is_empty(), "{deps}");

    let (_, v) = doctor(dir.path(), &[]);
    assert!(
        !codes(&v).contains(&"unresolved_dependency".to_string()),
        "{v}"
    );
}

#[test]
fn fix_unresolved_dependency_repins_stale_version() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let c1 = cid(1);
    write_claim(dir.path(), &c1, 2);
    write_page(dir.path(), "concepts", "p.md", "Agent-authored, not regenerable.");
    write_deps(dir.path(), &json!({"wiki/concepts/p.md": [format!("{c1}@1")]}));

    let (_, v) = doctor(dir.path(), &[]);
    assert!(
        codes(&v).contains(&"unresolved_dependency".to_string()),
        "{v}"
    );

    let (code, v) = doctor(dir.path(), &["--fix"]);
    assert_eq!(code, 0, "{v}");
    assert!(
        fixed_actions(&v)
            .iter()
            .any(|(c, a)| c == "unresolved_dependency" && a == "repin_dep_edge"),
        "{v}"
    );
    let deps = read_deps(dir.path());
    let list = deps["edges"]["wiki/concepts/p.md"].as_array().unwrap();
    assert_eq!(list, &vec![json!(format!("{c1}@2"))], "{deps}");

    let (_, v) = doctor(dir.path(), &[]);
    assert!(
        !codes(&v).contains(&"unresolved_dependency".to_string()),
        "{v}"
    );
}

#[test]
fn fix_missing_artifact_rebuilds_script_owned() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    // wiki/index.md is deterministic (build-pages renders it): a missing
    // registered artifact is rebuilt, not edge-dropped.
    write_deps(dir.path(), &json!({"wiki/index.md": []}));

    let (_, v) = doctor(dir.path(), &[]);
    assert!(codes(&v).contains(&"missing_artifact".to_string()), "{v}");

    let (code, v) = doctor(dir.path(), &["--fix"]);
    assert_eq!(code, 0, "{v}");
    assert!(
        fixed_actions(&v)
            .contains(&("missing_artifact".to_string(), "rebuild_artifact".to_string())),
        "{v}"
    );
    assert!(dot(dir.path()).join("wiki/index.md").is_file());
    let (_, v) = doctor(dir.path(), &[]);
    assert!(!codes(&v).contains(&"missing_artifact".to_string()), "{v}");
}

#[test]
fn fix_missing_artifact_drops_edge_when_not_regenerable() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    // Agent-authored page: no deterministic renderer — drop the edge.
    write_deps(dir.path(), &json!({"wiki/concepts/ghost.md": [cid(1)]}));

    let (_, v) = doctor(dir.path(), &[]);
    assert!(codes(&v).contains(&"missing_artifact".to_string()), "{v}");

    let (code, v) = doctor(dir.path(), &["--fix"]);
    assert_eq!(code, 0, "{v}");
    assert!(
        fixed_actions(&v).contains(&("missing_artifact".to_string(), "drop_edge".to_string())),
        "{v}"
    );
    let deps = read_deps(dir.path());
    assert!(deps["edges"].get("wiki/concepts/ghost.md").is_none(), "{deps}");
    let (_, v) = doctor(dir.path(), &[]);
    assert!(!codes(&v).contains(&"missing_artifact".to_string()), "{v}");
}

#[test]
fn fix_staging_leftover_deletes_only_dead_runs() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    // Dead run: no live-phase files → delete.
    let dead = dot(dir.path()).join(".state/staging/run_dead0001");
    std::fs::create_dir_all(&dead).unwrap();
    std::fs::write(dead.join("junk.txt"), "x").unwrap();
    // Live run: pending.json without compile.json (§5.5) → never touched.
    let live = dot(dir.path()).join(".state/staging/run_live0001");
    std::fs::create_dir_all(&live).unwrap();
    std::fs::write(live.join("pending.json"), "{}").unwrap();

    let (_, v) = doctor(dir.path(), &[]);
    assert_eq!(
        codes(&v)
            .iter()
            .filter(|c| *c == "staging_leftover")
            .count(),
        2,
        "{v}"
    );

    let (code, v) = doctor(dir.path(), &["--fix"]);
    assert_eq!(code, 0, "{v}");
    assert!(!dead.exists(), "dead staging dir deleted: {v}");
    assert!(live.exists(), "live run preserved: {v}");
    // The live run still reports as a finding in remaining[].
    let rem = remaining_codes(&v);
    assert!(rem.contains(&"staging_leftover".to_string()), "{v}");
}

#[test]
fn fix_journal_leftover_journal_only_deletes() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    // §5.4 journal-only branch: nothing was applied — delete journal(s)
    // plus the commit's staging dir, no restore.
    write_json(
        dir.path(),
        ".state/journal.json",
        &json!({"receipt": {"run_id": "run_jrnl001"}, "ops": []}),
    );
    write_json(
        dir.path(),
        ".state/journal-run_jrnl001.json",
        &json!({"receipt": {"run_id": "run_jrnl001"}, "ops": []}),
    );
    let staging = dot(dir.path()).join(".state/staging/run_jrnl001");
    std::fs::create_dir_all(&staging).unwrap();
    std::fs::write(staging.join("x.txt"), "{}").unwrap();

    let (_, v) = doctor(dir.path(), &[]);
    assert!(
        codes(&v).contains(&"transaction_journal_leftover".to_string()),
        "{v}"
    );

    let (code, v) = doctor(dir.path(), &["--fix"]);
    assert_eq!(code, 0, "{v}");
    assert!(
        fixed_actions(&v)
            .contains(&("transaction_journal_leftover".to_string(), "repair".to_string())),
        "{v}"
    );
    assert!(!dot(dir.path()).join(".state/journal.json").exists());
    assert!(!dot(dir.path())
        .join(".state/journal-run_jrnl001.json")
        .exists());
    assert!(!staging.exists(), "commit staging dir dropped: {v}");
    let (_, v) = doctor(dir.path(), &[]);
    assert!(
        !codes(&v).contains(&"transaction_journal_leftover".to_string()),
        "{v}"
    );
}

#[test]
fn fix_backup_leftover_restores_then_deletes() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let c1 = cid(1);
    write_claim(dir.path(), &c1, 2);
    // A leftover backup holds the pre-apply copy — repair restores it.
    let bak = dot(dir.path()).join(format!(".state/.backup-0/claims/{c1}.json"));
    std::fs::create_dir_all(bak.parent().unwrap()).unwrap();
    std::fs::write(&bak, serde_json::to_string_pretty(&claim_json(&c1, 1)).unwrap()).unwrap();
    write_json(
        dir.path(),
        ".state/journal.json",
        &json!({"receipt": {"run_id": "run_bak001"}, "ops": [["write", format!("claims/{c1}.json")]]}),
    );

    let (_, v) = doctor(dir.path(), &[]);
    let found = codes(&v);
    assert!(found.contains(&"stale_transaction_backup".to_string()), "{v}");
    assert!(
        found.contains(&"transaction_journal_leftover".to_string()),
        "{v}"
    );

    let (code, v) = doctor(dir.path(), &["--fix"]);
    assert_eq!(code, 0, "{v}");
    // Backup restored: version 1 content is back in place.
    let restored: Value = serde_json::from_str(
        &std::fs::read_to_string(dot(dir.path()).join(format!("claims/{c1}.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(restored["version"], json!(1), "{v}");
    assert!(!dot(dir.path()).join(".state/.backup-0").exists());
    assert!(!dot(dir.path()).join(".state/journal.json").exists());
    let (_, v) = doctor(dir.path(), &[]);
    let found = codes(&v);
    assert!(!found.contains(&"stale_transaction_backup".to_string()), "{v}");
    assert!(
        !found.contains(&"transaction_journal_leftover".to_string()),
        "{v}"
    );
}

#[test]
fn fix_malformed_frontmatter_defaults_only() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let page = dot(dir.path()).join("wiki/concepts/bad-page.md");
    std::fs::write(&page, "---\nno colon here\n---\n\nkeep this body\n").unwrap();

    let (code, v) = doctor(dir.path(), &[]);
    assert_eq!(code, 3, "{v}"); // error severity pre-fix
    assert!(
        codes(&v).contains(&"malformed_frontmatter".to_string()),
        "{v}"
    );

    let (code, v) = doctor(dir.path(), &["--fix"]);
    assert_eq!(code, 0, "{v}"); // exit-0-always even though ok was false
    assert!(
        fixed_actions(&v)
            .iter()
            .any(|(c, a)| c == "malformed_frontmatter" && a.contains("frontmatter")),
        "{v}"
    );
    let text = std::fs::read_to_string(&page).unwrap();
    assert!(text.contains("title: Bad Page"), "{text}");
    assert!(text.contains("type: concept"), "{text}");
    assert!(text.contains("tags: []"), "{text}");
    assert!(text.contains("keep this body"), "{text}");
    let (_, v) = doctor(dir.path(), &[]);
    assert!(
        !codes(&v).contains(&"malformed_frontmatter".to_string()),
        "{v}"
    );
}

#[test]
fn fix_index_stale_rebuilds_post_commit() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    write_claim(dir.path(), &cid(1), 1);
    // Index stamped behind the manifest → index_stale.
    {
        let wiki = llm_wiki::store::Wiki::open(dir.path()).unwrap();
        let idx = llm_wiki::index::Index::open_or_rebuild(&wiki).unwrap();
        idx.set_built_at_revision(-1).unwrap();
    }
    set_manifest_revision(dir.path(), 7);

    let (_, v) = doctor(dir.path(), &[]);
    assert!(codes(&v).contains(&"index_stale".to_string()), "{v}");

    let (code, v) = doctor(dir.path(), &["--fix"]);
    assert_eq!(code, 0, "{v}");
    assert!(
        fixed_actions(&v).contains(&("index_stale".to_string(), "rebuild_index".to_string())),
        "{v}"
    );
    // Post-commit rebuild stamps the CURRENT revision — no in-txn work
    // means no revision bump.
    {
        let wiki = llm_wiki::store::Wiki::open(dir.path()).unwrap();
        let idx = llm_wiki::index::Index::open_or_rebuild(&wiki).unwrap();
        assert_eq!(idx.built_at_revision().unwrap(), 7);
        let rows: i64 = idx
            .conn()
            .query_row("SELECT COUNT(*) FROM claims_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
    }
    let (_, v) = doctor(dir.path(), &[]);
    assert!(!codes(&v).contains(&"index_stale".to_string()), "{v}");
}

#[test]
fn fix_legacy_state_deletes_files() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    std::fs::write(dot(dir.path()).join(".state/claims-index.json"), "{}").unwrap();
    std::fs::write(dot(dir.path()).join(".state/fts.sqlite"), b"").unwrap();

    let (_, v) = doctor(dir.path(), &[]);
    assert_eq!(
        codes(&v).iter().filter(|c| *c == "legacy_state").count(),
        2,
        "{v}"
    );

    let (code, v) = doctor(dir.path(), &["--fix"]);
    assert_eq!(code, 0, "{v}");
    assert!(!dot(dir.path()).join(".state/claims-index.json").exists());
    assert!(!dot(dir.path()).join(".state/fts.sqlite").exists());
    let (_, v) = doctor(dir.path(), &[]);
    assert!(!codes(&v).contains(&"legacy_state".to_string()), "{v}");
}

#[test]
fn fix_stale_lock_deleted_commit_lock_untouched() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let locks = dot(dir.path()).join(".state/locks");
    std::fs::create_dir_all(&locks).unwrap();
    std::fs::write(locks.join("commit.lock"), b"").unwrap();
    std::fs::write(locks.join("bogus.lock"), b"").unwrap();

    let (_, v) = doctor(dir.path(), &[]);
    assert_eq!(
        codes(&v).iter().filter(|c| *c == "stale_lock").count(),
        1,
        "{v}"
    );

    let (code, v) = doctor(dir.path(), &["--fix"]);
    assert_eq!(code, 0, "{v}");
    assert!(!locks.join("bogus.lock").exists(), "stale lock deleted");
    assert!(
        locks.join("commit.lock").exists(),
        "§5.6: commit.lock is never deleted"
    );
    let (_, v) = doctor(dir.path(), &[]);
    assert!(!codes(&v).contains(&"stale_lock".to_string()), "{v}");
}

#[test]
fn fix_code_filter_limits_fix_set() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    write_page(dir.path(), "concepts", "lonely.md", "No index listing.");
    std::fs::write(dot(dir.path()).join(".state/claims-index.json"), "{}").unwrap();

    // --code legacy_state: only the legacy file is fixed.
    let (code, v) = doctor(dir.path(), &["--fix", "--code", "legacy_state"]);
    assert_eq!(code, 0, "{v}");
    assert!(!dot(dir.path()).join(".state/claims-index.json").exists());
    // The unlisted page was NOT fixed and stays in remaining.
    assert!(!dot(dir.path()).join("wiki/concepts/index.md").exists());
    assert!(
        remaining_codes(&v).contains(&"unlisted_page".to_string()),
        "{v}"
    );
    assert!(
        !fixed_actions(&v).iter().any(|(c, _)| c == "unlisted_page"),
        "{v}"
    );
}

#[test]
fn fix_scope_filters_before_fix_selection() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    write_page(dir.path(), "concepts", "lonely.md", "No index listing.");
    std::fs::write(dot(dir.path()).join(".state/claims-index.json"), "{}").unwrap();

    // Scope selects only legacy_state findings: the page is neither fixed
    // nor reported in remaining[] (§9.2).
    let (code, v) = doctor(dir.path(), &["--fix", "--scope", "legacy_state"]);
    assert_eq!(code, 0, "{v}");
    assert!(!dot(dir.path()).join(".state/claims-index.json").exists());
    assert!(!dot(dir.path()).join("wiki/concepts/index.md").exists());
    assert!(
        !remaining_codes(&v).contains(&"unlisted_page".to_string()),
        "scoped-out finding must not appear in remaining: {v}"
    );
}

#[test]
fn fix_dry_run_simulates_without_mutating() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    write_page(dir.path(), "concepts", "lonely.md", "No index listing.");
    write_deps(
        dir.path(),
        &json!({"wiki/concepts/lonely.md": ["wiki/concepts/gone.md"]}),
    );
    std::fs::write(dot(dir.path()).join(".state/claims-index.json"), "{}").unwrap();
    let rev_before = manifest_revision(dir.path());

    let (code, v) = doctor(dir.path(), &["--fix", "--dry-run"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["dry_run"], json!(true), "{v}");
    // Would-be world: index-fabric + dep fixes applied to the scratch
    // copy, so the re-lint reports the post-fix remainder only.
    let fixed = fixed_actions(&v);
    assert!(fixed.iter().any(|(c, _)| c == "unlisted_page"), "{v}");
    assert!(
        fixed.iter().any(|(c, _)| c == "unresolved_dependency"),
        "{v}"
    );
    assert!(
        !remaining_codes(&v).contains(&"unlisted_page".to_string()),
        "dry-run re-lints the post-fix world: {v}"
    );
    assert!(
        !remaining_codes(&v).contains(&"unresolved_dependency".to_string()),
        "{v}"
    );

    // Real wiki untouched: no index page, edges intact, revision same.
    assert!(!dot(dir.path()).join("wiki/concepts/index.md").exists());
    assert!(dot(dir.path()).join(".state/claims-index.json").exists());
    assert_eq!(manifest_revision(dir.path()), rev_before);
    let deps = read_deps(dir.path());
    assert_eq!(
        deps["edges"]["wiki/concepts/lonely.md"],
        json!(["wiki/concepts/gone.md"]),
        "{deps}"
    );
    let (_, v) = doctor(dir.path(), &[]);
    assert!(codes(&v).contains(&"unlisted_page".to_string()), "{v}");
}

#[test]
fn fix_legacy_flags_run_code_scoped_fixes() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    write_page(dir.path(), "concepts", "lonely.md", "No index listing.");
    let locks = dot(dir.path()).join(".state/locks");
    std::fs::create_dir_all(&locks).unwrap();
    std::fs::write(locks.join("bogus.lock"), b"").unwrap();

    // --fix-indexes fixes ONLY the index-fabric codes (§9.3).
    let (code, v) = doctor(dir.path(), &["--fix-indexes"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["deprecated_flag"], json!("--fix-indexes"), "{v}");
    assert_eq!(v["ok"], json!(true), "{v}");
    assert!(dot(dir.path()).join("wiki/concepts/index.md").exists());
    assert!(
        locks.join("bogus.lock").exists(),
        "stale_lock is outside --fix-indexes' code set: {v}"
    );
    assert!(
        remaining_codes(&v).contains(&"stale_lock".to_string()),
        "{v}"
    );
}

#[test]
fn fix_rebuild_index_flag_maps_to_index_codes() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    {
        let wiki = llm_wiki::store::Wiki::open(dir.path()).unwrap();
        let idx = llm_wiki::index::Index::open_or_rebuild(&wiki).unwrap();
        idx.set_built_at_revision(-1).unwrap();
    }
    set_manifest_revision(dir.path(), 3);

    let (code, v) = doctor(dir.path(), &["--rebuild-index"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["deprecated_flag"], json!("--rebuild-index"), "{v}");
    assert!(
        fixed_actions(&v).contains(&("index_stale".to_string(), "rebuild_index".to_string())),
        "{v}"
    );
    let (_, v) = doctor(dir.path(), &[]);
    assert!(!codes(&v).contains(&"index_stale".to_string()), "{v}");
}

#[test]
fn fix_deferred_codes_stay_in_remaining() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    // orphan_claim: no evidence — §9.4 deferred, never fixed.
    write_claim(dir.path(), &cid(1), 1);

    let (_, v) = doctor(dir.path(), &[]);
    assert!(codes(&v).contains(&"orphan_claim".to_string()), "{v}");

    let (code, v) = doctor(dir.path(), &["--fix"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["ok"], json!(true), "{v}");
    assert!(
        remaining_codes(&v).contains(&"orphan_claim".to_string()),
        "{v}"
    );
    assert!(
        !fixed_actions(&v).iter().any(|(c, _)| c == "orphan_claim"),
        "{v}"
    );
    let (_, v) = doctor(dir.path(), &[]);
    assert!(codes(&v).contains(&"orphan_claim".to_string()), "{v}");
}

#[test]
fn fix_rebuilds_decision_page_repinning_deps() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let c1 = cid(1);
    let d1 = did(1);
    write_claim(dir.path(), &c1, 2);
    write_json(
        dir.path(),
        &format!("decisions/{d1}.json"),
        &decision_json(&d1, &[&c1], 1),
    );
    // The script-owned decision page is missing AND its registered pin is
    // stale — both findings share one rebuild.
    write_deps(
        dir.path(),
        &json!({format!("wiki/decisions/{d1}.md"): [format!("{d1}@1"), format!("{c1}@1")]}),
    );

    let (_, v) = doctor(dir.path(), &[]);
    let found = codes(&v);
    assert!(found.contains(&"missing_artifact".to_string()), "{v}");
    assert!(found.contains(&"unresolved_dependency".to_string()), "{v}");

    let (code, v) = doctor(dir.path(), &["--fix"]);
    assert_eq!(code, 0, "{v}");
    assert!(
        dot(dir.path())
            .join(format!("wiki/decisions/{d1}.md"))
            .is_file(),
        "{v}"
    );
    // Registry pins regenerated at current versions.
    let deps = read_deps(dir.path());
    let list = deps["edges"][format!("wiki/decisions/{d1}.md")]
        .as_array()
        .unwrap();
    assert_eq!(
        list,
        &vec![json!(format!("{d1}@1")), json!(format!("{c1}@2"))],
        "{deps}"
    );
    let (_, v) = doctor(dir.path(), &[]);
    let found = codes(&v);
    assert!(!found.contains(&"missing_artifact".to_string()), "{v}");
    assert!(!found.contains(&"unresolved_dependency".to_string()), "{v}");
}
