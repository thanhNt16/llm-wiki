use std::process::Command;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_llm-wiki"))
}

#[test]
fn init_creates_wiki_and_reports() {
    let dir = tempfile::tempdir().unwrap();
    let out = bin().arg("init").arg(dir.path()).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["initialized"], true);
    assert_eq!(v["revision"], 0);
    assert!(dir.path().join(".llm-wiki/wiki.json").exists());
}

#[test]
fn init_is_idempotent() {
    let dir = tempfile::tempdir().unwrap();
    bin().arg("init").arg(dir.path()).output().unwrap();
    let out = bin().arg("init").arg(dir.path()).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["initialized"], false);
    assert_eq!(v["revision"], 0);
}

#[test]
fn usage_error_is_json_on_stdout_exit_2() {
    let out = bin().output().unwrap(); // no subcommand
    assert_eq!(out.status.code(), Some(2));
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["exit_code"], 2);
}

#[test]
fn init_scaffolds_full_layout() {
    let dir = tempfile::tempdir().unwrap();
    bin().arg("init").arg(dir.path()).output().unwrap();
    let dot = dir.path().join(".llm-wiki");
    for d in [
        "raw/files",
        "raw/urls",
        "raw/repositories",
        "raw/sessions",
        "raw/text",
        "sources",
        "claims",
        "decisions",
        "notes",
        "wiki/concepts",
        "wiki/entities",
        "wiki/decisions",
        "wiki/procedures",
        "wiki/questions",
        "wiki/changes",
        "context",
        "views",
        ".state/locks",
        ".state/operations",
        ".state/staging",
    ] {
        assert!(dot.join(d).is_dir(), "missing {}", d);
    }
    for f in [
        ".state/aliases.json",
        ".state/dependencies.json",
        ".state/graph.json",
        ".state/operations.jsonl",
        ".state/review-queue.jsonl",
        ".state/manifest.json",
        "page-kinds.json",
        "README.md",
        "wiki.json",
    ] {
        assert!(dot.join(f).is_file(), "missing {}", f);
    }
    let manifest: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dot.join(".state/manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["revision"], 0);
    assert_eq!(manifest["skill_version"], "0.3.0");
}

#[test]
fn init_partial_scaffold_completes() {
    // crash-window: dirs exist, no wiki.json → fill, return initialized:true
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dir.path().join(".llm-wiki/sources")).unwrap();
    let out = bin().arg("init").arg(dir.path()).output().unwrap();
    let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["initialized"], true);
}
