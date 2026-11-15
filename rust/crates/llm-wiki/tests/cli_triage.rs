//! `triage` subcommand + `ingest --triage` apply (sources.triage /
//! sources.ingest_triage parity): deterministic per-file manifest with
//! kind/priority/rationale, --out write, cursor changed/new/removed
//! buckets (spec §8 additive keys), batch apply where one failure logs
//! and continues, exclusivity violations → Txn exit 3.

use std::path::Path;
use std::process::Command;

use serde_json::Value;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_llm-wiki"))
}

fn init(dir: &Path) {
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

fn entry<'a>(manifest: &'a Value, name: &str) -> &'a Value {
    manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| {
            e["file"]
                .as_str()
                .map(|f| f.ends_with(name))
                .unwrap_or(false)
        })
        .unwrap_or_else(|| panic!("no manifest entry for {name}: {manifest}"))
}

/// Scan dir inside the wiki root: session .jsonl, plain .json, md, an
/// oversize .bin, and a nested file (top-level scan skips subdirs).
fn scan_tree(wiki_root: &Path) -> std::path::PathBuf {
    let d = wiki_root.join("scan");
    std::fs::create_dir_all(d.join("sub")).unwrap();
    std::fs::write(
        d.join("chat.jsonl"),
        "{\"role\":\"user\",\"message\":\"hi\"}\n{\"role\":\"assistant\",\"message\":\"yo\"}\n",
    )
    .unwrap();
    std::fs::write(d.join("data.json"), "{}").unwrap();
    std::fs::write(d.join("notes.md"), "# notes").unwrap();
    std::fs::write(d.join("big.bin"), vec![b'x'; 600_000]).unwrap();
    std::fs::write(d.join("sub/nested.txt"), "nested").unwrap();
    d
}

#[test]
fn triage_manifest_shape() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let scan = scan_tree(dir.path());

    let (code, v) = run_json(&[
        "--root", dir.path().to_str().unwrap(),
        "triage", scan.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["dir"], scan.to_str().unwrap());

    // Top-level files only — subdirs are not listed, symlinks excluded.
    let files = v["files"].as_array().unwrap();
    assert_eq!(files.len(), 4, "{files:?}");

    // Session sniffing: content beats filename.
    let chat = entry(&v, "chat.jsonl");
    assert_eq!(chat["kind"], "session");
    assert_eq!(chat["priority"], 1);
    assert!(chat["rationale"]
        .as_str()
        .unwrap()
        .contains("content carries role/message records"));

    let data = entry(&v, "data.json");
    assert!(data["kind"].is_null());
    assert_eq!(data["priority"], 2);
    assert!(data["rationale"].as_str().unwrap().contains("kind undetermined"));

    let notes = entry(&v, "notes.md");
    assert!(notes["kind"].is_null());
    assert_eq!(notes["rationale"], ".md text/markdown");

    // > 500 KB demotes to priority 3 with a size rationale.
    let big = entry(&v, "big.bin");
    assert_eq!(big["priority"], 3);
    assert!(big["rationale"].as_str().unwrap().contains("exceeds 500 KB"));
}

#[test]
fn triage_cursor_buckets() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let scan = scan_tree(dir.path());
    let root = dir.path().to_str().unwrap().to_string();

    // First scan: every walked file is `new` (recursive, dir-relative).
    let (code, v) = run_json(&["--root", &root, "triage", scan.to_str().unwrap()]);
    assert_eq!(code, 0, "{v}");
    assert!(v["changed"].as_array().unwrap().is_empty());
    assert!(v["removed"].as_array().unwrap().is_empty());
    let new: Vec<&str> = v["new"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert!(new.contains(&"chat.jsonl"), "{new:?}");
    assert!(new.contains(&"sub/nested.txt"), "{new:?}");

    // Second scan: unchanged — all buckets empty.
    let (code, v) = run_json(&["--root", &root, "triage", scan.to_str().unwrap()]);
    assert_eq!(code, 0, "{v}");
    assert!(v["changed"].as_array().unwrap().is_empty());
    assert!(v["new"].as_array().unwrap().is_empty());
    assert!(v["removed"].as_array().unwrap().is_empty());

    // Modify one, add one, delete one → changed/new/removed.
    std::fs::write(scan.join("notes.md"), "# notes v2 longer").unwrap();
    std::fs::write(scan.join("fresh.txt"), "fresh").unwrap();
    std::fs::remove_file(scan.join("data.json")).unwrap();
    let (code, v) = run_json(&["--root", &root, "triage", scan.to_str().unwrap()]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["changed"], serde_json::json!(["notes.md"]));
    assert_eq!(v["new"], serde_json::json!(["fresh.txt"]));
    assert_eq!(v["removed"], serde_json::json!(["data.json"]));
}

#[test]
fn triage_out_writes_manifest() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let scan = scan_tree(dir.path());
    let out_path = dir.path().join("manifest.json");

    let (code, v) = run_json(&[
        "--root", dir.path().to_str().unwrap(),
        "triage", "--dir", scan.to_str().unwrap(),
        "--out", out_path.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    let written: Value =
        serde_json::from_slice(&std::fs::read(&out_path).unwrap()).unwrap();
    assert_eq!(written["dir"], v["dir"]);
    assert_eq!(written["files"], v["files"]);

    // Unwritable --out → Txn exit 3.
    let (code, v) = run_json(&[
        "--root", dir.path().to_str().unwrap(),
        "triage", scan.to_str().unwrap(),
        "--out", "/nonexistent-dir/m.json",
    ]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["error_type"], "txn");
    assert!(v["error"].as_str().unwrap().contains("cannot write --out"));
}

#[test]
fn triage_error_lanes() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let root = dir.path().to_str().unwrap().to_string();

    // Not a directory → Txn exit 3.
    let (code, v) = run_json(&["--root", &root, "triage", "/not-a-dir"]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["error_type"], "txn");
    assert!(v["error"].as_str().unwrap().contains("not a directory"));

    // Missing dir argument → Usage exit 2.
    let (code, v) = run_json(&["--root", &root, "triage"]);
    assert_eq!(code, 2, "{v}");
    assert_eq!(v["error_type"], "usage");
}

#[test]
fn ingest_triage_apply() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let scan = scan_tree(dir.path());
    let root = dir.path().to_str().unwrap().to_string();
    let manifest_path = dir.path().join("manifest.json");

    let (code, _) = run_json(&[
        "--root", &root,
        "triage", scan.to_str().unwrap(),
        "--out", manifest_path.to_str().unwrap(),
    ]);
    assert_eq!(code, 0);

    // One bogus entry: failed[] records it, the rest still ingest.
    let mut manifest: Value =
        serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
    manifest["files"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "file": scan.join("ghost.txt").to_str().unwrap(),
            "kind": null, "priority": 2, "rationale": "bogus",
        }));
    std::fs::write(&manifest_path, serde_json::to_string(&manifest).unwrap()).unwrap();

    let (code, v) = run_json(&[
        "--root", &root,
        "ingest", "--triage", manifest_path.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["total"], 5);
    let ingested = v["ingested"].as_array().unwrap();
    assert_eq!(ingested.len(), 4, "{ingested:?}");
    assert!(ingested.iter().all(|i| i["deduplicated"] == false));
    assert!(ingested
        .iter()
        .all(|i| i["source_id"].as_str().unwrap().starts_with("source_")));
    let failed = v["failed"].as_array().unwrap();
    assert_eq!(failed.len(), 1);
    assert!(failed[0]["file"].as_str().unwrap().ends_with("ghost.txt"));
    assert!(failed[0]["error"].as_str().unwrap().contains("FileNotFoundError"));

    // Manifest kind drives the source kind: chat.jsonl is "session".
    let chat = ingested
        .iter()
        .find(|i| i["file"].as_str().unwrap().ends_with("chat.jsonl"))
        .unwrap();
    let sid = chat["source_id"].as_str().unwrap();
    let sm: Value = serde_json::from_slice(
        &std::fs::read(
            dir.path()
                .join(format!(".llm-wiki/sources/{sid}/manifest.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(sm["kind"], "session");

    // Re-apply: sha256 dedup flags every entry deduplicated.
    let (code, v) = run_json(&[
        "--root", &root,
        "ingest", "--triage", manifest_path.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    assert!(v["ingested"]
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["deduplicated"] == true));
}

#[test]
fn ingest_triage_bad_manifest() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let root = dir.path().to_str().unwrap().to_string();

    // Missing path → Txn exit 3 with the manifest wording.
    let (code, v) = run_json(&["--root", &root, "ingest", "--triage", "/nope.json"]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["error_type"], "txn");
    assert!(v["error"]
        .as_str()
        .unwrap()
        .contains("cannot read triage manifest /nope.json"));

    // Malformed JSON → same lane.
    let bad = dir.path().join("bad.json");
    std::fs::write(&bad, "not json {").unwrap();
    let (code, v) = run_json(&["--root", &root, "ingest", "--triage", bad.to_str().unwrap()]);
    assert_eq!(code, 3, "{v}");
    assert!(v["error"]
        .as_str()
        .unwrap()
        .contains("cannot read triage manifest"));
}

#[test]
fn ingest_triage_exclusivity() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let root = dir.path().to_str().unwrap().to_string();
    let m = dir.path().join("m.json");
    std::fs::write(&m, "{}").unwrap();
    let m = m.to_str().unwrap().to_string();

    // --triage excludes file/url/dir/text → Txn exit 3 (wiki.py:105).
    for extra in [
        vec!["--file", "x"],
        vec!["--url", "http://x"],
        vec!["--dir", "x"],
        vec!["--text", "x"],
    ] {
        let mut args = vec!["--root", &root, "ingest", "--triage", &m];
        args.extend(extra);
        let (code, v) = run_json(&args);
        assert_eq!(code, 3, "{args:?} → {v}");
        assert_eq!(v["error_type"], "txn");
        assert!(v["error"]
            .as_str()
            .unwrap()
            .contains("--triage does not combine"));
    }

    // --triage excludes per-file opts → Txn exit 3 (wiki.py:107).
    for extra in [
        vec!["--normalized-content", "x"],
        vec!["--source-id", "source_01"],
        vec!["--parser-name", "agent"],
    ] {
        let mut args = vec!["--root", &root, "ingest", "--triage", &m];
        args.extend(extra);
        let (code, v) = run_json(&args);
        assert_eq!(code, 3, "{args:?} → {v}");
        assert_eq!(v["error_type"], "txn");
    }

    // --kind with --triage → Txn exit 3 (wiki.py:102).
    let (code, v) = run_json(&[
        "--root", &root, "ingest", "--triage", &m, "--kind", "session",
    ]);
    assert_eq!(code, 3, "{v}");
    assert!(v["error"]
        .as_str()
        .unwrap()
        .contains("--kind only applies"));
}
