//! `.state/index.sqlite` — derived, disposable index (spec §10.1).
//! Schema pinned by `PRAGMA user_version`; `meta.built_at_revision` is the
//! watermark readers compare against `manifest.revision` — a stale index
//! triggers the canonical-scan fallback, never wrong rows.
//!
//! `Txn::commit` calls `on_commit` inside the commit critical section AFTER
//! apply and BEFORE receipt/operations/manifest — the same slot Python's
//! `ftx.apply_txn`/`rebuild`/`restamp` occupied (transaction.py:320-328).

use std::fs;
use std::path::PathBuf;

use fs4::fs_std::FileExt;
use rusqlite::{Connection, OpenFlags};

use crate::err::EngineError;
use crate::store::Wiki;

pub const SCHEMA_VERSION: i64 = 1;
pub const SCHEMA: &str = include_str!("schema.sql");

pub struct Index {
    conn: Connection,
    path: PathBuf,
}

impl Index {
    /// Open `.state/index.sqlite`, creating the schema when absent. A
    /// `PRAGMA user_version` mismatch drops and recreates — the index is a
    /// disposable projection, never canonical data.
    pub fn open_or_rebuild(wiki: &Wiki) -> Result<Index, EngineError> {
        let path = wiki.p(".state/index.sqlite");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let conn = match Connection::open(&path) {
            Ok(c) => c,
            Err(_) => {
                // not a database file — disposable, recreate
                let _ = fs::remove_file(&path);
                Connection::open(&path)?
            }
        };
        let version: i64 = match conn.pragma_query_value(None, "user_version", |r| r.get(0))
        {
            Ok(v) => v,
            Err(_) => {
                // unreadable/corrupt — recreate from scratch
                drop(conn);
                let _ = fs::remove_file(&path);
                return Self::open_or_rebuild(wiki);
            }
        };
        if version != SCHEMA_VERSION {
            drop_tables(&conn)?;
            conn.execute_batch(SCHEMA)?;
            conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
            conn.execute(
                "INSERT OR REPLACE INTO meta (k, v) VALUES ('schema_version', ?1)",
                [SCHEMA_VERSION.to_string()],
            )?;
            conn.execute(
                "INSERT OR REPLACE INTO meta (k, v) VALUES ('built_at_revision', '-1')",
                [],
            )?;
        }
        Ok(Index { conn, path })
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn path(&self) -> &PathBuf {
        &self.path
    }

    /// `meta.built_at_revision` — the revision this index was built/patched
    /// against; -1 when never built.
    pub fn built_at_revision(&self) -> Result<i64, EngineError> {
        let v: String = self.conn.query_row(
            "SELECT v FROM meta WHERE k = 'built_at_revision'",
            [],
            |r| r.get(0),
        )?;
        Ok(v.parse().unwrap_or(-1))
    }

    pub fn set_built_at_revision(&self, rev: i64) -> Result<(), EngineError> {
        self.conn.execute(
            "INSERT OR REPLACE INTO meta (k, v) VALUES ('built_at_revision', ?1)",
            [rev.to_string()],
        )?;
        Ok(())
    }

    /// Watermark behind the manifest → readers must use the canonical
    /// fallback scan.
    pub fn is_stale(&self, wiki: &Wiki) -> Result<bool, EngineError> {
        Ok(self.built_at_revision()? != wiki.revision()?)
    }

    /// Whole-index rebuild, single-flighted by `.state/locks/index.lock`
    /// (same never-delete lifecycle as commit.lock, §5.6). Rebuilds from
    /// canonical files only (claims/*.json, .state/dir-cursors.json) and
    /// stamps the watermark to the pinned revision — §10.2.
    pub fn rebuild_under_index_lock(
        wiki: &Wiki,
        at_revision: i64,
    ) -> Result<serde_json::Value, EngineError> {
        let lock_path = wiki.p(".state/locks/index.lock");
        if let Some(parent) = lock_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .open(&lock_path)?;
        file.lock_exclusive()?;
        let idx = Index::open_or_rebuild(wiki)?;
        idx.conn.execute("DELETE FROM claims_index", [])?;
        idx.conn.execute("DELETE FROM claims_fts", [])?;
        idx.conn.execute("DELETE FROM dir_cursors", [])?;
        let mut rows = 0usize;
        for claim in crate::claims::list(wiki)? {
            let row = crate::claims::index_row_from(&claim);
            idx.conn.execute(
                "INSERT OR REPLACE INTO claims_index(id, subject, predicate, status, version, path)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                rusqlite::params![
                    row.id, row.subject, row.predicate, row.status, row.version, row.path
                ],
            )?;
            idx.conn.execute(
                "INSERT INTO claims_fts(claim_id, body) VALUES (?1, ?2)",
                rusqlite::params![row.id, crate::claims::body_text(&claim)],
            )?;
            rows += 1;
        }
        // dir_cursors is a derived projection of .state/dir-cursors.json (§8)
        if let Some(cursors) = wiki.load_json_opt(".state/dir-cursors.json")? {
            if let Some(map) = cursors.as_object() {
                for (p, meta) in map {
                    idx.conn.execute(
                        "INSERT OR REPLACE INTO dir_cursors(path, mtime_ns, size)
                         VALUES (?1, ?2, ?3)",
                        rusqlite::params![
                            p,
                            meta["mtime_ns"].as_i64().unwrap_or(0),
                            meta["size"].as_i64().unwrap_or(0)
                        ],
                    )?;
                }
            }
        }
        idx.set_built_at_revision(at_revision)?;
        let _ = file.unlock();
        Ok(serde_json::json!({"rows": rows, "revision": at_revision}))
    }

    /// Inside-commit incremental update — port of `ftx.apply_txn`
    /// (ftx.py:60-93). Patches `claims_index` + `claims_fts` from the
    /// staged op list and stamps the watermark to `rev`. Runs inside
    /// commit's flock AFTER the JSON apply, so claim bodies are read from
    /// their live destinations. All row work sits in one SQLite
    /// transaction: an error discards partial rows (Python's no-commit
    /// discard) and aborts the commit loudly; the JSON restore in
    /// commit's error path keeps objects authoritative.
    ///
    /// Projection parity with `rebuild_under_index_lock` (which scans
    /// `claims::list`): only canonical `claims/claim_*.json` writes are
    /// indexed — nested (`_archive/`) or non-`claim_` names are never
    /// inserted. Deletes still clear any `claims/*.json` rel so rows
    /// a prior build could have held are removed.
    pub fn apply_txn(
        &self,
        wiki: &Wiki,
        ops: &[[String; 2]],
        rev: i64,
    ) -> Result<(), EngineError> {
        let tx = self.conn.unchecked_transaction()?;
        for op in ops {
            let [kind, rel] = op;
            if !rel.starts_with("claims/") || !rel.ends_with(".json") {
                continue;
            }
            let cid = &rel["claims/".len()..rel.len() - ".json".len()];
            // delete-then-upsert: covers both op kinds and id renames.
            tx.execute(
                "DELETE FROM claims_fts WHERE claim_id = ?1",
                rusqlite::params![cid],
            )?;
            tx.execute(
                "DELETE FROM claims_index WHERE id = ?1 OR path = ?2",
                rusqlite::params![cid, rel],
            )?;
            // Rebuild only ever holds canonical claim rels — keep the
            // patch path on the same projection.
            let canonical = kind == "write" && !cid.contains('/') && cid.starts_with("claim_");
            if canonical {
                let text = fs::read_to_string(wiki.p(rel))?;
                let claim: serde_json::Value = serde_json::from_str(&text)?;
                let row = crate::claims::index_row_from(&claim);
                tx.execute(
                    "INSERT OR REPLACE INTO claims_index(id, subject, predicate, status, version, path)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    rusqlite::params![
                        row.id, row.subject, row.predicate, row.status, row.version, row.path
                    ],
                )?;
                tx.execute(
                    "DELETE FROM claims_fts WHERE claim_id = ?1",
                    rusqlite::params![row.id],
                )?;
                tx.execute(
                    "INSERT INTO claims_fts(claim_id, body) VALUES (?1, ?2)",
                    rusqlite::params![row.id, crate::claims::body_text(&claim)],
                )?;
            }
        }
        tx.execute(
            "INSERT OR REPLACE INTO meta (k, v) VALUES ('built_at_revision', ?1)",
            [rev.to_string()],
        )?;
        tx.commit()?;
        Ok(())
    }
}

fn drop_tables(conn: &Connection) -> Result<(), EngineError> {
    conn.execute_batch(
        "DROP TABLE IF EXISTS claims_fts;
         DROP TABLE IF EXISTS claims_index;
         DROP TABLE IF EXISTS dep_edges;
         DROP TABLE IF EXISTS jev_answers;
         DROP TABLE IF EXISTS dir_cursors;
         DROP TABLE IF EXISTS meta;",
    )?;
    Ok(())
}

/// Commit seam called by `Txn::commit` under the flock, after apply and
/// before receipt/manifest (spec §5.1). Ports transaction.py:320-328:
///   - claim-touching commit + usable index → `apply_txn` patch at new_rev
///   - claim-touching commit + unusable index → whole rebuild at new_rev
///   - claim-free commit + usable index → restamp watermark only, so
///     readers keep serving indexed rows instead of falling back after a
///     no-op revision bump
///   - claim-free commit + unusable index → untouched (stale stays stale;
///     readers keep the canonical-scan fallback, §10.2).
/// "Usable" = ftx.health contract: watermark == manifest revision at commit
/// entry (new_rev - 1). Never stamps a watermark ahead of content the
/// index does not actually hold, and never creates the file on a
/// claim-free commit — absent stays absent (probe → not stale), the index
/// is disposable and lazily created by readers or the next claim commit.
pub fn on_commit(wiki: &Wiki, ops: &[[String; 2]], new_rev: i64) -> Result<(), EngineError> {
    let touches_claims = ops.iter().any(|op| op[1].starts_with("claims/"));
    if !touches_claims && !wiki.p(".state/index.sqlite").is_file() {
        return Ok(());
    }
    let idx = Index::open_or_rebuild(wiki)?;
    let usable = idx.built_at_revision()? == new_rev - 1;
    if touches_claims {
        if usable {
            idx.apply_txn(wiki, ops, new_rev)
        } else {
            Index::rebuild_under_index_lock(wiki, new_rev).map(|_| ())
        }
    } else if usable {
        idx.set_built_at_revision(new_rev)
    } else {
        Ok(())
    }
}

/// Read-only staleness probe — opens `.state/index.sqlite` without creating
/// it. `Ok(None)` = absent/not stale; `Ok(Some(built))` = watermark vs the
/// manifest revision decides staleness (`built != wiki.revision()` → stale);
/// corrupt/unreadable/schema-mismatched files report `Some(-1)`.
pub fn staleness_probe(wiki: &Wiki) -> Result<Option<i64>, EngineError> {
    let path = wiki.p(".state/index.sqlite");
    if !path.is_file() {
        return Ok(None);
    }
    let conn = match Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY) {
        Ok(c) => c,
        Err(_) => return Ok(Some(-1)),
    };
    let version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap_or(-1);
    if version != SCHEMA_VERSION {
        return Ok(Some(-1));
    }
    let built: i64 = conn
        .query_row("SELECT v FROM meta WHERE k = 'built_at_revision'", [], |r| {
            let s: String = r.get(0)?;
            Ok(s.parse().unwrap_or(-1))
        })
        .unwrap_or(-1);
    Ok(Some(built))
}
