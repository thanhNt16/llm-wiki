//! Global CLI flags (spec §6.1): root precedence `--root` >
//! `LLM_WIKI_ROOT` > cwd (no ancestor walk), and `--allow-repair`
//! bypassing the leftover-journal commit gate on txn-owning commands.

mod common;

use std::process::Command;

use serde_json::{json, Value};
use tempfile::TempDir;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_llm-wiki"))
}

fn run_json(cmd: &mut Command) -> (i32, Value) {
    let out = cmd.output().unwrap();
    let code = out.status.code().unwrap_or(-1);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!("stdout not JSON ({e}): {:?}", String::from_utf8_lossy(&out.stdout))
    });
    (code, v)
}

fn wiki_dir() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    let (code, v) = run_json(bin().args(["init", dir.path().to_str().unwrap()]));
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["initialized"], true, "{v}");
    dir
}

fn sources_count(dir: &TempDir) -> i64 {
    let (code, v) = run_json(bin().args(["--root", dir.path().to_str().unwrap(), "status"]));
    assert_eq!(code, 0, "{v}");
    v["counts"]["sources"].as_i64().unwrap()
}

/// Leftover journal files = a crashed prior commit → repair gate trips
/// (txn/recovery.rs scan: `journal.json` + `journal-<run>.json`).
fn plant_journal(dir: &TempDir) {
    let payload = serde_json::to_string_pretty(&json!({
        "receipt": {"run_id": "run_crashed", "revision": {"before": 0, "after": 1}},
        "ops": [],
    }))
    .unwrap()
        + "\n";
    let state = dir.path().join(".llm-wiki/.state");
    std::fs::write(state.join("journal.json"), &payload).unwrap();
    std::fs::write(state.join("journal-run_crashed.json"), &payload).unwrap();
}

#[test]
fn llm_wiki_root_used_when_no_root_flag() {
    let wiki = wiki_dir();
    let other = tempfile::tempdir().unwrap();
    // run from a directory that is NOT a wiki; only LLM_WIKI_ROOT points
    // at the real one
    let (code, v) = run_json(
        bin()
            .current_dir(other.path())
            .env("LLM_WIKI_ROOT", wiki.path())
            .args(["ingest", "--text", "env-root evidence"]),
    );
    assert_eq!(code, 0, "{v}");
    assert!(
        v["source_id"].as_str().unwrap().starts_with("source_"),
        "{v}"
    );
    assert_eq!(sources_count(&wiki), 1);
}

#[test]
fn root_flag_beats_llm_wiki_root() {
    let wiki_flag = wiki_dir();
    let wiki_env = wiki_dir();
    let (code, v) = run_json(
        bin()
            .env("LLM_WIKI_ROOT", wiki_env.path())
            .args([
                "--root",
                wiki_flag.path().to_str().unwrap(),
                "ingest",
                "--text",
                "flag wins",
            ]),
    );
    assert_eq!(code, 0, "{v}");
    assert_eq!(sources_count(&wiki_flag), 1, "--root was ignored");
    assert_eq!(sources_count(&wiki_env), 0, "env root was used over --root");
}

#[test]
fn cwd_used_when_no_root_or_env() {
    let wiki = wiki_dir();
    let (code, v) = run_json(
        bin()
            .current_dir(wiki.path())
            .env_remove("LLM_WIKI_ROOT")
            .args(["ingest", "--text", "cwd evidence"]),
    );
    assert_eq!(code, 0, "{v}");
    assert_eq!(sources_count(&wiki), 1);
}

#[test]
fn allow_repair_bypasses_leftover_journal() {
    let dir = wiki_dir();
    let root = dir.path().to_str().unwrap();
    plant_journal(&dir);

    // gate trips without the flag → repair_required exit 3
    let (code, v) = run_json(bin().args(["--root", root, "ingest", "--text", "x"]));
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["error_type"], "repair_required", "{v}");

    // with --allow-repair the commit proceeds and clears the journals
    let (code, v) = run_json(
        bin().args(["--root", root, "--allow-repair", "ingest", "--text", "x"]),
    );
    assert_eq!(code, 0, "{v}");
    assert!(
        v["source_id"].as_str().unwrap().starts_with("source_"),
        "{v}"
    );
    assert!(!dir.path().join(".llm-wiki/.state/journal.json").exists());
}

#[test]
fn allow_repair_flag_position_after_subcommand() {
    // global flag parses after the subcommand too
    let dir = wiki_dir();
    let root = dir.path().to_str().unwrap();
    plant_journal(&dir);
    let (code, v) = run_json(
        bin().args(["ingest", "--root", root, "--allow-repair", "--text", "x"]),
    );
    assert_eq!(code, 0, "{v}");
}

#[test]
fn non_mutating_command_with_wait_lock_is_usage_2() {
    let dir = wiki_dir();
    let root = dir.path().to_str().unwrap();
    for args in [
        vec!["--root", root, "status", "--wait-lock"],
        vec!["--root", root, "status", "--no-wait"],
        vec!["--root", root, "topics", "--wait-lock"],
        vec!["--root", root, "check-evidence", "--wait-lock"],
        vec!["--root", root, "ingest", "--list-connectors", "--wait-lock"],
    ] {
        let (code, v) = run_json(bin().args(&args));
        assert_eq!(code, 2, "{args:?}: {v}");
        assert_eq!(v["error_type"], "usage", "{args:?}: {v}");
    }
}
