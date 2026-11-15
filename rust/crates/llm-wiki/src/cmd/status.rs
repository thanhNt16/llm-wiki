//! `status` — wiki health snapshot (wiki.py:cmd_status 222-242 parity,
//! extended per spec §6.1/plan Task 11).
//!
//! Payload key set: `{initialized, project, skill_version, revision,
//! counts{sources,claims,decisions,pages,pending}, pending_versions,
//! stale_artifacts}`. An uninitialized root emits `{"initialized": false}`
//! only (wiki.py:224-226).
//!
//! `stale_artifacts` is the deps-graph stale list plus the index indicator
//! `.state/index.sqlite` when the index watermark is behind the manifest —
//! the spec §7.5/ES1 fix that keeps the full wiki.py:232-240 key set while
//! surfacing staleness as a list instead of a bare count.

use serde_json::{json, Value};

use crate::err::EngineError;
use crate::evidence::sources;
use crate::index;
use crate::store::{Wiki, SKILL_VERSION};

fn count_claims(wiki: &Wiki) -> Result<i64, EngineError> {
    let dir = wiki.p("claims");
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut n = 0i64;
    for e in std::fs::read_dir(&dir)? {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        if e.file_type()?.is_file() && name.starts_with("claim_") && name.ends_with(".json") {
            n += 1;
        }
    }
    Ok(n)
}

fn count_sources(wiki: &Wiki) -> Result<i64, EngineError> {
    let dir = wiki.p("sources");
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut n = 0i64;
    for e in std::fs::read_dir(&dir)? {
        let e = e?;
        if e.file_type()?.is_dir() && e.path().join("manifest.json").is_file() {
            n += 1;
        }
    }
    Ok(n)
}

fn count_json_files(dir: &std::path::Path) -> Result<i64, EngineError> {
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut n = 0i64;
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        if e.file_type()?.is_file()
            && e.file_name().to_string_lossy().ends_with(".json")
        {
            n += 1;
        }
    }
    Ok(n)
}

/// Recursive `wiki/**/*.md` walk.
fn count_pages(dir: &std::path::Path) -> Result<i64, EngineError> {
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut n = 0i64;
    for e in std::fs::read_dir(dir)? {
        let e = e?;
        let ft = e.file_type()?;
        if ft.is_dir() {
            n += count_pages(&e.path())?;
        } else if ft.is_file() && e.file_name().to_string_lossy().ends_with(".md") {
            n += 1;
        }
    }
    Ok(n)
}

/// Read-only index staleness probe — compares `meta.built_at_revision` to
/// the manifest revision via `index::staleness_probe`. Absent file → not
/// stale; corrupt/unreadable → stale.
fn index_is_stale(wiki: &Wiki) -> Result<bool, EngineError> {
    match index::staleness_probe(wiki)? {
        None => Ok(false),
        Some(built) => Ok(built != wiki.revision()?),
    }
}

/// `.state/dependencies.json` `stale` list (deps.py:stale_artifacts —
/// stored order) with the index indicator appended when the watermark is
/// behind the manifest revision.
fn stale_artifacts(wiki: &Wiki) -> Result<Vec<Value>, EngineError> {
    let mut out: Vec<Value> = wiki
        .load_json_opt(".state/dependencies.json")?
        .and_then(|d| d.get("stale").and_then(Value::as_array).cloned())
        .unwrap_or_default();
    if index_is_stale(wiki)? {
        out.push(json!(".state/index.sqlite"));
    }
    Ok(out)
}

pub fn run(wiki: &Wiki) -> Result<Value, EngineError> {
    if !wiki.exists() {
        return Ok(json!({"initialized": false}));
    }
    let pending = sources::pending_versions(wiki)?;
    let counts = json!({
        "sources": count_sources(wiki)?,
        "claims": count_claims(wiki)?,
        "decisions": count_json_files(&wiki.p("decisions"))?,
        "pages": count_pages(&wiki.p("wiki"))?,
        "pending": pending.len() as i64,
    });
    Ok(json!({
        "initialized": true,
        "project": wiki.config()?.get("project").cloned().unwrap_or(Value::Null),
        "skill_version": SKILL_VERSION,
        "revision": wiki.revision()?,
        "counts": counts,
        "pending_versions": pending.len() as i64,
        "stale_artifacts": stale_artifacts(wiki)?,
    }))
}
