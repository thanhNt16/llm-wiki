//! Journal/backup leftovers — spec §5.4.
//!
//! `commit()` writes `journal.json` + `journal-<run>.json` BEFORE apply and
//! `.backup-<rev>/` before touching a destination, so a crash mid-commit
//! leaves breadcrumbs. `scan` finds them; `repair` restores (restore-from-
//! backup THEN delete) or, for the journal-only crash window, simply clears.
//! The commit-side gate (`Txn::commit`) aborts with `EngineError::Repair`
//! (exit 3) while leftovers from a PRIOR attempt exist.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::err::EngineError;
use crate::store::Wiki;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Leftover {
    /// `.state/journal.json` or `.state/journal-<run>.json`
    Journal(PathBuf),
    /// `.state/.backup-<rev>/` or `.state/.backup-<rev>.prev-<ts>/`
    BackupDir(PathBuf),
    /// `.state/staging/<run>/` from a dead attempt (informational — the
    /// commit gate keys on Journal/BackupDir only, per §5.4).
    Staging(PathBuf),
}

/// `.backup-<N>` or `.backup-<N>.prev-<ts>` → `(N, prev_ts or MAX)`.
/// Sorting by this key restores revision N's prev-generation first and lets
/// the highest N win per file (spec §5.4 repair rule).
fn backup_key(name: &str) -> Option<(u64, u64)> {
    let rest = name.strip_prefix(".backup-")?;
    match rest.split_once(".prev-") {
        Some((n, ts)) => Some((n.parse().ok()?, ts.parse().unwrap_or(0))),
        None => Some((rest.parse().ok()?, u64::MAX)),
    }
}

fn journal_run_id(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_string_lossy();
    name.strip_prefix("journal-")?
        .strip_suffix(".json")
        .map(|s| s.to_string())
}

fn parse_journal_ops(path: &Path) -> (Option<String>, Vec<(String, String)>) {
    let text = match fs::read_to_string(path) {
        Ok(t) => t,
        Err(_) => return (journal_run_id(path), Vec::new()),
    };
    let v: Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(_) => return (journal_run_id(path), Vec::new()),
    };
    let run_id = v["receipt"]["run_id"].as_str().map(|s| s.to_string());
    let mut ops = Vec::new();
    if let Some(arr) = v["ops"].as_array() {
        for op in arr {
            if let (Some(kind), Some(rel)) = (op[0].as_str(), op[1].as_str()) {
                ops.push((kind.to_string(), rel.to_string()));
            }
        }
    }
    (run_id.or_else(|| journal_run_id(path)), ops)
}

fn list_dirs(dir: &Path) -> Vec<PathBuf> {
    fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default()
}

/// True while a staging run dir is in one of the §5.5 live phases —
/// repair and junk cleanup MUST skip these.
pub(crate) fn staging_is_live(d: &Path) -> bool {
    let has = |n: &str| d.join(n).is_file();
    (has("pending.json") && !has("compile.json"))
        || (has("candidates.jsonl") && !has("comparisons.json"))
        || (has("compile.json") && !has("comparisons.json"))
        || (has("comparisons.json") && !has("receipt.json"))
}

/// All leftover breadcrumbs under `.state/` — journals, `.backup-*` dirs,
/// and dead staging dirs. Live-phase staging dirs are not leftovers.
pub fn scan(wiki: &Wiki) -> Vec<Leftover> {
    let mut out = Vec::new();
    let state = wiki.p(".state");
    if let Ok(rd) = fs::read_dir(&state) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let p = e.path();
            if name == "journal.json"
                || (name.starts_with("journal-") && name.ends_with(".json"))
            {
                if p.is_file() {
                    out.push(Leftover::Journal(p));
                }
            } else if backup_key(&name).is_some() && p.is_dir() {
                out.push(Leftover::BackupDir(p));
            }
        }
    }
    for d in list_dirs(&state.join("staging")) {
        if !staging_is_live(&d) {
            out.push(Leftover::Staging(d));
        }
    }
    out
}

/// The commit gate: true while a prior attempt's journal or backup remains.
/// Staging dirs are informational (§5.5 parked runs are legitimate).
pub fn repair_needed(wiki: &Wiki) -> bool {
    scan(wiki)
        .iter()
        .any(|l| matches!(l, Leftover::Journal(_) | Leftover::BackupDir(_)))
}

/// Restore-then-delete every leftover (spec §5.4):
/// - `.backup-<N>` dirs restore in ascending-N order (newest wins per
///   file); `.prev-<ts>` generations of the same N restore before it.
/// - Destinations that a journaled write created but no backup covers are
///   unlinked (newly-written destinations not in the backup).
/// - Journal-without-backup: nothing was applied — journals + staging are
///   deleted and NO file restoration is attempted.
pub fn repair(wiki: &Wiki) -> Result<Value, EngineError> {
    let found = scan(wiki);
    let journals: Vec<PathBuf> = found
        .iter()
        .filter_map(|l| match l {
            Leftover::Journal(p) => Some(p.clone()),
            _ => None,
        })
        .collect();
    let mut backups: Vec<PathBuf> = found
        .iter()
        .filter_map(|l| match l {
            Leftover::BackupDir(p) => Some(p.clone()),
            _ => None,
        })
        .collect();
    backups.sort_by_key(|p| {
        backup_key(&p.file_name().unwrap().to_string_lossy()).unwrap_or((u64::MAX, u64::MAX))
    });

    let mut restored: Vec<String> = Vec::new();
    let mut unlinked: Vec<String> = Vec::new();
    let mut removed: Vec<String> = Vec::new();
    let mut run_ids: BTreeSet<String> = BTreeSet::new();
    let mut journal_ops: Vec<(String, String)> = Vec::new();

    for j in &journals {
        let (rid, ops) = parse_journal_ops(j);
        if let Some(r) = rid {
            run_ids.insert(r);
        }
        journal_ops.extend(ops);
    }

    // Restore only when a backup exists — the journal-only branch MUST NOT
    // restore and MUST NOT unlink destinations (nothing was applied).
    if !backups.is_empty() {
        // `backups` is sorted oldest-generation first, so apply per-dir in
        // order — the newest generation's copy lands last and wins (§5.4).
        let mut covered: BTreeSet<String> = BTreeSet::new();
        for bdir in &backups {
            for src in walk_files(bdir) {
                let rel = src.strip_prefix(bdir).unwrap().to_string_lossy().replace('\\', "/");
                let dst = wiki.p(&rel);
                if let Some(parent) = dst.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(&src, &dst)?;
                covered.insert(rel.clone());
                restored.push(rel);
            }
        }
        // Unlink newly-written destinations the backup does not cover.
        for (kind, rel) in &journal_ops {
            if kind == "write" && !covered.contains(rel) {
                let dst = wiki.p(rel);
                if dst.symlink_metadata().is_ok() {
                    fs::remove_file(&dst)?;
                    unlinked.push(rel.clone());
                }
            }
        }
    }

    // Delete leftovers: backups, journals, dead staging dirs of the
    // journaled runs plus any marker-less staging dir.
    for bdir in &backups {
        fs::remove_dir_all(bdir)?;
        removed.push(bdir.file_name().unwrap().to_string_lossy().to_string());
    }
    for j in &journals {
        fs::remove_file(j)?;
        removed.push(j.file_name().unwrap().to_string_lossy().to_string());
    }
    for d in list_dirs(&state_staging(wiki)) {
        let dead = !staging_is_live(&d);
        let owned = d
            .file_name()
            .map(|n| run_ids.contains(&*n.to_string_lossy()))
            .unwrap_or(false);
        if dead || owned {
            fs::remove_dir_all(&d)?;
            removed.push(format!("staging/{}", d.file_name().unwrap().to_string_lossy()));
        }
    }

    Ok(json!({
        "repaired": !restored.is_empty() || !removed.is_empty(),
        "restored": restored,
        "unlinked": unlinked,
        "removed": removed,
    }))
}

fn state_staging(wiki: &Wiki) -> PathBuf {
    wiki.p(".state/staging")
}

fn walk_files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).into_iter().flatten().flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                out.push(p);
            }
        }
    }
    out
}
