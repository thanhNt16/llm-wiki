//! Recursive directory ingest: deterministic walk, one source per file —
//! port of wikicore/dirs.py.
//!
//! Traversal is deterministic (sorted per level, final list sorted by
//! relpath); each member file goes through the caller-supplied per-file
//! ingest seam ([`ingest_dir`]'s `ingest` argument — the
//! `sources.ingest(wiki, "file", full, data, …)` call in Python, wired to
//! the sources port when it lands). Skip rules are recorded per entry in
//! the batch receipt. Skipped directories are recorded once and pruned —
//! their contents are never walked (node_modules/.git scale) and never
//! appear in the receipt.

use crate::err::EngineError;
use crate::ids;
use crate::store::{self, Wiki};
use crate::util;
use serde_json::{json, Map, Value};
use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

/// dirs.py SKIP_DIRS — pruned on sight, recorded as "vcs/vendor dir".
pub const SKIP_DIRS: &[&str] = &[
    ".git",
    ".svn",
    ".hg",
    "node_modules",
    "__pycache__",
    ".venv",
    "venv",
    "dist",
    "build",
    "target",
    "vendor",
    ".idea",
    ".llm-wiki",
];

/// dirs.py DEFAULT_MAX_BYTES — per-file ingest ceiling.
pub const DEFAULT_MAX_BYTES: u64 = 50 * 1024 * 1024;

/// sources.py SKIP_NAMES (dirs.py imports them for file classification).
/// ponytail: lives here until the sources port lands; it owns these consts.
pub const SKIP_NAMES: &[&str] = &[".DS_Store", "Thumbs.db"];

/// sources.py SKIP_EXTS (lowercase, dot-prefixed).
pub const SKIP_EXTS: &[&str] = &[
    ".zip", ".tar", ".tgz", ".gz", ".bz2", ".xz", ".7z", ".rar", ".exe", ".dll", ".dylib", ".so",
    ".o", ".a", ".class", ".wasm", ".pyc", ".mp3", ".mp4", ".wav", ".mov",
];

/// `os.path.splitext(name)[1].lower()`.
pub fn ext(name: &str) -> String {
    Path::new(name)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default()
}

/// `os.O_NOFOLLOW` where the OS defines it, 0 elsewhere (dirs.py
/// `getattr(os, "O_NOFOLLOW", 0)` — the portable fallback then relies on
/// the post-read `islink` re-check).
#[cfg(all(unix, target_os = "linux"))]
const O_NOFOLLOW: i32 = 0o400000;
#[cfg(all(unix, not(target_os = "linux")))]
const O_NOFOLLOW: i32 = 0x100; // macOS / *BSD
#[cfg(not(unix))]
const O_NOFOLLOW: i32 = 0;

fn is_link(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// `dirs._classify_skip` — skip reason or None to ingest. Check order is
/// contractual (dirs.py:30-52): dir → vcs/vendor then hidden; file →
/// skip-name, skip-ext, hidden, symlink, oversize.
pub fn classify_skip(
    path: &Path,
    name: &str,
    is_dir: bool,
    include_hidden: bool,
    max_bytes: u64,
) -> Option<&'static str> {
    if is_dir {
        if SKIP_DIRS.contains(&name) {
            return Some("vcs/vendor dir");
        }
        if name.starts_with('.') && !include_hidden {
            return Some("hidden");
        }
        return None;
    }
    if SKIP_NAMES.contains(&name) {
        return Some("non-evidence binary");
    }
    if SKIP_EXTS.contains(&ext(name).as_str()) {
        return Some("non-evidence binary");
    }
    if name.starts_with('.') && !include_hidden {
        return Some("hidden");
    }
    if is_link(path) {
        return Some("symlink");
    }
    match fs::metadata(path) {
        Ok(md) if md.len() > max_bytes => Some("oversize"),
        Ok(_) => None,
        Err(_) => Some("unreadable"),
    }
}

/// One `_walk` entry: `action` is `"file"` (ingest) when `skip_reason` is
/// None, `"skipped"` with the reason otherwise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WalkItem {
    pub rel: String,
    pub skip_reason: Option<&'static str>,
}

fn rel_of(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// `dirs._walk` — one pass over the tree emitting (relpath, action, reason).
/// Entries sort per level like the sorted os.walk, and the final list is
/// sorted by relpath (stable; Python sorts the same flat list by `t[0]`).
/// Skipped/symlinked directories are pruned: their contents are never
/// walked. Dir-ness follows symlinks (os.walk `entry.is_dir()` follows);
/// the symlink check then records "symlink" and prunes.
pub fn walk(root: &Path, include_hidden: bool, max_bytes: u64) -> Vec<WalkItem> {
    let mut items = Vec::new();
    walk_dir(root, root, include_hidden, max_bytes, &mut items);
    items.sort_by(|a, b| a.rel.cmp(&b.rel));
    items
}

fn walk_dir(
    dir: &Path,
    root: &Path,
    include_hidden: bool,
    max_bytes: u64,
    items: &mut Vec<WalkItem>,
) {
    let rd = match fs::read_dir(dir) {
        Ok(rd) => rd,
        Err(_) => return,
    };
    let mut dirs: Vec<(String, PathBuf)> = Vec::new();
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    for e in rd.flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let path = e.path();
        // Dir-ness follows symlinks (os.walk parity); the symlink check
        // below catches link dirs before descent.
        if e.metadata().map(|m| m.is_dir()).unwrap_or(false) {
            dirs.push((name, path));
        } else {
            files.push((name, path));
        }
    }
    dirs.sort_by(|a, b| a.0.cmp(&b.0));
    files.sort_by(|a, b| a.0.cmp(&b.0));
    for (name, path) in dirs {
        let rel = rel_of(&path, root);
        if is_link(&path) {
            items.push(WalkItem {
                rel,
                skip_reason: Some("symlink"),
            });
            continue; // prune: never descend
        }
        if let Some(reason) = classify_skip(&path, &name, true, include_hidden, max_bytes) {
            items.push(WalkItem {
                rel,
                skip_reason: Some(reason),
            });
            continue; // prune: never descend
        }
        walk_dir(&path, root, include_hidden, max_bytes, items);
    }
    for (name, path) in files {
        let rel = rel_of(&path, root);
        items.push(WalkItem {
            rel,
            skip_reason: classify_skip(&path, &name, false, include_hidden, max_bytes),
        });
    }
}

/// `os.open(path, O_RDONLY|O_NOFOLLOW)` + read — ELOOP on a symlink swap
/// surfaces as an io::Error the caller re-classifies via `is_link`.
pub(crate) fn read_nofollow(path: &Path) -> std::io::Result<Vec<u8>> {
    let mut opts = OpenOptions::new();
    opts.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        if O_NOFOLLOW != 0 {
            opts.custom_flags(O_NOFOLLOW);
        }
    }
    let mut f = opts.open(path)?;
    let mut data = Vec::new();
    f.read_to_end(&mut data)?;
    Ok(data)
}

// ---- batch ingest (dirs.py ingest_dir) -----------------------------------

/// Why one file failed its per-file ingest. `Symlink` models the TOCTOU
/// swap (`_BecameSymlink` / ELOOP in Python): recorded as skipped/
/// "symlink", never as an error.
pub enum PerFileError {
    Symlink,
    Other(String),
}

/// Per-file ingest seam — `sources.ingest(wiki, "file", full, data,
/// force=…, root_origin=…)` (dirs.py:118-119). Receives the absolute file
/// path, the bytes read with `O_NOFOLLOW`, and the batch `root_origin`
/// (`dir:<abs root>`); returns the sources receipt (`source_id`,
/// `version`, optional `deduplicated`/`updated_normalized`/
/// `coverage`/`warnings`).
pub type IngestFn<'a> = dyn FnMut(&Path, &[u8], &str) -> Result<Value, PerFileError> + 'a;

/// Python truthiness for the receipt's optional coverage/warnings merge
/// (`if r.get("coverage")` / `if r.get("warnings")`, dirs.py:147-150).
fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64() != Some(0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

fn bump(counts: &mut Map<String, Value>, key: &str) {
    let cur = counts.get(key).and_then(Value::as_u64).unwrap_or(0);
    counts.insert(key.to_string(), json!(cur + 1));
}

/// Outcome classification for one walked file (dirs.py:104-151). Returns
/// the receipt item to append; `counts` is bumped accordingly.
pub fn classify_outcome(
    rel: &str,
    result: Result<Value, PerFileError>,
    counts: &mut Map<String, Value>,
) -> Value {
    let r = match result {
        Ok(r) => r,
        Err(PerFileError::Symlink) => {
            bump(counts, "skipped");
            return json!({"path": rel, "status": "skipped", "reason": "symlink"});
        }
        Err(PerFileError::Other(e)) => {
            bump(counts, "errors");
            return json!({"path": rel, "status": "error", "error": e});
        }
    };
    let status = if truthy(r.get("deduplicated")) {
        "deduplicated"
    } else if truthy(r.get("updated_normalized")) {
        "updated_normalized"
    } else {
        "ingested"
    };
    bump(counts, status);
    let mut entry = json!({
        "path": rel, "status": status,
        "source_id": r["source_id"], "version": r["version"],
    });
    if truthy(r.get("coverage")) {
        entry["coverage"] = r["coverage"].clone();
    }
    if truthy(r.get("warnings")) {
        entry["warnings"] = r["warnings"].clone();
    }
    entry
}

/// Empty receipt counts skeleton (dirs.py:101-102).
pub fn empty_counts() -> Map<String, Value> {
    let mut m = Map::new();
    for k in [
        "ingested",
        "deduplicated",
        "updated_normalized",
        "skipped",
        "errors",
    ] {
        m.insert(k.to_string(), json!(0));
    }
    m
}

/// `dirs.ingest_dir` — batch receipt `{batch_id, dir, captured_at, counts,
/// items}`, persisted to `.state/ingest-batches/<batch_id>.json`.
///
/// `ingest` is the per-file seam (see [`IngestFn`]); read errors are
/// classified exactly like dirs.py:124-133 — an ELOOP/symlink swap becomes
/// a skipped "symlink" entry, any other failure an error entry, and the
/// batch always completes with a receipt.
pub fn ingest_dir(
    wiki: &Wiki,
    path: &Path,
    include_hidden: bool,
    max_bytes: u64,
    ingest: &mut IngestFn,
) -> Result<Value, EngineError> {
    if !wiki.exists() {
        return Err(EngineError::Txn(
            "wiki not initialized; run wiki-init first".into(),
        ));
    }
    if !path.is_dir() {
        return Err(EngineError::Txn(format!(
            "not a directory: {}",
            path.display()
        )));
    }
    let root = store::absolute(path)?;
    let root_origin = format!("dir:{}", root.display());
    let batch_id = ids::new("batch");
    let mut items: Vec<Value> = Vec::new();
    let mut counts = empty_counts();
    for item in walk(&root, include_hidden, max_bytes) {
        let rel = item.rel;
        if let Some(reason) = item.skip_reason {
            items.push(json!({"path": rel, "status": "skipped", "reason": reason}));
            bump(&mut counts, "skipped");
            continue;
        }
        let full = root.join(&rel);
        // Re-check at open time: the path may have been swapped for a
        // symlink between walk and now (TOCTOU); O_NOFOLLOW covers the
        // swap, the post-read islink covers filesystems where the flag is
        // absent or ineffective (dirs.py:109-117).
        let data = match read_nofollow(&full) {
            Ok(data) => {
                if is_link(&full) {
                    None
                } else {
                    Some(data)
                }
            }
            Err(_) if is_link(&full) => None, // O_NOFOLLOW refused a symlink
            Err(e) => {
                items.push(json!({"path": rel, "status": "error", "error": e.to_string()}));
                bump(&mut counts, "errors");
                continue;
            }
        };
        let outcome = match data {
            None => Err(PerFileError::Symlink),
            Some(data) => (*ingest)(&full, &data, &root_origin),
        };
        items.push(classify_outcome(&rel, outcome, &mut counts));
    }
    let receipt = json!({
        "batch_id": batch_id,
        "dir": root.to_string_lossy(),
        "captured_at": util::now_iso(),
        "counts": counts,
        "items": items,
    });
    let out_dir = wiki.p(".state/ingest-batches");
    fs::create_dir_all(&out_dir)?;
    util::write_json_atomic(&out_dir.join(format!("{batch_id}.json")), &receipt)?;
    Ok(receipt)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build the fixture tree from the Python ground-truth run and run
    /// `walk`; assert the exact (rel, action, reason) triples.
    fn fixture(root: &Path) {
        let files: &[&str] = &[
            "a.txt",
            "b.log",
            ".hidden.txt",
            ".DS_Store",
            "Thumbs.db",
            "c.mp3",
            "d.zip",
            "sub1/e.txt",
            "sub1/f.txt",
            "node_modules/g.txt",
            ".git/h.txt",
            ".venv/i.txt",
            "empty_dir/j.txt",
            "sub2/.secret.txt",
            "sub2/k.TXT",
        ];
        for rel in files {
            let p = root.join(rel);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, "x").unwrap();
        }
        // Python fixture: big.bin is 100 bytes (oversize at max_bytes=50).
        fs::write(root.join("big.bin"), "x".repeat(100)).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("a.txt"), root.join("link_file")).unwrap();
            std::os::unix::fs::symlink(root.join("sub1"), root.join("link_dir")).unwrap();
        }
    }

    fn rels(items: &[WalkItem]) -> Vec<(String, Option<&'static str>)> {
        items
            .iter()
            .map(|i| (i.rel.clone(), i.skip_reason))
            .collect()
    }

    #[test]
    fn walk_matches_python_ground_truth() {
        // Exactly the triples Python's dirs._walk printed for this fixture
        // (include_hidden=False, max_bytes=50): big.bin(100B) oversize.
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let got = rels(&walk(dir.path(), false, 50));
        let want: Vec<(String, Option<&'static str>)> = [
            (".DS_Store", Some("non-evidence binary")),
            (".git", Some("vcs/vendor dir")),
            (".hidden.txt", Some("hidden")),
            (".venv", Some("vcs/vendor dir")),
            ("Thumbs.db", Some("non-evidence binary")),
            ("a.txt", None),
            ("b.log", None),
            ("big.bin", Some("oversize")),
            ("c.mp3", Some("non-evidence binary")),
            ("d.zip", Some("non-evidence binary")),
            ("empty_dir/j.txt", None),
            ("link_dir", Some("symlink")),
            ("link_file", Some("symlink")),
            ("node_modules", Some("vcs/vendor dir")),
            ("sub1/e.txt", None),
            ("sub1/f.txt", None),
            ("sub2/.secret.txt", Some("hidden")),
            ("sub2/k.TXT", None),
        ]
        .iter()
        .map(|(r, s)| (r.to_string(), *s))
        .collect();
        assert_eq!(got, want);
    }

    #[test]
    fn walk_include_hidden_and_pruned_dirs_match_python() {
        // include_hidden=True ground truth: hidden files ingest, pruned
        // dirs (.git/.venv/node_modules) still recorded and never descended.
        let dir = tempfile::tempdir().unwrap();
        fixture(dir.path());
        let got = rels(&walk(dir.path(), true, 50));
        let want: Vec<(String, Option<&'static str>)> = [
            (".DS_Store", Some("non-evidence binary")),
            (".git", Some("vcs/vendor dir")),
            (".hidden.txt", None),
            (".venv", Some("vcs/vendor dir")),
            ("Thumbs.db", Some("non-evidence binary")),
            ("a.txt", None),
            ("b.log", None),
            ("big.bin", Some("oversize")),
            ("c.mp3", Some("non-evidence binary")),
            ("d.zip", Some("non-evidence binary")),
            ("empty_dir/j.txt", None),
            ("link_dir", Some("symlink")),
            ("link_file", Some("symlink")),
            ("node_modules", Some("vcs/vendor dir")),
            ("sub1/e.txt", None),
            ("sub1/f.txt", None),
            ("sub2/.secret.txt", None),
            ("sub2/k.TXT", None),
        ]
        .iter()
        .map(|(r, s)| (r.to_string(), *s))
        .collect();
        assert_eq!(got, want);
        // link_dir contents never appear (pruned).
        assert!(!got.iter().any(|(r, _)| r.starts_with("link_dir/")));
        assert!(!got.iter().any(|(r, _)| r.starts_with("node_modules/")));
    }

    #[test]
    fn classify_skip_order_is_contractual() {
        // .DS_Store is both hidden and skip-named → skip-name wins
        // ("non-evidence binary", Python order: name check before hidden).
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join(".DS_Store");
        fs::write(&p, "x").unwrap();
        assert_eq!(
            classify_skip(&p, ".DS_Store", false, false, 50),
            Some("non-evidence binary")
        );
        // uppercase ext lowercases before the SKIP_EXTS check.
        assert_eq!(ext("K.TXT"), ".txt");
        assert_eq!(classify_skip(&p, "K.TXT", false, false, 50), None);
        // unreadable size → "unreadable".
        let missing = dir.path().join("ghost.txt");
        assert_eq!(
            classify_skip(&missing, "ghost.txt", false, false, 50),
            Some("unreadable")
        );
        // dirs: vendor list, then hidden.
        assert_eq!(
            classify_skip(&p, "node_modules", true, false, 50),
            Some("vcs/vendor dir")
        );
        assert_eq!(classify_skip(&p, ".idea2", true, false, 50), Some("hidden"));
        assert_eq!(classify_skip(&p, "src", true, false, 50), None);
        assert_eq!(classify_skip(&p, ".idea2", true, true, 50), None);
    }

    #[test]
    fn classify_outcome_receipt_parity() {
        // dirs.py:138-151 — status selection + optional coverage/warnings
        // merge, exercised with synthetic ingest receipts.
        let mut counts = empty_counts();
        let item = classify_outcome(
            "a.txt",
            Ok(json!({"source_id": "s1", "version": 1})),
            &mut counts,
        );
        assert_eq!(item["status"], "ingested");
        assert_eq!(counts["ingested"], json!(1));
        let item = classify_outcome(
            "b.txt",
            Ok(json!({"source_id": "s1", "version": 1, "deduplicated": true})),
            &mut counts,
        );
        assert_eq!(item["status"], "deduplicated");
        assert_eq!(counts["deduplicated"], json!(1));
        let item = classify_outcome(
            "c.txt",
            Ok(json!({
                "source_id": "s2", "version": 2, "updated_normalized": true,
                "coverage": {"files": 3}, "warnings": ["x"],
            })),
            &mut counts,
        );
        assert_eq!(item["status"], "updated_normalized");
        assert_eq!(item["coverage"], json!({"files": 3}));
        assert_eq!(item["warnings"], json!(["x"]));
        assert_eq!(counts["updated_normalized"], json!(1));
        // falsy coverage/warnings are dropped (Python `if r.get(...)`).
        let item = classify_outcome(
            "d.txt",
            Ok(json!({"source_id": "s3", "version": 1, "coverage": {}, "warnings": []})),
            &mut counts,
        );
        assert!(item.get("coverage").is_none());
        assert!(item.get("warnings").is_none());
        // symlink swap → skipped, not error.
        let item = classify_outcome("e.txt", Err(PerFileError::Symlink), &mut counts);
        assert_eq!(
            item,
            json!({"path": "e.txt", "status": "skipped", "reason": "symlink"})
        );
        assert_eq!(counts["skipped"], json!(1));
        // real failure → error entry.
        let item = classify_outcome(
            "f.txt",
            Err(PerFileError::Other("boom".into())),
            &mut counts,
        );
        assert_eq!(
            item,
            json!({"path": "f.txt", "status": "error", "error": "boom"})
        );
        assert_eq!(counts["errors"], json!(1));
    }

    #[cfg(unix)]
    #[test]
    fn read_nofollow_refuses_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real.txt");
        fs::write(&real, "data").unwrap();
        let link = dir.path().join("link.txt");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(read_nofollow(&link).is_err()); // ELOOP
        assert_eq!(read_nofollow(&real).unwrap(), b"data");
    }

    #[test]
    fn truthy_matches_python_get_truthiness() {
        assert!(!truthy(None));
        assert!(!truthy(Some(&Value::Null)));
        assert!(!truthy(Some(&json!(false))));
        assert!(!truthy(Some(&json!(0))));
        assert!(!truthy(Some(&json!(""))));
        assert!(!truthy(Some(&json!([]))));
        assert!(!truthy(Some(&json!({}))));
        assert!(truthy(Some(&json!(true))));
        assert!(truthy(Some(&json!(1))));
        assert!(truthy(Some(&json!("x"))));
        assert!(truthy(Some(&json!([1]))));
        assert!(truthy(Some(&json!({"a": 1}))));
    }

    #[test]
    fn ingest_dir_requires_initialized_wiki() {
        let root = tempfile::tempdir().unwrap();
        let wiki = Wiki::open(root.path()).unwrap();
        let src = tempfile::tempdir().unwrap();
        let mut ingest: Box<IngestFn> =
            Box::new(|_, _, _| Ok(json!({"source_id": "s", "version": 1})));
        let err = ingest_dir(&wiki, src.path(), false, DEFAULT_MAX_BYTES, &mut ingest).unwrap_err();
        assert!(err.to_string().contains("wiki not initialized"), "{err}");
    }

    #[test]
    fn ingest_dir_requires_directory() {
        let root = tempfile::tempdir().unwrap();
        let wiki = Wiki::open(root.path()).unwrap();
        crate::store::init_wiki(&wiki, "proj").unwrap();
        let missing = root.path().join("nope");
        let mut ingest: Box<IngestFn> =
            Box::new(|_, _, _| Ok(json!({"source_id": "s", "version": 1})));
        let err = ingest_dir(&wiki, &missing, false, DEFAULT_MAX_BYTES, &mut ingest).unwrap_err();
        assert!(err.to_string().contains("not a directory"), "{err}");
    }

    #[test]
    fn ingest_dir_full_receipt_parity() {
        // dirs.py:91-161 end-to-end with a stubbed sources.ingest: skip
        // classification, per-file status selection, counts, receipt write
        // to .state/ingest-batches/<batch_id>.json.
        let wiki_root = tempfile::tempdir().unwrap();
        let wiki = Wiki::open(wiki_root.path()).unwrap();
        crate::store::init_wiki(&wiki, "proj").unwrap();

        let src = tempfile::tempdir().unwrap();
        fs::write(src.path().join("a.txt"), "hello").unwrap();
        fs::create_dir_all(src.path().join("node_modules")).unwrap();
        fs::write(src.path().join("node_modules/g.txt"), "x").unwrap();
        fs::write(src.path().join(".DS_Store"), "x").unwrap();
        fs::write(src.path().join("d.zip"), "x").unwrap();
        fs::create_dir_all(src.path().join("sub")).unwrap();
        fs::write(src.path().join("sub/b.txt"), "hi").unwrap();

        let calls: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let mut ingest: Box<IngestFn> = Box::new(|path, _data, root_origin| {
            calls.lock().unwrap().push(format!(
                "{}|{}",
                path.file_name().unwrap().to_string_lossy(),
                root_origin
            ));
            Ok(json!({"source_id": "s1", "version": 1}))
        });
        let receipt = ingest_dir(&wiki, src.path(), false, DEFAULT_MAX_BYTES, &mut ingest).unwrap();

        assert_eq!(
            receipt["counts"],
            json!({"ingested": 2, "deduplicated": 0, "updated_normalized": 0, "skipped": 3, "errors": 0})
        );
        let items: Vec<Value> = receipt["items"].as_array().cloned().unwrap();
        assert_eq!(
            Value::Array(items),
            json!([
                {"path": ".DS_Store", "status": "skipped", "reason": "non-evidence binary"},
                {"path": "a.txt", "status": "ingested", "source_id": "s1", "version": 1},
                {"path": "d.zip", "status": "skipped", "reason": "non-evidence binary"},
                {"path": "node_modules", "status": "skipped", "reason": "vcs/vendor dir"},
                {"path": "sub/b.txt", "status": "ingested", "source_id": "s1", "version": 1},
            ])
        );
        assert!(receipt["batch_id"].as_str().unwrap().starts_with("batch_"));
        // root canonicalizes (macOS /var → /private/var) — dirs.py abspath parity
        assert_eq!(
            receipt["dir"],
            json!(std::fs::canonicalize(src.path()).unwrap().to_string_lossy())
        );
        // the seam receives each file with the dir root_origin
        let calls = calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert!(calls.iter().all(|c| c.contains("|dir:")), "{calls:?}");
        // receipt persisted under .state/ingest-batches/<batch_id>.json
        let path = wiki.p(&format!(
            ".state/ingest-batches/{}.json",
            receipt["batch_id"].as_str().unwrap()
        ));
        let persisted: Value = serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(persisted["counts"]["ingested"], json!(2));
    }

    #[test]
    fn ingest_dir_reports_ingest_errors_and_symlinks() {
        let wiki_root = tempfile::tempdir().unwrap();
        let wiki = Wiki::open(wiki_root.path()).unwrap();
        crate::store::init_wiki(&wiki, "proj").unwrap();
        let src = tempfile::tempdir().unwrap();
        fs::write(src.path().join("ok.txt"), "a").unwrap();
        fs::write(src.path().join("bad.txt"), "b").unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(src.path().join("ok.txt"), src.path().join("link.txt")).unwrap();

        let mut ingest: Box<IngestFn> = Box::new(|path, _, _| {
            if path.file_name().unwrap() == "bad.txt" {
                Err(PerFileError::Other("extract failed".into()))
            } else {
                Ok(json!({"source_id": "s1", "version": 1}))
            }
        });
        let receipt = ingest_dir(&wiki, src.path(), false, DEFAULT_MAX_BYTES, &mut ingest).unwrap();
        assert_eq!(
            receipt["counts"],
            json!({"ingested": 1, "deduplicated": 0, "updated_normalized": 0, "skipped": 1, "errors": 1})
        );
        let items: Vec<Value> = receipt["items"].as_array().cloned().unwrap();
        assert!(items
            .contains(&json!({"path": "bad.txt", "status": "error", "error": "extract failed"})));
        #[cfg(unix)]
        assert!(
            items.contains(&json!({"path": "link.txt", "status": "skipped", "reason": "symlink"}))
        );
        assert!(items.contains(
            &json!({"path": "ok.txt", "status": "ingested", "source_id": "s1", "version": 1})
        ));
    }

    #[cfg(unix)]
    #[test]
    fn ingest_dir_symlink_file_skipped() {
        // A symlinked file is skipped "symlink" at walk time — recorded,
        // never descended, never an error (dirs.py:44-46/71-73).
        let wiki_root = tempfile::tempdir().unwrap();
        let wiki = Wiki::open(wiki_root.path()).unwrap();
        crate::store::init_wiki(&wiki, "proj").unwrap();
        let src = tempfile::tempdir().unwrap();
        fs::write(src.path().join("real.txt"), "data").unwrap();
        std::os::unix::fs::symlink(src.path().join("real.txt"), src.path().join("swap.txt"))
            .unwrap();
        let mut ingest: Box<IngestFn> =
            Box::new(|_, _, _| Ok(json!({"source_id": "s1", "version": 1})));
        let receipt = ingest_dir(&wiki, src.path(), false, DEFAULT_MAX_BYTES, &mut ingest).unwrap();
        let items: Vec<Value> = receipt["items"].as_array().cloned().unwrap();
        assert!(
            items.contains(&json!({"path": "swap.txt", "status": "skipped", "reason": "symlink"}))
        );
        assert_eq!(receipt["counts"]["skipped"], json!(1));
    }
}
