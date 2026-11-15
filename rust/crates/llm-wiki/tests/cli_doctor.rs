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
    let v: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout not JSON ({e}): {}", String::from_utf8_lossy(&out.stdout)));
    (code, v)
}

fn doctor(dir: &std::path::Path, extra: &[&str]) -> (i32, Value) {
    let mut args = vec!["--root", dir.to_str().unwrap(), "doctor"];
    args.extend_from_slice(extra);
    run_json(&args)
}

fn codes(v: &Value) -> Vec<String> {
    v["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["code"].as_str().unwrap().to_string())
        .collect()
}

fn write_claim(dir: &std::path::Path, name: &str, claim: &Value) {
    let path = dir.join(".llm-wiki/claims").join(name);
    std::fs::write(path, serde_json::to_string_pretty(claim).unwrap()).unwrap();
}

/// Schema-valid claim whose file name does not match its `id`.
fn claim_json(id: &str) -> Value {
    json!({
        "id": id,
        "subject": "subject one",
        "predicate": "pred",
        "value": "v",
        "status": "candidate",
        "authority": {"type": "authoritative_source"},
        "evidence": [],
        "recorded_at": "2026-11-01T00:00:00Z",
        "version": 1,
    })
}

#[test]
fn doctor_clean_wiki_ok() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let (code, v) = doctor(dir.path(), &[]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["ok"], true, "{v}");
    assert_eq!(v["findings"], json!([]), "{v}");
    assert_eq!(v["counts"], json!({"error": 0, "warn": 0, "info": 0}), "{v}");
}

#[test]
fn doctor_invalid_id_error_exit3() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    // Schema-valid object, wrong file name → invalid_id (error) → exit 3.
    write_claim(
        dir.path(),
        "claim_wrongname.json",
        &claim_json("claim_3G8HMPTVXN7ZKDJW4QRYF2SABC"),
    );
    let (code, v) = doctor(dir.path(), &[]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["ok"], false);
    assert!(codes(&v).contains(&"invalid_id".to_string()), "{v}");
    assert_eq!(v["counts"]["error"].as_i64().unwrap() >= 1, true, "{v}");
}

#[test]
fn doctor_corrupt_claim_json_schema_invalid() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    std::fs::write(
        dir.path().join(".llm-wiki/claims/claim_4G8HMPTVXN7ZKDJW4QRYF2SABC.json"),
        "{ not json",
    )
    .unwrap();
    let (code, v) = doctor(dir.path(), &[]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["ok"], false);
    assert!(codes(&v).contains(&"schema_invalid".to_string()), "{v}");
}

#[test]
fn doctor_scope_filters_and_recounts() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    // Two unrelated findings: a staging leftover (info) + a corrupt claim.
    std::fs::create_dir_all(dir.path().join(".llm-wiki/.state/staging/run_zzz"))
        .unwrap();
    std::fs::write(
        dir.path().join(".llm-wiki/claims/claim_5G8HMPTVXN7ZKDJW4QRYF2SABC.json"),
        "{ broken",
    )
    .unwrap();

    let (code, v) = doctor(dir.path(), &["--scope", "run_zzz"]);
    assert_eq!(code, 0, "{v}");
    let found = codes(&v);
    assert_eq!(found, vec!["staging_leftover"], "{v}");
    assert_eq!(v["ok"], true);
    assert_eq!(v["counts"]["info"], 1);
    assert_eq!(v["counts"]["error"], 0);
}

#[test]
fn doctor_missing_index_emits_nothing() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    assert!(!dir.path().join(".llm-wiki/.state/index.sqlite").exists());
    let (code, v) = doctor(dir.path(), &[]);
    assert_eq!(code, 0, "{v}");
    assert!(!codes(&v).contains(&"index_stale".to_string()), "{v}");
}

#[test]
fn doctor_stale_index_flagged() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    // Build an index stamped at revision 0 while the manifest has moved on.
    std::fs::create_dir_all(dir.path().join(".llm-wiki/.state")).unwrap();
    {
        let wiki = llm_wiki::store::Wiki::open(dir.path()).unwrap();
        let idx = llm_wiki::index::Index::open_or_rebuild(&wiki).unwrap();
        idx.set_built_at_revision(-1).unwrap();
    }
    // Bump the manifest revision so the watermark is behind.
    let manifest_path = dir.path().join(".llm-wiki/.state/manifest.json");
    let mut m: Value =
        serde_json::from_str(&std::fs::read_to_string(&manifest_path).unwrap()).unwrap();
    m["revision"] = json!(7);
    std::fs::write(&manifest_path, serde_json::to_string_pretty(&m).unwrap()).unwrap();

    let (code, v) = doctor(dir.path(), &[]);
    assert_eq!(code, 0, "{v}");
    assert!(codes(&v).contains(&"index_stale".to_string()), "{v}");
}

#[test]
fn doctor_corrupt_index_flagged() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    std::fs::write(
        dir.path().join(".llm-wiki/.state/index.sqlite"),
        b"not a sqlite db",
    )
    .unwrap();
    let (code, v) = doctor(dir.path(), &[]);
    assert_eq!(code, 0, "{v}");
    assert!(codes(&v).contains(&"index_stale".to_string()), "{v}");
}

#[test]
fn doctor_legacy_state_files() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    std::fs::write(dir.path().join(".llm-wiki/.state/claims-index.json"), "{}").unwrap();
    std::fs::write(dir.path().join(".llm-wiki/.state/fts.sqlite"), b"").unwrap();
    let (code, v) = doctor(dir.path(), &[]);
    assert_eq!(code, 0, "{v}");
    let found = codes(&v);
    assert_eq!(found.iter().filter(|c| *c == "legacy_state").count(), 2, "{v}");
}

#[test]
fn doctor_deprecated_fix_flags() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    // §9.3: legacy flags run the fix path, emit identical output plus
    // deprecated_flag. On a clean wiki there is nothing to fix.
    for flag in ["--fix-indexes", "--rebuild-index", "--rebuild-fts"] {
        let (code, v) = doctor(dir.path(), &[flag]);
        assert_eq!(code, 0, "{flag}: {v}");
        assert_eq!(v["deprecated_flag"], json!(flag), "{flag}: {v}");
        assert_eq!(v["ok"], true, "{flag}: {v}");
        assert!(v["fixed"].is_array(), "{flag}: {v}");
        assert!(v["remaining"].is_array(), "{flag}: {v}");
        assert!(v.get("fix_pending").is_none(), "{flag}: {v}");
    }
    // --fix is the real flag: no deprecated_flag, ok always true.
    let (code, v) = doctor(dir.path(), &["--fix"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["ok"], true, "{v}");
    assert!(v.get("deprecated_flag").is_none(), "{v}");
    // Modifier flags alone still take the fix path.
    let (code, v) = doctor(dir.path(), &["--code", "index_stale"]);
    assert_eq!(code, 0, "{v}");
    assert!(v["fixed"].is_array(), "{v}");
}

#[test]
fn doctor_wiki_page_lints() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    // An agent-authored page not listed in a section index + no edges.
    std::fs::write(
        dir.path().join(".llm-wiki/wiki/concepts/lonely-page.md"),
        "---\ntitle: Lonely\n---\n\nA page with no links.\n",
    )
    .unwrap();
    let (code, v) = doctor(dir.path(), &[]);
    assert_eq!(code, 0, "{v}");
    let found = codes(&v);
    assert!(found.contains(&"unregistered_page".to_string()), "{v}");
    assert!(found.contains(&"unlisted_page".to_string()), "{v}");
    assert!(found.contains(&"dead_end_page".to_string()), "{v}");
    // Single page wiki: no orphan flag.
    assert!(!found.contains(&"orphan_page".to_string()), "{v}");
}

#[test]
fn doctor_malformed_frontmatter_is_error() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    std::fs::write(
        dir.path().join(".llm-wiki/wiki/concepts/bad-page.md"),
        "---\nno colon here\n---\n\nbody\n",
    )
    .unwrap();
    let (code, v) = doctor(dir.path(), &[]);
    assert_eq!(code, 3, "{v}");
    assert!(codes(&v).contains(&"malformed_frontmatter".to_string()), "{v}");
}
