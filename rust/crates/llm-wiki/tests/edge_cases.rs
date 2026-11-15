//! Phase-1 hardening edges — weird-but-real inputs must produce clean
//! JSON errors (exit 2/3/4/5), a valid receipt, or documented parity
//! behavior. Never a panic, never non-JSON stdout.

mod common;

use std::ffi::OsStr;
use std::process::Command;

use llm_wiki::err::EngineError;
use llm_wiki::txn::Txn;
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

// ---- root resolution / init --------------------------------------------

#[test]
fn status_on_missing_root_reports_uninitialized() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("does-not-exist");
    let (code, v) = run_json(&["--root", missing.to_str().unwrap(), "status"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v, serde_json::json!({"initialized": false}));
}

#[test]
fn root_pointing_at_a_file_is_usage() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("not-a-dir.txt");
    std::fs::write(&file, b"x").unwrap();
    let root = file.to_str().unwrap();
    for args in [
        vec!["--root", root, "status"],
        vec!["--root", root, "ingest", "--text", "x"],
        vec!["init", root], // positional path takes the same Wiki::open gate
    ] {
        let (code, v) = run_json(&args);
        assert_eq!(code, 2, "{args:?} -> {v}");
        assert_eq!(v["error_type"], "usage", "{v}");
    }
}

#[test]
fn init_inside_dot_llm_wiki_stays_cwd_scoped() {
    // CLI root discovery is cwd-only — running init INSIDE a live
    // `.llm-wiki` must scaffold a new nested wiki in place, not find the
    // ancestor wiki (spec §6.1: ancestor walk is MCP-only).
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let dot = dir.path().join(".llm-wiki");
    let out = bin().arg("init").current_dir(&dot).output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(0), "{v}");
    assert_eq!(v["initialized"], true, "{v}");
    assert!(dot.join(".llm-wiki/wiki.json").is_file());
    // the parent wiki's manifest is untouched
    let manifest: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join(".llm-wiki/.state/manifest.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(manifest["revision"], 0);
}

#[test]
fn concurrent_init_is_tolerated() {
    // init is an idempotent, non-destructive scaffold — two racing
    // processes both writing identical scaffold bytes must not corrupt or
    // crash. Both exit 0 (the fixed `.tmp` rename race is benign); a clean
    // exit-5 is also acceptable per the hardening contract.
    let dir = tempfile::tempdir().unwrap();
    let spawn = || {
        bin()
            .arg("init")
            .arg(dir.path())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .unwrap()
    };
    let a_child = spawn();
    let b_child = spawn();
    let a = a_child.wait_with_output().unwrap();
    let b = b_child.wait_with_output().unwrap();
    for out in [a, b] {
        let code = out.status.code().unwrap_or(-1);
        assert!(
            code == 0 || code == 5,
            "init raced to exit {code}: {}",
            String::from_utf8_lossy(&out.stdout)
        );
        serde_json::from_slice::<Value>(&out.stdout)
            .expect("racing init must still emit JSON");
    }
    assert!(dir.path().join(".llm-wiki/wiki.json").is_file());
    let (code, v) = run_json(&["--root", dir.path().to_str().unwrap(), "status"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["initialized"], true);
}

#[test]
fn unicode_root_and_filename_ingest() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("projekt-δelta-");
    std::fs::create_dir(&root).unwrap();
    init(&root);
    let doc = root.join("résumé-.txt");
    std::fs::write(&doc, "ünïcodé content — こんにちは").unwrap();
    let (code, v) = run_json(&[
        "--root",
        root.to_str().unwrap(),
        "ingest",
        "--file",
        doc.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    assert!(v["source_id"].as_str().unwrap().starts_with("source_"), "{v}");
    assert_eq!(v["deduplicated"], false);
}

// ---- ingest edges -------------------------------------------------------

#[test]
fn ingest_file_pointing_at_directory_is_usage() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let (code, v) = run_json(&[
        "--root",
        dir.path().to_str().unwrap(),
        "ingest",
        "--file",
        dir.path().to_str().unwrap(),
    ]);
    assert_eq!(code, 2, "{v}");
    assert_eq!(v["error_type"], "usage", "{v}");
}

#[test]
fn invalid_utf8_argv_is_usage_not_panic() {
    // A non-UTF-8 argv value cannot become a Rust String — clap must
    // surface it as a clean usage error, not crash.
    use std::os::unix::ffi::OsStrExt;
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let bad = OsStr::from_bytes(b"\xff\xfe\xfa");
    let out = bin()
        .args(["--root", dir.path().to_str().unwrap(), "ingest", "--text"])
        .arg(bad)
        .output()
        .unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    assert_eq!(out.status.code(), Some(2), "{v}");
    assert_eq!(v["error_type"], "usage", "{v}");
}

#[test]
fn invalid_utf8_file_bytes_ingest_as_binary() {
    // UTF-8-invalid file CONTENT is not an error: it takes the binary lane
    // (raw preserved verbatim, no normalized text). This is what office
    // files/images rely on — the payload must not be rejected.
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let doc = dir.path().join("blob.bin");
    let bytes = b"\xff\xfe\xfa\x01binary-payload";
    std::fs::write(&doc, bytes).unwrap();
    let (code, v) = run_json(&[
        "--root",
        dir.path().to_str().unwrap(),
        "ingest",
        "--file",
        doc.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["sha256"].as_str().unwrap(), llm_wiki::util::sha256_hex(bytes));
    // raw payload preserved byte-for-byte
    let raw = dir.path().join(".llm-wiki").join(v["raw_path"].as_str().unwrap());
    assert_eq!(std::fs::read(&raw).unwrap(), bytes);
}

#[test]
fn normalized_content_invalid_utf8_is_clean_txn() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let doc = dir.path().join("doc.txt");
    std::fs::write(&doc, b"hello").unwrap();
    let bad = dir.path().join("bad-extraction.md");
    std::fs::write(&bad, b"\xff\xfe\xfa").unwrap();
    let (code, v) = run_json(&[
        "--root",
        dir.path().to_str().unwrap(),
        "ingest",
        "--file",
        doc.to_str().unwrap(),
        "--normalized-content",
        bad.to_str().unwrap(),
    ]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["error_type"], "txn", "{v}");
}

#[test]
fn empty_file_ingests_with_empty_sha256() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let doc = dir.path().join("empty.txt");
    std::fs::write(&doc, b"").unwrap();
    let (code, v) = run_json(&[
        "--root",
        dir.path().to_str().unwrap(),
        "ingest",
        "--file",
        doc.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(
        v["sha256"].as_str().unwrap(),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(v["version"], 1, "{v}");
}

#[test]
fn ingest_on_uninitialized_wiki_is_txn() {
    let dir = tempfile::tempdir().unwrap();
    let (code, v) = run_json(&[
        "--root",
        dir.path().to_str().unwrap(),
        "ingest",
        "--text",
        "x",
    ]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["error_type"], "txn", "{v}");
}

#[test]
fn file_lane_ignores_max_bytes_parity() {
    // Parity note (sources.py / wiki.py `_read_bytes`): --max-bytes caps
    // the --url response and the --dir walk only. --file reads are
    // uncapped — a file larger than --max-bytes still ingests.
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let doc = dir.path().join("big.txt");
    std::fs::write(&doc, vec![b'x'; 2048]).unwrap();
    let (code, v) = run_json(&[
        "--root",
        dir.path().to_str().unwrap(),
        "ingest",
        "--file",
        doc.to_str().unwrap(),
        "--max-bytes",
        "64",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["version"], 1, "{v}");
}

// ---- corrupt / hostile on-disk state ------------------------------------

#[test]
fn corrupt_manifest_is_txn_not_panic() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    std::fs::write(
        dir.path().join(".llm-wiki/.state/manifest.json"),
        b"{{{not json",
    )
    .unwrap();
    for args in ["status", "verify"] {
        let (code, v) = run_json(&["--root", dir.path().to_str().unwrap(), args]);
        assert_eq!(code, 3, "{args} -> {v}");
        assert_eq!(v["error_type"], "txn", "{v}");
    }
    let (code, v) = run_json(&[
        "--root",
        dir.path().to_str().unwrap(),
        "ingest",
        "--text",
        "x",
    ]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["error_type"], "txn", "{v}");
}

#[cfg(unix)]
#[test]
fn ingest_on_readonly_wiki_is_clean_txn_error() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let state = dir.path().join(".llm-wiki/.state");
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o555)).unwrap();
    let out = bin()
        .args([
            "--root",
            dir.path().to_str().unwrap(),
            "ingest",
            "--text",
            "x",
        ])
        .output()
        .unwrap();
    // always restore — tempfile cleanup needs write permission
    std::fs::set_permissions(&state, std::fs::Permissions::from_mode(0o755)).unwrap();
    if out.status.code() == Some(0) {
        // running as root / a filesystem that ignores mode bits —
        // the edge can't be exercised, nothing to assert
        return;
    }
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    assert_eq!(out.status.code(), Some(3), "{v}");
    assert!(
        ["txn", "repair_required"].contains(&v["error_type"].as_str().unwrap_or("")),
        "{v}"
    );
}

// ---- txn engine path/stamp edges (library level) -------------------------

#[test]
fn stamp_target_deleted_mid_txn_is_conflict() {
    let (_d, wiki) = common::wiki();
    std::fs::write(wiki.p("claims/c1.json"), b"{\"id\":\"claim_01\"}").unwrap();
    let mut txn = Txn::begin(&wiki, "run_del").unwrap();
    txn.expect("claims/c1.json").unwrap();
    // external process removes the file between begin and commit
    std::fs::remove_file(wiki.p("claims/c1.json")).unwrap();
    match txn.commit() {
        Err(EngineError::Conflict(_)) => {}
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[test]
fn expect_absent_file_appearing_mid_txn_is_conflict() {
    let (_d, wiki) = common::wiki();
    let mut txn = Txn::begin(&wiki, "run_app").unwrap();
    txn.expect("claims/new-appears.json").unwrap(); // absent → expect-absent
    std::fs::write(wiki.p("claims/new-appears.json"), b"{}").unwrap();
    match txn.commit() {
        Err(EngineError::Conflict(_)) => {}
        other => panic!("expected Conflict, got {other:?}"),
    }
}

#[test]
fn dotdot_escapes_are_rejected() {
    // §5.2: `..` survives clean_rel and must be rejected by the
    // component walk — both escaping the wiki root entirely and
    // "internal" escapes that stay inside the namespace.
    let (_d, wiki) = common::wiki();
    let mut txn = Txn::begin(&wiki, "run_dd").unwrap();
    for rel in ["../escape.txt", "claims/../escape.txt", "a/b/../../x.json"] {
        match txn.stage_write(rel, b"x") {
            Err(EngineError::Txn(_)) => {}
            other => panic!("expected Txn for {rel:?}, got {other:?}"),
        }
    }
    assert!(!wiki.root.join("escape.txt").exists());
}

#[test]
fn deeply_nested_and_hidden_paths_are_legal() {
    // Deep nesting is fine — the parent chain is created under the flock.
    // Hidden directories inside the wiki namespace are legal too; only
    // `..` components and symlinks are hostile.
    let (_d, wiki) = common::wiki();
    let mut txn = Txn::begin(&wiki, "run_deep").unwrap();
    txn.stage_write("notes/a/b/c/d/e/f/deep.json", b"{}").unwrap();
    txn.stage_write("notes/.hidden/secret.json", b"{}").unwrap();
    txn.commit().unwrap();
    assert!(wiki.p("notes/a/b/c/d/e/f/deep.json").is_file());
    assert!(wiki.p("notes/.hidden/secret.json").is_file());
}
