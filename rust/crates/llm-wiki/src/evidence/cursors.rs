//! Incremental directory triage — spec §8 dir-cursors, a NEW Rust capability
//! (no Python precursor; Python `triage` was a one-shot manifest).
//!
//! `scan(wiki, dir)` walks `dir` (files only), captures each file's
//! `(mtime_ns, size)` via lstat, and diffs the walk against the persisted
//! cursor map `.state/dir-cursors.json` (`{key → {mtime_ns, size}}`):
//!
//!   present + cursor-equal    → unchanged (not reported)
//!   present + differs         → `changed`
//!   present + absent-in-cursor→ `new`
//!   cursor-present + absent   → `removed`
//!
//! The refreshed cursor map is persisted through the txn engine
//! (`stage_state` → optimistic-concurrency + atomic apply) and mirrored into
//! the `dir_cursors` sqlite table — a best-effort derived projection (§10.1)
//! that never fails the scan.
//!
//! Cursor keys are project-root-relative slash paths so one flat map serves
//! multiple watched directories without collisions; directories outside the
//! wiki root key on their absolute path. `Triage` lists are paths relative
//! to the scanned `dir`, sorted.
//!
//! Hidden files/dirs (any `.`-prefixed component below `dir`) are skipped by
//! default — aligns with the `--include-hidden` flag pattern. Size caps stay
//! ingest's `--max-bytes` concern, not the cursor's.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::err::EngineError;
use crate::index::Index;
use crate::store::{absolute, Wiki};
use crate::txn::stamps::Stamp;
use crate::txn::{mint_run_id, Txn, TxnOpts};

/// Canonical cursor file — the sqlite `dir_cursors` table is its derived
/// projection (§10.1), never the other way around.
pub const CURSORS_REL: &str = ".state/dir-cursors.json";

/// Walk-vs-cursor diff, all path lists sorted relative to the scanned dir.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Triage {
    pub changed: Vec<String>,
    pub new: Vec<String>,
    pub removed: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ScanOpts {
    /// Include `.`-prefixed files/dirs (default skips them, matching
    /// `ingest --include-hidden` semantics).
    pub include_hidden: bool,
}

/// Files the commit protocol itself mutates — reporting them as content
/// churn would flap on every scan. Applied only to keys under `.llm-wiki/`.
fn is_transient(dot_rel: &str) -> bool {
    dot_rel == CURSORS_REL // self-reference: the scan rewrites this file
        || dot_rel.starts_with(".state/staging/") // txn work area (`.live` markers)
        || dot_rel.starts_with(".state/journal") // journal.json / journal-<run>.json
}

fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// Cursor-key prefix for `dir_abs` — project-root-relative when possible so
/// different watched dirs occupy disjoint key ranges in the flat map.
fn dir_prefix(wiki: &Wiki, dir_abs: &Path) -> String {
    match dir_abs.strip_prefix(&wiki.root) {
        Ok(rel) if rel.as_os_str().is_empty() => String::new(),
        Ok(rel) => format!("{}/", slash(rel)),
        Err(_) => format!("{}/", slash(dir_abs)),
    }
}


/// One walked file's captured stamp + key material.
struct LiveFile {
    key: String,
    rel: String,
    mtime_ns: Option<u64>,
    size: u64,
}

/// Files-only recursive walk under `dir_abs` → `key → LiveFile`. Skips
/// hidden components unless `include_hidden`, never follows symlinks
/// (§5.2 lstat discipline). A missing/unreadable `dir` yields an empty
/// walk. Transient commit bookkeeping under `.llm-wiki/` (the cursor file
/// itself, staging dirs, journals) is never triage content.
fn walk_all(
    dir_abs: &Path,
    prefix: &str,
    include_hidden: bool,
) -> Result<BTreeMap<String, LiveFile>, EngineError> {
    let mut paths: Vec<PathBuf> = Vec::new();
    walk_collect(dir_abs, include_hidden, &mut paths);
    let mut live = BTreeMap::new();
    for path in paths {
        let rel = slash(path.strip_prefix(dir_abs).unwrap_or(&path));
        let key = format!("{prefix}{rel}");
        let dot_rel = key.strip_prefix(".llm-wiki/").unwrap_or(&key);
        if is_transient(dot_rel) {
            continue;
        }
        let md = match fs::symlink_metadata(&path) {
            Ok(md) => md,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(e.into()),
        };
        if !md.file_type().is_file() {
            continue;
        }
        let stamp = Stamp::from_meta(&md);
        live.insert(
            key.clone(),
            LiveFile {
                key,
                rel,
                mtime_ns: stamp.mtime_ns.map(|v| u64::try_from(v).unwrap_or(u64::MAX)),
                size: stamp.size,
            },
        );
    }
    Ok(live)
}

fn walk_collect(dir: &Path, include_hidden: bool, out: &mut Vec<PathBuf>) {
    let mut entries: Vec<_> = match fs::read_dir(dir) {
        Ok(rd) => rd.flatten().collect(),
        Err(_) => return,
    };
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name();
        if !include_hidden && name.to_string_lossy().starts_with('.') {
            continue;
        }
        let path = e.path();
        let ft = match e.file_type() {
            Ok(ft) => ft,
            Err(_) => continue,
        };
        if ft.is_symlink() {
            continue;
        } else if ft.is_dir() {
            walk_collect(&path, include_hidden, out);
        } else if ft.is_file() {
            out.push(path);
        }
    }
}

fn cursor_equal(meta: &Value, mtime_ns: Option<u64>, size: u64) -> bool {
    meta.get("mtime_ns").and_then(Value::as_u64) == mtime_ns
        && meta.get("size").and_then(Value::as_u64) == Some(size)
}

/// `key` under `prefix` → dir-relative path for `removed` reporting.
fn strip_prefix<'k>(key: &'k str, prefix: &str) -> &'k str {
    key.strip_prefix(prefix).unwrap_or(key)
}

/// Best-effort mirror of the refreshed cursor map into `dir_cursors` —
/// the table is a disposable projection; index absence never fails a scan.
fn mirror_index(wiki: &Wiki, map: &Map<String, Value>) {
    let Ok(idx) = Index::open_or_rebuild(wiki) else {
        return;
    };
    if idx.conn().execute("DELETE FROM dir_cursors", []).is_err() {
        return;
    }
    for (path, meta) in map {
        let _ = idx.conn().execute(
            "INSERT OR REPLACE INTO dir_cursors(path, mtime_ns, size) VALUES (?1, ?2, ?3)",
            rusqlite::params![
                path,
                meta["mtime_ns"].as_i64().unwrap_or(0),
                meta["size"].as_i64().unwrap_or(0)
            ],
        );
    }
}

/// Canonicalize `dir` even when it doesn't exist: canonicalize the deepest
/// existing ancestor and re-append the missing tail, so the key prefix
/// stays wiki-root-relative (canonicalized `wiki.root`) and `removed`
/// detection still works after a watched dir is deleted.
fn resolve_dir(dir: &Path) -> PathBuf {
    if let Ok(c) = fs::canonicalize(dir) {
        return c;
    }
    let abs = absolute(dir).unwrap_or_else(|_| dir.to_path_buf());
    let mut missing: Vec<std::ffi::OsString> = Vec::new();
    let mut cur: &Path = abs.as_path();
    loop {
        if let Ok(c) = fs::canonicalize(cur) {
            let mut resolved = c;
            for comp in missing.iter().rev() {
                resolved = resolved.join(comp);
            }
            return resolved;
        }
        match (cur.file_name(), cur.parent()) {
            (Some(name), Some(parent)) => {
                missing.push(name.to_os_string());
                cur = parent;
            }
            _ => return abs,
        }
    }
}

/// Walk + classify + merge into the stored cursor map. `stored` is consumed
/// and returned refreshed: keys under this dir's prefix are rewritten from
/// the live walk, other prefixes pass through untouched.
fn diff(
    wiki: &Wiki,
    dir: &Path,
    opts: &ScanOpts,
    mut stored: Map<String, Value>,
) -> Result<(Triage, Map<String, Value>), EngineError> {
    let dir_abs = resolve_dir(dir);
    let prefix = dir_prefix(wiki, &dir_abs);
    let live = walk_all(&dir_abs, &prefix, opts.include_hidden)?;

    let mut triage = Triage::default();
    let mut live_keys: BTreeMap<String, ()> = BTreeMap::new();
    for (key, f) in &live {
        live_keys.insert(key.clone(), ());
        match stored.get(key) {
            None => triage.new.push(f.rel.clone()),
            Some(meta) if !cursor_equal(meta, f.mtime_ns, f.size) => {
                triage.changed.push(f.rel.clone())
            }
            Some(_) => {} // unchanged
        }
    }
    for key in stored.keys() {
        if key.starts_with(&prefix) && !live_keys.contains_key(key) {
            triage.removed.push(strip_prefix(key, &prefix).to_string());
        }
    }

    // Refreshed map: drop this dir's stale keys, overlay the live walk.
    stored.retain(|k, _| !k.starts_with(&prefix));
    for f in live.values() {
        stored.insert(
            f.key.clone(),
            json!({"mtime_ns": f.mtime_ns, "size": f.size}),
        );
    }
    Ok((triage, stored))
}

/// Full scan entry point. `txn: Some` stages the refreshed cursor map into
/// the caller's txn (`scan_with_txn` path — commit is the caller's job and
/// the staged overlay is what a second `scan` in the same txn diffs
/// against). `txn: None` runs its own txn and commits before returning.
///
/// The sqlite `dir_cursors` mirror is refreshed best-effort after staging;
/// for caller-owned txns the projection can transiently lead the JSON file
/// if the caller never commits — derived data, always re-synced on the next
/// scan/rebuild.
pub fn scan_opts(
    wiki: &Wiki,
    dir: &Path,
    opts: &ScanOpts,
    txn: Option<&mut Txn<'_>>,
) -> Result<Triage, EngineError> {
    if let Some(t) = txn {
        let stored = load_cursor_map(t)?;
        let (triage, map) = diff(wiki, dir, opts, stored)?;
        t.stage_state(CURSORS_REL, &Value::Object(map.clone()))?;
        mirror_index(wiki, &map);
        Ok(triage)
    } else {
        let mut t = Txn::begin_with(
            wiki,
            &mint_run_id(),
            TxnOpts {
                command: Some("dir-scan".to_string()),
                ..Default::default()
            },
        )?;
        let stored = load_cursor_map(&t)?;
        let (triage, map) = diff(wiki, dir, opts, stored)?;
        t.stage_state(CURSORS_REL, &Value::Object(map.clone()))?;
        t.commit()?;
        mirror_index(wiki, &map);
        Ok(triage)
    }
}

/// `scan(wiki, dir)` — own txn, default opts (hidden files skipped).
pub fn scan(wiki: &Wiki, dir: &Path) -> Result<Triage, EngineError> {
    scan_opts(wiki, dir, &ScanOpts::default(), None)
}

/// Commit-aware variant: the caller owns the txn (and its commit). Multiple
/// `scan_with_txn` calls on one txn compose — the staged cursor overlay is
/// read back so a second dir's diff merges rather than clobbers.
pub fn scan_with_txn(
    wiki: &Wiki,
    dir: &Path,
    txn: &mut Txn<'_>,
) -> Result<Triage, EngineError> {
    scan_opts(wiki, dir, &ScanOpts::default(), Some(txn))
}

/// Overlay-aware load: inside a txn, a previously staged cursor map wins
/// over the on-disk file so sequential `scan_with_txn` calls merge.
fn load_cursor_map(txn: &Txn<'_>) -> Result<Map<String, Value>, EngineError> {
    match txn.read_json(CURSORS_REL)? {
        None => Ok(Map::new()),
        Some(v) => v.as_object().cloned().ok_or_else(|| {
            EngineError::Txn(format!(
                "{CURSORS_REL}: expected object at top level"
            ))
        }),
    }
}
