//! `ingest --dir` — recursive batch ingest (dirs.py parity): deterministic
//! sorted walk, skip reasons recorded once per pruned entry, per-file
//! sources through the same lane as --file, batch receipt persisted under
//! `.state/ingest-batches/`.

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

fn item<'a>(receipt: &'a Value, path: &str) -> &'a Value {
    receipt["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["path"] == path)
        .unwrap_or_else(|| panic!("no receipt item for {path}: {receipt}"))
}

/// Fixture: a doc tree exercising every skip lane.
fn doc_tree(root: &Path) -> std::path::PathBuf {
    let d = root.join("docs");
    std::fs::create_dir_all(d.join("sub")).unwrap();
    std::fs::create_dir_all(d.join(".git")).unwrap();
    std::fs::create_dir_all(d.join("node_modules/dep")).unwrap();
    std::fs::write(d.join("a.txt"), "hello dir world").unwrap();
    std::fs::write(d.join("sub/n.txt"), "nested").unwrap();
    std::fs::write(d.join(".git/index"), "gitfile").unwrap();
    std::fs::write(d.join("node_modules/dep/p.js"), "junk").unwrap();
    std::fs::write(d.join("song.mp3"), "binary").unwrap();
    std::fs::write(d.join(".hidden"), "secret").unwrap();
    std::fs::write(d.join("big.bin"), vec![b'x'; 200_000]).unwrap();
    d
}

#[test]
fn ingest_dir_per_file_sources_and_counts() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let docs = doc_tree(dir.path());

    let (code, v) = run_json(&[
        "--root", dir.path().to_str().unwrap(),
        "ingest", "--dir", docs.to_str().unwrap(),
        "--max-bytes", "100000",
    ]);
    assert_eq!(code, 0, "{v}");
    assert!(v["batch_id"].as_str().unwrap().starts_with("batch_"), "{v}");
    assert_eq!(v["dir"], docs.to_str().unwrap());
    assert_eq!(v["counts"]["ingested"], 2);
    assert_eq!(v["counts"]["skipped"], 5);
    assert_eq!(v["counts"]["errors"], 0);

    // Ingested items carry source identity through the --file lane.
    let a = item(&v, "a.txt");
    assert_eq!(a["status"], "ingested");
    assert!(a["source_id"].as_str().unwrap().starts_with("source_"));
    assert_eq!(a["version"], 1);
    let n = item(&v, "sub/n.txt");
    assert_eq!(n["status"], "ingested");

    // Every ingested file got its own source manifest + raw payload.
    let dot = dir.path().join(".llm-wiki");
    let sid = a["source_id"].as_str().unwrap();
    assert!(dot.join(format!("sources/{sid}/manifest.json")).is_file());
    assert_eq!(
        std::fs::read_to_string(dot.join(format!("sources/{sid}/content.md"))).unwrap(),
        "hello dir world"
    );

    // Receipt persisted under .state/ingest-batches/<batch_id>.json.
    let batch = dot.join(format!(".state/ingest-batches/{}.json", v["batch_id"].as_str().unwrap()));
    assert!(batch.is_file(), "receipt not persisted: {batch:?}");
    let persisted: Value = serde_json::from_slice(&std::fs::read(batch).unwrap()).unwrap();
    assert_eq!(persisted["batch_id"], v["batch_id"]);
}

#[test]
fn ingest_dir_skip_reasons() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let docs = doc_tree(dir.path());

    let (code, v) = run_json(&[
        "--root", dir.path().to_str().unwrap(),
        "ingest", "--dir", docs.to_str().unwrap(),
        "--max-bytes", "100000",
    ]);
    assert_eq!(code, 0, "{v}");

    assert_eq!(item(&v, ".git")["reason"], "vcs/vendor dir");
    assert_eq!(item(&v, "node_modules")["reason"], "vcs/vendor dir");
    assert_eq!(item(&v, ".hidden")["reason"], "hidden");
    assert_eq!(item(&v, "song.mp3")["reason"], "non-evidence binary");
    assert_eq!(item(&v, "big.bin")["reason"], "oversize");

    // Pruned dirs are recorded once: their contents never appear.
    let paths: Vec<&str> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["path"].as_str().unwrap())
        .collect();
    assert!(!paths.iter().any(|p| p.contains(".git/")), "{paths:?}");
    assert!(!paths.iter().any(|p| p.contains("node_modules/")), "{paths:?}");
}

#[cfg(unix)]
#[test]
fn ingest_dir_symlink_skipped() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let docs = dir.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("real.txt"), "real").unwrap();
    std::os::unix::fs::symlink(docs.join("real.txt"), docs.join("link.txt")).unwrap();
    std::fs::create_dir_all(docs.join("inner")).unwrap();
    std::fs::write(docs.join("inner/i.txt"), "i").unwrap();
    std::os::unix::fs::symlink(docs.join("inner"), docs.join("dirlink")).unwrap();

    let (code, v) = run_json(&[
        "--root", dir.path().to_str().unwrap(),
        "ingest", "--dir", docs.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(item(&v, "link.txt")["reason"], "symlink");
    assert_eq!(item(&v, "dirlink")["reason"], "symlink");
    assert_eq!(item(&v, "inner/i.txt")["status"], "ingested");
    assert_eq!(v["counts"]["skipped"], 2);
}

#[test]
fn ingest_dir_deterministic_order() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let docs = dir.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    // Create in reverse order — receipt order must still be sorted by path.
    for name in ["c.txt", "a.txt", "b.txt"] {
        std::fs::write(docs.join(name), name).unwrap();
    }
    let (code, v) = run_json(&[
        "--root", dir.path().to_str().unwrap(),
        "ingest", "--dir", docs.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    let paths: Vec<&str> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["path"].as_str().unwrap())
        .collect();
    let mut sorted = paths.clone();
    sorted.sort_unstable();
    assert_eq!(paths, sorted);
}

#[test]
fn ingest_dir_dedup_and_force() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let docs = dir.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("a.txt"), "v1").unwrap();

    let args = &[
        "--root", dir.path().to_str().unwrap(),
        "ingest", "--dir", docs.to_str().unwrap(),
    ];
    let (code, v1) = run_json(args);
    assert_eq!(code, 0, "{v1}");
    assert_eq!(item(&v1, "a.txt")["status"], "ingested");

    // Second run: sha256 dedup reports deduplicated, not a new version.
    let (code, v2) = run_json(args);
    assert_eq!(code, 0, "{v2}");
    assert_eq!(item(&v2, "a.txt")["status"], "deduplicated");
    assert_eq!(item(&v2, "a.txt")["version"], 1);
    assert_eq!(v2["counts"]["deduplicated"], 1);

    // --force appends a new version instead of deduplicating.
    let mut force_args = args.to_vec();
    force_args.push("--force");
    let (code, v3) = run_json(&force_args);
    assert_eq!(code, 0, "{v3}");
    assert_eq!(item(&v3, "a.txt")["status"], "ingested");
    assert_eq!(item(&v3, "a.txt")["version"], 2);
}

#[test]
fn ingest_dir_include_hidden() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let docs = doc_tree(dir.path());

    let (code, v) = run_json(&[
        "--root", dir.path().to_str().unwrap(),
        "ingest", "--dir", docs.to_str().unwrap(),
        "--include-hidden",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(item(&v, ".hidden")["status"], "ingested");
    // SKIP_DIRS membership wins over the hidden check: .git stays pruned.
    assert_eq!(item(&v, ".git")["reason"], "vcs/vendor dir");
}

#[test]
fn ingest_dir_error_lanes() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let root = dir.path().to_str().unwrap().to_string();

    // Not a directory → Txn exit 3 (dirs.py:96 parity).
    let (code, v) = run_json(&["--root", &root, "ingest", "--dir", "/nonexistent-dir"]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["error_type"], "txn");
    assert!(v["error"].as_str().unwrap().contains("not a directory"));

    // --normalized-content is per-file only → Txn exit 3.
    let docs = dir.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    let (code, v) = run_json(&[
        "--root", &root, "ingest", "--dir", docs.to_str().unwrap(),
        "--normalized-content", "x.md",
    ]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["error_type"], "txn");

    // --kind is --file/--text only → Txn exit 3.
    let (code, v) = run_json(&[
        "--root", &root, "ingest", "--dir", docs.to_str().unwrap(),
        "--kind", "session",
    ]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["error_type"], "txn");
}

#[test]
fn ingest_dir_requires_initialized_wiki() {
    let dir = tempfile::tempdir().unwrap(); // never init'd
    let docs = dir.path().join("docs");
    std::fs::create_dir_all(&docs).unwrap();
    std::fs::write(docs.join("a.txt"), "x").unwrap();

    let (code, v) = run_json(&[
        "--root", dir.path().to_str().unwrap(),
        "ingest", "--dir", docs.to_str().unwrap(),
    ]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["error_type"], "txn");
    assert!(v["error"].as_str().unwrap().contains("wiki not initialized"));
}
