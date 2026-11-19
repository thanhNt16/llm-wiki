//! `wiki` binary end-to-end — the wiki.py-shaped surface (issue #15):
//! init / ingest / compile / query-prepare over llm-wiki-core, one JSON
//! object on stdout, wiki.py exit codes.

use std::process::Command;

use serde_json::Value;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_wiki"))
}

fn run_json(root: &std::path::Path, args: &[&str]) -> (i32, Value) {
    let out = bin()
        .arg("--root")
        .arg(root)
        .args(args)
        .output()
        .unwrap();
    let code = out.status.code().unwrap_or(-1);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    (code, v)
}

fn init(root: &std::path::Path) {
    let (code, v) = run_json(root, &["init"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["initialized"], true, "{v}");
}

#[test]
fn init_idempotent_names_root_and_reinits_cleanly() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let (code, v) = run_json(dir.path(), &["init", "--name", "renamed"]);
    assert_eq!(code, 0, "{v}");
    // Idempotent: second init reports already-initialized, never resets.
    assert_eq!(v["initialized"], false, "{v}");
    // --name wins on a fresh init (project lands in wiki.json config;
    // wiki.py's init receipt itself carries only initialized/revision/root).
    let dir2 = tempfile::tempdir().unwrap();
    let (code, _) = run_json(dir2.path(), &["init", "--name", "shop-platform"]);
    assert_eq!(code, 0);
    let cfg: Value = serde_json::from_str(
        &std::fs::read_to_string(dir2.path().join(".llm-wiki/wiki.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(cfg["project"], "shop-platform", "{cfg}");
}

#[test]
fn ingest_text_receipt_and_dedup() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let (code, v) = run_json(
        dir.path(),
        &["ingest", "--text", "use Postgres for orders", "--text-ref", "chat-1"],
    );
    assert_eq!(code, 0, "{v}");
    for k in ["source_id", "version", "sha256", "deduplicated", "warnings"] {
        assert!(v.get(k).is_some(), "receipt missing {k}: {v}");
    }
    assert_eq!(v["deduplicated"], false, "{v}");
    let (code, v2) = run_json(
        dir.path(),
        &["ingest", "--text", "use Postgres for orders", "--text-ref", "chat-1"],
    );
    assert_eq!(code, 0, "{v2}");
    assert_eq!(v2["deduplicated"], true, "{v2}");
}

#[test]
fn ingest_requires_input_usage() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let (code, v) = run_json(dir.path(), &["ingest"]);
    assert_eq!(code, 3, "{v}");
    assert!(v["error"].as_str().unwrap().contains("--file"), "{v}");
}

#[test]
fn compile_then_query_prepare_end_to_end() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    run_json(dir.path(), &["ingest", "--text", "the api uses tokens"]);
    let (code, v) = run_json(dir.path(), &["compile"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["phase"], "extract", "{v}");
    let (code, v) = run_json(
        dir.path(),
        &["query-prepare", "--question", "what does the api use?"],
    );
    assert_eq!(code, 0, "{v}");
    for k in ["matches", "gaps"] {
        assert!(v.get(k).is_some(), "query-prepare missing {k}: {v}");
    }
    // `query` alias resolves to the same command.
    let (code_a, v_a) = run_json(
        dir.path(),
        &["query", "--question", "what does the api use?"],
    );
    assert_eq!(code_a, 0, "{v_a}");
    assert_eq!(v_a, v, "{v_a}");
}

#[test]
fn usage_errors_are_json_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    // unknown flag → usage, JSON payload on stdout, exit 2
    let out = bin()
        .arg("--root")
        .arg(dir.path())
        .arg("init")
        .arg("--nope")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["exit_code"], 2, "{v}");
    // missing required --question → usage
    let (code, v) = run_json(dir.path(), &["query-prepare"]);
    assert_eq!(code, 2, "{v}");
    assert_eq!(v["exit_code"], 2, "{v}");
}

#[test]
fn no_wait_on_read_only_command_is_accepted() {
    let dir = tempfile::tempdir().unwrap();
    // query-prepare is read-only: wiki.py never waits for the lock there,
    // and the flag is simply inert rather than a usage error.
    let (code, _) = run_json(
        dir.path(),
        &["--no-wait", "query-prepare", "--question", "q"],
    );
    assert_eq!(code, 0);
}
