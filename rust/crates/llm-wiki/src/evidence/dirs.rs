//! Recursive directory ingest: deterministic walk, one source per file.
//! Port of wikicore/dirs.py verbatim.
//!
//! Traversal is deterministic (sorted); each member file goes through
//! `sources::ingest` unchanged (own source_id, sha256 dedup, secrets scan,
//! extraction report). Skip rules are recorded per entry in the batch
//! receipt. Skipped directories are recorded once and pruned — their
//! contents are never walked (node_modules/.git scale) and never appear
//! in the receipt.

use std::fs::{self, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::err::EngineError;
use crate::evidence::{self, sources, IngestOpts, SKIP_EXTS, SKIP_NAMES};
use crate::ids;
use crate::store::Wiki;
use crate::util;

/// dirs.py SKIP_DIRS — pruned on sight, recorded as "vcs/vendor dir".
const SKIP_DIRS: &[&str] = &[
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

/// dirs.py DEFAULT_MAX_BYTES — per-file ingest ceiling for --dir/--max-bytes.
pub const DEFAULT_MAX_BYTES: u64 = 50 * 1024 * 1024;

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
/// contractual (skip-name/ext before hidden, symlink before size).
fn classify_skip(
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
    if SKIP_EXTS.contains(&evidence::ext(name).as_str()) {
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

struct WalkItem {
    rel: String,
    reason: Option<&'static str>,
}

fn rel_of(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

/// `dirs._walk` — one pass over the tree emitting (relpath, action,
/// reason); entries sort by name per level like the sorted os.walk, and
/// the final list is sorted by relpath. Skipped/symlinked directories are
/// pruned: their contents are never walked. Directory classification
/// follows links to decide dir-ness (os.walk `entry.is_dir()` follows);
/// the symlink check then records "symlink" and prunes.
fn walk(root: &Path, include_hidden: bool, max_bytes: u64) -> Vec<WalkItem> {
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
        // Dir-ness follows symlinks (os.walk entry.is_dir parity); the
        // symlink check below catches link dirs before descent.
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
                reason: Some("symlink"),
            });
            continue; // prune: never descend
        }
        if let Some(reason) = classify_skip(&path, &name, true, include_hidden, max_bytes) {
            items.push(WalkItem {
                rel,
                reason: Some(reason),
            });
            continue; // prune: never descend
        }
        walk_dir(&path, root, include_hidden, max_bytes, items);
    }
    for (name, path) in files {
        let rel = rel_of(&path, root);
        items.push(WalkItem {
            rel,
            reason: classify_skip(&path, &name, false, include_hidden, max_bytes),
        });
    }
}

/// `os.open(path, O_RDONLY|O_NOFOLLOW)` + read — ELOOP on a symlink swap
/// surfaces as an io::Error the caller re-classifies via `is_link`.
fn read_nofollow(path: &Path) -> std::io::Result<Vec<u8>> {
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

/// Python truthiness for the receipt's optional coverage/warnings merge
/// (`if r.get("coverage")` / `if r.get("warnings")`).
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

fn bump(counts: &mut Value, key: &str) {
    counts[key] = json!(counts[key].as_u64().unwrap_or(0) + 1);
}

enum PerFile {
    Receipt(Value),
    Symlink,
    Io(std::io::Error),
    Ingest(EngineError),
}

/// `dirs.ingest_dir` — batch receipt {batch_id, dir, captured_at, counts,
/// items}; persisted to `.state/ingest-batches/<batch_id>.json`.
pub fn ingest_dir(
    wiki: &Wiki,
    path: &Path,
    include_hidden: bool,
    max_bytes: u64,
    force: bool,
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
    let root = std::path::absolute(path)?;
    let root_origin = format!("dir:{}", root.display());
    let batch_id = ids::new("batch");
    let mut items: Vec<Value> = Vec::new();
    let mut counts = json!({
        "ingested": 0, "deduplicated": 0, "updated_normalized": 0,
        "skipped": 0, "errors": 0,
    });
    for item in walk(&root, include_hidden, max_bytes) {
        let rel = item.rel;
        if let Some(reason) = item.reason {
            items.push(json!({"path": rel, "status": "skipped", "reason": reason}));
            bump(&mut counts, "skipped");
            continue;
        }
        let full = root.join(&rel);
        let outcome = match read_nofollow(&full) {
            Err(e) => PerFile::Io(e),
            Ok(data) => {
                // Re-check at open time: the path may have been swapped
                // for a symlink between walk and now (TOCTOU);
                // O_NOFOLLOW covers the swap, this covers filesystems
                // where the flag is absent or ineffective.
                if is_link(&full) {
                    PerFile::Symlink
                } else {
                    let opts = IngestOpts {
                        force,
                        root_origin: Some(root_origin.clone()),
                        ..Default::default()
                    };
                    match sources::ingest(
                        wiki,
                        "file",
                        &full.to_string_lossy(),
                        &data,
                        &opts,
                    ) {
                        Ok(r) => PerFile::Receipt(r),
                        Err(e) => PerFile::Ingest(e),
                    }
                }
            }
        };
        let r = match outcome {
            PerFile::Receipt(r) => r,
            PerFile::Symlink => {
                items.push(json!({"path": rel, "status": "skipped", "reason": "symlink"}));
                bump(&mut counts, "skipped");
                continue;
            }
            PerFile::Io(e) => {
                if is_link(&full) {
                    // O_NOFOLLOW refused a symlink (ELOOP): skipped, not error.
                    items.push(json!({"path": rel, "status": "skipped", "reason": "symlink"}));
                    bump(&mut counts, "skipped");
                } else {
                    items.push(json!({"path": rel, "status": "error", "error": e.to_string()}));
                    bump(&mut counts, "errors");
                }
                continue;
            }
            PerFile::Ingest(e) => {
                items.push(json!({"path": rel, "status": "error", "error": e.to_string()}));
                bump(&mut counts, "errors");
                continue;
            }
        };
        let status = if truthy(r.get("deduplicated")) {
            "deduplicated"
        } else if truthy(r.get("updated_normalized")) {
            "updated_normalized"
        } else {
            "ingested"
        };
        bump(&mut counts, status);
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
        items.push(entry);
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
