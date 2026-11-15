//! `--wait-lock` end-to-end: a real second process holds `commit.lock`
//! (test-side `Lock::acquire` — same kernel flock the CLI contends on),
//! then the CLI either exits 5 immediately, waits it out, or rejects the
//! flag on non-transaction commands (Task 12 / spec §5.2+§6.1).

mod common;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use llm_wiki::txn::lock::Lock;
use serde_json::Value;
use tempfile::TempDir;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_llm-wiki"))
}

/// Initialized wiki with `commit.lock` HELD by this process until the
/// returned guard drops — flock is advisory+cross-process, so the test
/// process acts as "process A" for spawned CLI children.
fn locked_wiki() -> (TempDir, Lock) {
    let (dir, wiki) = common::wiki();
    let lock = Lock::acquire(&wiki).unwrap();
    (dir, lock)
}

fn ingest_args(dir: &TempDir) -> Vec<String> {
    vec![
        "--root".into(),
        dir.path().to_str().unwrap().into(),
        "ingest".into(),
        "--text".into(),
        "lock probe evidence".into(),
    ]
}

#[test]
fn held_lock_without_wait_flag_exits_5_locked_json() {
    let (dir, _a) = locked_wiki();
    let out = bin().args(ingest_args(&dir)).output().unwrap();
    assert_eq!(out.status.code(), Some(5), "{:?}", out);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["exit_code"], 5, "{v}");
    assert_eq!(v["error_type"], "locked", "{v}");
}

#[test]
fn wait_lock_blocks_until_release_then_succeeds() {
    let (dir, a) = locked_wiki();
    let mut args = ingest_args(&dir);
    args.push("--wait-lock".into());
    let mut child = bin()
        .args(&args)
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();

    std::thread::sleep(Duration::from_millis(700));
    assert!(
        child.try_wait().unwrap().is_none(),
        "child exited while commit.lock was still held"
    );

    drop(a); // process A releases
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{:?}", out);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(v["source_id"].as_str().unwrap().starts_with("source_"), "{v}");
}

#[test]
fn wait_lock_secs_times_out_to_exit_5() {
    let (dir, _a) = locked_wiki();
    let start = Instant::now();
    let mut args = ingest_args(&dir);
    args.push("--wait-lock=0.6".into());
    let out = bin().args(&args).output().unwrap();
    let elapsed = start.elapsed();
    assert_eq!(out.status.code(), Some(5), "{:?}", out);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["error_type"], "locked", "{v}");
    assert!(
        elapsed >= Duration::from_millis(500) && elapsed < Duration::from_secs(10),
        "waited {elapsed:?}"
    );
}

#[test]
fn wait_lock_on_non_txn_commands_is_usage_exit_2() {
    let (dir, _a) = locked_wiki();
    let root = dir.path().to_str().unwrap();
    // read-only commands and init (scaffolds, never transacts) reject the
    // flag instead of silently ignoring it.
    for extra in [
        vec!["status", "--wait-lock"],
        vec!["topics", "--wait-lock"],
        vec!["init", "--wait-lock"],
        vec!["ingest", "--list-connectors", "--wait-lock"],
        vec!["status", "--no-wait"],
    ] {
        let mut args = vec!["--root", root];
        args.extend(extra.iter().copied());
        let out = bin().args(&args).output().unwrap();
        assert_eq!(out.status.code(), Some(2), "{extra:?}: {:?}", out);
        let v: Value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{extra:?} stdout not JSON ({e})"));
        assert_eq!(v["error_type"], "usage", "{extra:?}: {v}");
    }
}

#[test]
fn wait_lock_on_topics_backfill_waits_for_lock() {
    let (dir, a) = locked_wiki();
    let mut child = bin()
        .args([
            "--root",
            dir.path().to_str().unwrap(),
            "topics",
            "--backfill",
            "--wait-lock",
        ])
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(400));
    assert!(child.try_wait().unwrap().is_none(), "backfill did not wait");
    drop(a);
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{:?}", out);
}

#[test]
fn no_wait_overrides_wait_lock() {
    let (dir, _a) = locked_wiki();
    let mut args = ingest_args(&dir);
    args.push("--wait-lock".into());
    args.push("--no-wait".into());
    let out = bin().args(&args).output().unwrap();
    assert_eq!(out.status.code(), Some(5), "{:?}", out);
}

#[test]
fn wait_lock_before_subcommand_still_parses() {
    // global flag + bare value must not swallow the subcommand token.
    let (dir, _wiki) = common::wiki();
    let out = bin()
        .args([
            "--wait-lock",
            "ingest",
            "--root",
            dir.path().to_str().unwrap(),
            "--text",
            "x",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{:?}", out);
}
