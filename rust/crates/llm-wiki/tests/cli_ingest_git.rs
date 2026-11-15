use std::process::Command;

use serde_json::Value;

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

fn git(repo: &std::path::Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Temp git repo with `n` commits (a.txt grows per commit) and a commit
/// that touches a second file so changed-files metadata is exercised.
fn make_repo(commits: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["init", "-q", "-b", "main"]);
    git(repo, &["config", "user.email", "t@example.com"]);
    git(repo, &["config", "user.name", "T User"]);
    git(repo, &["config", "commit.gpgsign", "false"]);
    for i in 0..commits {
        std::fs::write(repo.join("a.txt"), format!("line {i}\n")).unwrap();
        git(repo, &["add", "a.txt"]);
        if i == 1 {
            std::fs::write(repo.join("b.txt"), "b\n").unwrap();
            git(repo, &["add", "b.txt"]);
        }
        git(repo, &["commit", "-q", "-m", &format!("commit {i}")]);
    }
    dir
}

#[test]
fn ingest_git_ingests_commits() {
    let wiki_dir = tempfile::tempdir().unwrap();
    init(wiki_dir.path());
    let repo = make_repo(3);

    let (code, v) = run_json(&[
        "--root",
        wiki_dir.path().to_str().unwrap(),
        "ingest-git",
        repo.path().to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["connector"], "git");
    assert_eq!(v["items"], 3);
    assert_eq!(v["ingested"], 3);
    assert_eq!(v["deduplicated"], 0);
    assert_eq!(v["sources"].as_array().unwrap().len(), 3);
    assert!(v["repo"].as_str().unwrap().ends_with(
        repo.path().file_name().unwrap().to_str().unwrap()
    ));

    // re-run: same commit content dedups per item
    let (code, v) = run_json(&[
        "--root",
        wiki_dir.path().to_str().unwrap(),
        "ingest-git",
        "--repo",
        repo.path().to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["items"], 3);
    assert_eq!(v["deduplicated"], 3);
    assert_eq!(v["ingested"], 0);
}

#[test]
fn ingest_git_max_limits_items() {
    let wiki_dir = tempfile::tempdir().unwrap();
    init(wiki_dir.path());
    let repo = make_repo(3);
    let (code, v) = run_json(&[
        "--root",
        wiki_dir.path().to_str().unwrap(),
        "ingest-git",
        repo.path().to_str().unwrap(),
        "--max",
        "1",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["items"], 1);
}

#[test]
fn ingest_git_non_repo_is_txn() {
    let wiki_dir = tempfile::tempdir().unwrap();
    init(wiki_dir.path());
    let not_repo = tempfile::tempdir().unwrap();
    let (code, v) = run_json(&[
        "--root",
        wiki_dir.path().to_str().unwrap(),
        "ingest-git",
        not_repo.path().to_str().unwrap(),
    ]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["error_type"], "txn");
    assert!(v["error"].as_str().unwrap().contains("git connector"), "{v}");
}

#[test]
fn ingest_git_requires_initialized_wiki() {
    let dir = tempfile::tempdir().unwrap();
    let repo = make_repo(1);
    let (code, v) = run_json(&[
        "--root",
        dir.path().to_str().unwrap(),
        "ingest-git",
        repo.path().to_str().unwrap(),
    ]);
    assert_eq!(code, 3, "{v}");
    assert!(v["error"].as_str().unwrap().contains("not initialized"), "{v}");
}
