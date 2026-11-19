//! PLAN -> STAGE -> VALIDATE -> COMMIT transaction engine — spec §5,
//! porting skill/llm-wiki/scripts/wikicore/transaction.py.
//!
//! Commit order (spec §5.1, corrected write-ahead order):
//!   lock → repair-gate → base_revision check → expect-stamp re-check
//!   → index-watermark overlay patch → materialize stage_state files
//!   → `journal.json` + `journal-<run>.json`  (BEFORE apply)
//!   → backup every existing target to `.backup-<rev>/`
//!   → apply (atomic_write / unlink)
//!   → commit hook (§10.2 seam: [`set_commit_hook`]; the core is
//!     index-free — the embedding CLI registers its projection updater)
//!   → receipt + operations.jsonl + manifest revision bump
//!   → clear journals + backup + staging
//!
//! The repair gate runs UNDER the flock: an in-flight commit's own journal
//! is never visible to it, and a waiting commit sees a held lock (exit 5),
//! never a repair_required (exit 3).

pub mod lock;
pub mod overlay;
pub mod recovery;
pub mod stamps;

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde_json::{json, Map, Value};

use crate::err::EngineError;
use crate::store::{Wiki, SKILL_VERSION};
use crate::util::{self, atomic_write_checked, write_json_atomic};

use lock::Lock;
use overlay::{clean_rel, last_op_for, replace_or_append, write_state_file, StagedOp};
use stamps::Stamp;

/// Python `claims.INDEX_REL` — staged writes of this file get the §5.3
/// revision watermark patched to current+1 under the flock.
pub const CLAIMS_INDEX_REL: &str = ".state/claims-index.json";

/// Derived-index updater invoked INSIDE the commit critical section after
/// apply (transaction.py's ftx patch/rebuild/restamp step). `ops` is the
/// journal op list, `new_rev` the revision this commit creates.
pub type CommitHook = fn(&Wiki, &[[String; 2]], i64) -> Result<(), EngineError>;

static COMMIT_HOOK: OnceLock<CommitHook> = OnceLock::new();

/// Register the process-wide commit hook (idempotent — first registration
/// wins; a library embedding must call this before its first commit).
pub fn set_commit_hook(hook: CommitHook) {
    let _ = COMMIT_HOOK.set(hook);
}

/// The registered hook, if any.
pub fn commit_hook() -> Option<CommitHook> {
    COMMIT_HOOK.get().copied()
}

/// `ids.new("run")` — Python parity (transaction.py:124 mints
/// `self.run_id = ids.new("run")`).
pub fn mint_run_id() -> String {
    crate::ids::new("run")
}

#[derive(Debug, Default, Clone)]
pub struct TxnOpts {
    /// Skip the under-flock repair gate (documented escape hatch;
    /// `doctor --fix` is always exempt).
    pub allow_repair: bool,
    /// Receipt `command` label (e.g. "wiki-ingest"); defaults to "txn".
    pub command: Option<String>,
}

pub struct Txn<'w> {
    pub wiki: &'w Wiki,
    pub run_id: String,
    pub command: String,
    pub started_at: String,
    pub base_revision: i64,
    pub allow_repair: bool,
    pub staging_dir: PathBuf,
    staged: Vec<StagedOp>,
    /// rel → unserialized stage_state value; composes repeated calls.
    state_overlay: HashMap<String, Value>,
    /// rel → expected stamp; `None` = expect-absent (file must NOT appear).
    expected: HashMap<String, Option<Stamp>>,
    /// receipt payloads, populated by callers (Python parity).
    pub changes: Map<String, Value>,
    pub warnings: Vec<Value>,
    pub conflicts: Vec<Value>,
}

impl<'w> Txn<'w> {
    /// Begin a txn against `run_id` (caller-minted — compile parks multi-step
    /// artifacts in `.state/staging/<run_id>/` between commands).
    pub fn begin(wiki: &'w Wiki, run_id: &str) -> Result<Txn<'w>, EngineError> {
        Self::begin_with(wiki, run_id, TxnOpts::default())
    }

    pub fn begin_with(
        wiki: &'w Wiki,
        run_id: &str,
        opts: TxnOpts,
    ) -> Result<Txn<'w>, EngineError> {
        let staging_dir = wiki.p(&format!(".state/staging/{}", run_id));
        fs::create_dir_all(&staging_dir)?;
        // §5.5 heartbeat: touched on entry so maintenance can tell a live
        // run from junk-eligible leftovers.
        fs::File::create(staging_dir.join(".live"))?;
        Ok(Txn {
            wiki,
            run_id: run_id.to_string(),
            command: opts.command.unwrap_or_else(|| "txn".to_string()),
            started_at: util::now_iso(),
            base_revision: wiki.revision()?,
            allow_repair: opts.allow_repair || wiki.allow_repair,
            staging_dir,
            staged: Vec::new(),
            state_overlay: HashMap::new(),
            expected: HashMap::new(),
            changes: Map::new(),
            warnings: Vec::new(),
            conflicts: Vec::new(),
        })
    }

    pub fn run_id(&self) -> &str {
        &self.run_id
    }

    pub fn staging_dir(&self) -> &Path {
        &self.staging_dir
    }

    /// Staging path for `rel` (cleaned). Callers materialize probe files
    /// here (sources.py runs markitdown against the staged path).
    pub fn staging_path(&self, rel: &str) -> PathBuf {
        self.staging_dir.join(clean_rel(rel))
    }

    /// Ordered ops as `(kind, rel)` — the journal payload shape.
    pub fn ops(&self) -> Vec<[String; 2]> {
        self.staged.iter().map(|op| op.journal_pair()).collect()
    }

    /// Staged overlay value for `rel`, if a `stage_state`/`stage_write`
    /// has queued one (compile.py reads `txn._state_overlay` directly).
    pub fn staged_value(&self, rel: &str) -> Option<&Value> {
        self.state_overlay.get(&clean_rel(rel))
    }

    // ---- STAGE ----------------------------------------------------------

    /// Queue `rel` := `bytes`. Bytes go to the staging dir NOW; the
    /// destination is never touched until apply. Registers an expect-stamp
    /// (or expect-absent) against the destination — §5.2 registration rule.
    pub fn stage_write(&mut self, rel: &str, bytes: &[u8]) -> Result<(), EngineError> {
        let rel = clean_rel(rel);
        let dst = self.wiki.p(&rel);
        stamps::check_no_symlink_components(&dst, &self.wiki.root)?;
        let src = self.staging_dir.join(&rel);
        if let Some(parent) = src.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&src, bytes)?;
        replace_or_append(
            &mut self.staged,
            StagedOp::Write { rel: rel.clone(), src },
        );
        self.state_overlay.remove(&rel);
        self.expected.insert(rel, Stamp::capture(&dst)?);
        Ok(())
    }

    /// Queue `rel` := `value` as canonical JSON (pretty + trailing newline).
    /// The value lands in the overlay and is materialized ONCE under the
    /// flock by commit — repeated calls compose without re-serializing.
    pub fn stage_state(&mut self, rel: &str, value: &Value) -> Result<(), EngineError> {
        let rel = clean_rel(rel);
        let dst = self.wiki.p(&rel);
        stamps::check_no_symlink_components(&dst, &self.wiki.root)?;
        if !self.state_overlay.contains_key(&rel) {
            // read-time stat (§5.2 registration rule for stage_state)
            self.expected.insert(rel.clone(), Stamp::capture(&dst)?);
        }
        self.state_overlay.insert(rel.clone(), value.clone());
        let src = self.staging_dir.join(&rel);
        replace_or_append(
            &mut self.staged,
            StagedOp::Write { rel, src },
        );
        Ok(())
    }

    /// Read-modify-write form (Python `stage_state(rel, update_fn)` parity):
    /// overlay first, then disk, then `{}`; `f` mutates; the result is
    /// staged. Returns the updated value.
    pub fn stage_state_with<F>(&mut self, rel: &str, f: F) -> Result<Value, EngineError>
    where
        F: FnOnce(&mut Value),
    {
        let rel = clean_rel(rel);
        let dst = self.wiki.p(&rel);
        stamps::check_no_symlink_components(&dst, &self.wiki.root)?;
        let mut data = match self.state_overlay.get(&rel) {
            Some(v) => v.clone(),
            None => {
                self.expected.insert(rel.clone(), Stamp::capture(&dst)?);
                match fs::read_to_string(&dst) {
                    Ok(text) => serde_json::from_str(&text)?,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => json!({}),
                    Err(e) => return Err(e.into()),
                }
            }
        };
        f(&mut data);
        self.state_overlay.insert(rel.clone(), data.clone());
        let src = self.staging_dir.join(&rel);
        replace_or_append(&mut self.staged, StagedOp::Write { rel, src });
        Ok(data)
    }

    /// Queue `rel` to be unlinked at apply. A pending `stage_state` overlay
    /// is popped and materialized once so the queued write op still has a
    /// file to apply before the delete runs (write-then-delete parity).
    pub fn stage_delete(&mut self, rel: &str) -> Result<(), EngineError> {
        let rel = clean_rel(rel);
        let dst = self.wiki.p(&rel);
        stamps::check_no_symlink_components(&dst, &self.wiki.root)?;
        if let Some(data) = self.state_overlay.remove(&rel) {
            for op in &self.staged {
                if let StagedOp::Write { rel: r, src } = op {
                    if *r == rel {
                        write_state_file(src, &data)?;
                        break;
                    }
                }
            }
        }
        replace_or_append(&mut self.staged, StagedOp::Delete { rel: rel.clone() });
        self.expected.insert(rel, Stamp::capture(&dst)?);
        Ok(())
    }

    /// Register an expect-stamp for a file the caller read but does not
    /// stage (or expect-absent when the path does not exist).
    pub fn expect(&mut self, rel: &str) -> Result<(), EngineError> {
        let rel = clean_rel(rel);
        let dst = self.wiki.p(&rel);
        self.expected.insert(rel, Stamp::capture_in(&dst, &self.wiki.root)?);
        Ok(())
    }

    /// Overlay-first read (§5.3 read-your-writes): staged bytes, then the
    /// unserialized `stage_state` value, then a `Delete` as absent, else disk.
    pub fn read(&self, rel: &str) -> Result<Option<Vec<u8>>, EngineError> {
        let rel = clean_rel(rel);
        match last_op_for(&self.staged, &rel) {
            Some(StagedOp::Delete { .. }) => return Ok(None),
            Some(StagedOp::Write { src, .. }) => {
                if let Some(v) = self.state_overlay.get(&rel) {
                    return Ok(Some(
                        format!("{}\n", serde_json::to_string_pretty(v)?).into_bytes(),
                    ));
                }
                return Ok(Some(fs::read(src)?));
            }
            None => {}
        }
        if let Some(v) = self.state_overlay.get(&rel) {
            return Ok(Some(
                format!("{}\n", serde_json::to_string_pretty(v)?).into_bytes(),
            ));
        }
        match fs::read(self.wiki.p(&rel)) {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// `read` parsed as JSON.
    pub fn read_json(&self, rel: &str) -> Result<Option<Value>, EngineError> {
        match self.read(rel)? {
            Some(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
            None => Ok(None),
        }
    }

    // ---- COMMIT (spec §5.1 order) ---------------------------------------

    pub fn commit(mut self) -> Result<Value, EngineError> {
        // flock first — the repair gate MUST run under it (§5.1).
        let _lock = Lock::acquire(self.wiki)?;

        if !self.allow_repair && recovery::repair_needed(self.wiki) {
            return Err(EngineError::Repair(
                "leftover transaction journal or backup; run doctor --fix or pass --allow-repair"
                    .to_string(),
            ));
        }

        let current = self.wiki.revision()?;
        if current != self.base_revision {
            return Err(EngineError::Conflict(format!(
                "stale commit: base_revision={} but wiki is at {}; re-read and rebase",
                self.base_revision, current
            )));
        }

        // expect-stamp re-check on every read file (§5.2).
        for (rel, stamp) in &self.expected {
            let dst = self.wiki.p(rel);
            stamps::check_no_symlink_components(&dst, &self.wiki.root)?;
            match stamp {
                Some(s) => {
                    if !s.matches(&dst)? {
                        return Err(EngineError::Conflict(format!(
                            "expect_stamp mismatch on {}",
                            rel
                        )));
                    }
                }
                None => {
                    if dst.symlink_metadata().is_ok() {
                        return Err(EngineError::Conflict(format!(
                            "expect-absent violated: {} appeared mid-commit",
                            rel
                        )));
                    }
                }
            }
        }

        // §5.3 watermark hook: a staged claims-index write must carry the
        // revision THIS commit creates (transaction.py:239-257).
        for i in 0..self.staged.len() {
            let StagedOp::Write { rel, src } = &self.staged[i] else {
                continue;
            };
            if rel != CLAIMS_INDEX_REL {
                continue;
            }
            let rel = rel.clone();
            let src = src.clone();
            if let Some(idx) = self.state_overlay.get_mut(&rel) {
                // lazy case: patch the overlay before it materializes
                if idx.get("revision").and_then(Value::as_i64) != Some(current + 1) {
                    idx["revision"] = json!(current + 1);
                }
            } else if let Ok(text) = fs::read_to_string(&src) {
                if let Ok(mut idx) = serde_json::from_str::<Value>(&text) {
                    if idx.get("revision").and_then(Value::as_i64) != Some(current + 1) {
                        idx["revision"] = json!(current + 1);
                        write_state_file(&src, &idx)?;
                    }
                }
            }
        }

        // Serialize every still-current stage_state overlay exactly once,
        // under the flock (transaction.py `_materialize_state`).
        for op in &self.staged {
            if let StagedOp::Write { rel, src } = op {
                if let Some(data) = self.state_overlay.get(rel.as_str()) {
                    write_state_file(src, data)?;
                }
            }
        }

        // (a) every destination's parent must exist or be creatable — the
        // symlink walk runs first so we never mkdir through a link.
        for op in &self.staged {
            let dst = self.wiki.p(op.rel());
            stamps::check_no_symlink_components(&dst, &self.wiki.root)?;
            if let Some(parent) = dst.parent() {
                fs::create_dir_all(parent)?;
            }
        }

        // (b) receipt built up front — the journal carries it.
        let receipt = json!({
            "run_id": self.run_id,
            "command": self.command,
            "skill_version": SKILL_VERSION,
            "started_at": self.started_at,
            "finished_at": util::now_iso(),
            "changes": Value::Object(self.changes.clone()),
            "warnings": self.warnings,
            "conflicts": self.conflicts,
            "revision": {"before": current, "after": current + 1},
        });
        let journal_payload =
            format!("{}\n", serde_json::to_string_pretty(&json!({
                "receipt": receipt,
                "ops": self.ops(),
            }))?);
        let journal_path = self.wiki.p(".state/journal.json");
        let run_journal_path = self
            .wiki
            .p(&format!(".state/journal-{}.json", self.run_id));
        // journal written BEFORE apply — the crash breadcrumb.
        for jp in [&run_journal_path, &journal_path] {
            atomic_write_checked(jp, &self.wiki.root, journal_payload.as_bytes())?;
        }

        let backup_dir = self.wiki.p(&format!(".state/.backup-{}", current));
        // §5.4 collision rule: a retried --allow-repair commit can target
        // the same revision — rename, never merge.
        if backup_dir.is_dir() {
            let mut n = 0u32;
            loop {
                let suffix = if n == 0 {
                    format!("{}", util::unix_secs())
                } else {
                    format!("{}-{}", util::unix_secs(), n)
                };
                let prev = self
                    .wiki
                    .p(&format!(".state/.backup-{}.prev-{}", current, suffix));
                if !prev.exists() {
                    fs::rename(&backup_dir, &prev)?;
                    break;
                }
                n += 1;
            }
        }

        // (c)+(d)+(e)+(f): backup → apply → index hook → receipt/state →
        // clear. ANY error inside restores from the backup and leaves the
        // journal behind for the repair gate (transaction.py:292-362).
        match self.apply_section(&backup_dir, current, &receipt, &journal_path, &run_journal_path)
        {
            Ok(()) => Ok(receipt),
            Err(e) => {
                let _ = self.rollback(&backup_dir);
                Err(e)
            }
        }
    }

    /// The critical section after journals are written: back up every
    /// existing target, apply each op, run the index hook, then write
    /// receipt + operations line + manifest bump and clear breadcrumbs.
    fn apply_section(
        &self,
        backup_dir: &Path,
        current: i64,
        receipt: &Value,
        journal_path: &Path,
        run_journal_path: &Path,
    ) -> Result<(), EngineError> {
        // (c) back up every existing target.
        let mut backed_up = false;
        for op in &self.staged {
            let dst = self.wiki.p(op.rel());
            if dst.symlink_metadata().is_ok() {
                let bpath = backup_dir.join(op.rel());
                if let Some(parent) = bpath.parent() {
                    fs::create_dir_all(parent)?;
                }
                stamps::check_no_symlink_components(&bpath, &self.wiki.root)?;
                fs::copy(&dst, &bpath)?;
                backed_up = true;
            }
        }
        // (d) apply per-file.
        for op in &self.staged {
            let dst = self.wiki.p(op.rel());
            match op {
                StagedOp::Write { src, .. } => {
                    atomic_write_checked(&dst, &self.wiki.root, &fs::read(src)?)?;
                }
                StagedOp::Delete { .. } => {
                    if dst.symlink_metadata().is_ok() {
                        fs::remove_file(&dst)?;
                    }
                }
            }
        }
        // §10.2 hook — the embedding binary's derived projection (the CLI's
        // sqlite index; transaction.py:314-328) stays live inside the same
        // critical section. Core itself ships without one.
        if let Some(hook) = commit_hook() {
            hook(self.wiki, &self.ops(), current + 1)?;
        }
        // (e) receipt + operations line + manifest revision bump.
        fs::create_dir_all(self.wiki.p(".state/operations"))?;
        write_json_atomic(
            &self
                .wiki
                .p(&format!(".state/operations/{}.receipt.json", self.run_id)),
            receipt,
        )?;
        append_operations_line(
            &self.wiki.p(".state/operations.jsonl"),
            &serde_json::to_string(receipt)?,
        )?;
        let mut state = self.wiki.state()?;
        state["revision"] = json!(current + 1);
        let ops_desc: Vec<String> = self
            .staged
            .iter()
            .map(|op| {
                let [kind, rel] = op.journal_pair();
                format!("{}:{}", kind, rel)
            })
            .collect();
        let mut hash_src = serde_json::to_string(&ops_desc)?;
        hash_src.truncate(64);
        state["content_hash"] = json!(hash_src);
        self.wiki.save_state(&state)?;
        // (f) clear journals + backup + staging.
        fs::remove_file(journal_path)?;
        if run_journal_path.symlink_metadata().is_ok() {
            fs::remove_file(run_journal_path)?;
        }
        if backed_up {
            let _ = fs::remove_dir_all(backup_dir);
        }
        let _ = fs::remove_dir_all(&self.staging_dir);
        Ok(())
    }

    /// Rollback = backup-restore (transaction.py:351-360): restore every
    /// `.backup-<rev>` entry; unlink newly-written destinations the backup
    /// does not cover. The journal stays behind for the repair gate.
    fn rollback(&self, backup_dir: &Path) -> Result<(), EngineError> {
        for op in &self.staged {
            let dst = self.wiki.p(op.rel());
            let bpath = backup_dir.join(op.rel());
            if bpath.symlink_metadata().is_ok() {
                fs::rename(&bpath, &dst)?;
            } else if matches!(op, StagedOp::Write { .. }) && dst.symlink_metadata().is_ok() {
                fs::remove_file(&dst)?;
            }
        }
        Ok(())
    }
}

/// Port of `_append_operations_line` (transaction.py:54-74): append one
/// JSONL record after amputating a torn tail left by a mid-append crash.
/// Public: the operations log is core state and embedders may append
/// their own receipt-style records.
pub fn append_operations_line(path: &Path, line: &str) -> Result<(), EngineError> {
    if path.symlink_metadata().is_ok() {
        let mut data = fs::read(path)?;
        let body_len = data.iter().rposition(|b| *b != b'\n').map(|i| i + 1).unwrap_or(0);
        let body = &data[..body_len];
        let tail_start = body.iter().rposition(|b| *b == b'\n').map(|i| i + 1).unwrap_or(0);
        let tail = &body[tail_start..];
        if !tail.is_empty() && serde_json::from_slice::<Value>(tail).is_err() {
            // torn final line: drop it before appending
            data.truncate(tail_start);
            fs::write(path, &data)?;
        }
        let needs_nl = {
            let data = fs::read(path)?;
            !data.is_empty() && !data.ends_with(b"\n")
        };
        if needs_nl {
            fs::OpenOptions::new().append(true).open(path)?.write_all(b"\n")?;
        }
    }
    let mut f = fs::OpenOptions::new().append(true).create(true).open(path)?;
    f.write_all(line.as_bytes())?;
    f.write_all(b"\n")?;
    Ok(())
}

