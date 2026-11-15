//! Claim records — canonical `claims/*.json` io + index/fallback read path.
//! Port of wikicore/claims.py scan semantics; claim files are the only
//! source of truth, `claims_index` is a disposable projection (spec §10.1).

use serde_json::Value;

use crate::err::EngineError;
use crate::index::Index;
use crate::store::Wiki;
use crate::txn::Txn;

/// Tolerant claim record (claim.schema.json fields; extras ignored).
#[derive(Debug, Clone, Default)]
pub struct Claim {
    pub id: String,
    pub subject: Value,
    pub predicate: String,
    pub value: Value,
    pub status: String,
    pub authority: Value,
    pub scope: Value,
    pub validity: Value,
    pub evidence: Vec<Value>,
    /// Full JSON object — write/round-trips go through this verbatim.
    pub raw: Value,
}

impl Claim {
    pub fn from_json(v: &Value) -> Claim {
        let get = |k: &str| v.get(k).cloned().unwrap_or(Value::Null);
        Claim {
            id: get("id").as_str().unwrap_or("").to_string(),
            subject: get("subject"),
            predicate: get("predicate").as_str().unwrap_or("").to_string(),
            value: get("value"),
            status: get("status").as_str().unwrap_or("").to_string(),
            authority: get("authority"),
            scope: get("scope"),
            validity: get("validity"),
            evidence: get("evidence").as_array().cloned().unwrap_or_default(),
            raw: v.clone(),
        }
    }
}

/// Row shape for `claims_index` — identical whether served from the index
/// or the canonical-file fallback scan (index_reader_fallback gate).
#[derive(Debug, Clone, PartialEq)]
pub struct IndexRow {
    pub id: String,
    pub subject: String,
    pub predicate: String,
    pub status: String,
    pub version: i64,
    pub path: String,
}
pub fn claim_rel(id: &str) -> String {
    format!("claims/{id}.json")
}

fn archive_rel(id: &str) -> String {
    format!("claims/_archive/{id}.json")
}

/// `_index_entry` (claims.py:38) — the projected row for one claim.
pub fn index_row_from(claim: &Value) -> IndexRow {
    IndexRow {
        id: claim["id"].as_str().unwrap_or("").to_string(),
        subject: claim["subject"].as_str().unwrap_or("").to_string(),
        predicate: claim["predicate"].as_str().unwrap_or("").to_string(),
        status: claim["status"].as_str().unwrap_or("").to_string(),
        version: claim["version"].as_i64().unwrap_or(0),
        path: claim_rel(claim["id"].as_str().unwrap_or("")),
    }
}

/// `ftx.body_text` (ftx.py:18-24) — subject + predicate + value + summary +
/// title + sorted scope entries rendered "k v", space-joined.
pub fn body_text(claim: &Value) -> String {
    let mut parts: Vec<String> = Vec::new();
    for k in ["subject", "predicate", "value", "summary", "title"] {
        match claim.get(k) {
            Some(Value::String(s)) if !s.is_empty() => parts.push(s.clone()),
            Some(v @ (Value::Number(_) | Value::Bool(_))) => parts.push(v.to_string()),
            _ => {}
        }
    }
    if let Some(obj) = claim.get("scope").and_then(|s| s.as_object()) {
        let mut kvs: Vec<(&String, &Value)> = obj.iter().collect();
        kvs.sort_by(|a, b| a.0.cmp(b.0));
        for (k, v) in kvs {
            let vs = match v {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            parts.push(format!("{k} {vs}"));
        }
    }
    parts.join(" ")
}

/// Canonical scan — claims/*.json minus `_archive/`, sorted by filename
/// (claims.py:_scan_claims). Claim files named `claim_*.json` only.
pub fn list(wiki: &Wiki) -> Result<Vec<Value>, EngineError> {
    let mut out = Vec::new();
    let cdir = wiki.p("claims");
    if !cdir.is_dir() {
        return Ok(out);
    }
    let mut names: Vec<String> = Vec::new();
    for e in std::fs::read_dir(&cdir)? {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        if e.file_type()?.is_file() && name.starts_with("claim_") && name.ends_with(".json") {
            names.push(name);
        }
    }
    names.sort();
    for name in names {
        let text = std::fs::read_to_string(cdir.join(&name))?;
        out.push(serde_json::from_str(&text)?);
    }
    Ok(out)
}

/// Typed variant of `list`.
pub fn list_claims(wiki: &Wiki) -> Result<Vec<Claim>, EngineError> {
    Ok(list(wiki)?.iter().map(Claim::from_json).collect())
}

/// `claims.list_decisions` — canonical `decisions/decision_*.json` scan,
/// sorted by filename (the Python index path falls back to exactly this).
pub fn list_decisions(wiki: &Wiki) -> Result<Vec<Value>, EngineError> {
    let mut out = Vec::new();
    let ddir = wiki.p("decisions");
    if !ddir.is_dir() {
        return Ok(out);
    }
    let mut names: Vec<String> = Vec::new();
    for e in std::fs::read_dir(&ddir)? {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        if e.file_type()?.is_file() && name.starts_with("decision_") && name.ends_with(".json") {
            names.push(name);
        }
    }
    names.sort();
    for name in names {
        let text = std::fs::read_to_string(ddir.join(&name))?;
        out.push(serde_json::from_str(&text)?);
    }
    Ok(out)
}

/// `load_claim` (claims.py:97) — canonical first, then `_archive/`.
pub fn get(wiki: &Wiki, id: &str) -> Result<Option<Value>, EngineError> {
    if let Some(c) = wiki.load_json_opt(&claim_rel(id))? {
        return Ok(Some(c));
    }
    wiki.load_json_opt(&archive_rel(id))
}

/// `save_claim` — stage `claims/<id>.json` via txn stage_state.
pub fn write(_wiki: &Wiki, txn: &mut Txn, claim: &Value) -> Result<(), EngineError> {
    let id = claim["id"]
        .as_str()
        .ok_or_else(|| EngineError::Txn("claim requires an 'id'".into()))?;
    if !crate::ids::validate(id) || !id.starts_with("claim_") {
        return Err(EngineError::Txn(format!("invalid claim id: {id:?}")));
    }
    txn.stage_state(&claim_rel(id), claim)
}

/// `archive` — move a claim to `claims/_archive/` inside the txn.
pub fn archive(wiki: &Wiki, txn: &mut Txn, id: &str) -> Result<(), EngineError> {
    let rel = claim_rel(id);
    let v = wiki.load_json_opt(&rel)?;
    let v = v.ok_or_else(|| EngineError::Txn(format!("no claim {id:?} to archive")))?;
    txn.stage_state(&archive_rel(id), &v)?;
    txn.stage_delete(&rel)
}

/// `claims::index_rows` — read `claims_index` when the index is fresh;
/// otherwise fall back to the canonical scan and best-effort repopulate the
/// table (index_reader_fallback). Identical row shape either way.
pub fn index_rows(wiki: &Wiki) -> Result<Vec<IndexRow>, EngineError> {
    let idx = Index::open_or_rebuild(wiki)?;
    if !idx.is_stale(wiki)? {
        return index_rows_from_conn(idx.conn());
    }
    let mut rows: Vec<IndexRow> = Vec::new();
    let mut claims = Vec::new();
    for claim in list(wiki)? {
        rows.push(index_row_from(&claim));
        claims.push(claim);
    }
    // Best-effort repopulate — failures leave the fallback intact.
    let _ = repopulate(&idx, &claims);
    Ok(rows)
}

fn index_rows_from_conn(conn: &rusqlite::Connection) -> Result<Vec<IndexRow>, EngineError> {
    let mut stmt = conn.prepare(
        "SELECT id, subject, predicate, status, version, path FROM claims_index ORDER BY id",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(IndexRow {
                id: r.get::<_, String>(0)?,
                subject: r.get::<_, Option<String>>(1)?.unwrap_or_default(),
                predicate: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                status: r.get::<_, Option<String>>(3)?.unwrap_or_default(),
                version: r.get::<_, Option<i64>>(4)?.unwrap_or(0),
                path: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
            })
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn repopulate(idx: &Index, claims: &[Value]) -> Result<(), EngineError> {
    idx.conn().execute("DELETE FROM claims_index", [])?;
    for claim in claims {
        let row = index_row_from(claim);
        idx.conn().execute(
            "INSERT OR REPLACE INTO claims_index(id, subject, predicate, status, version, path)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            rusqlite::params![
                row.id, row.subject, row.predicate, row.status, row.version, row.path
            ],
        )?;
    }
    Ok(())
}

/// claims.py:27 — statuses that cannot support a decision and cannot
/// return to a non-terminal state.
pub const TERMINAL: &[&str] = &["superseded", "rejected"];

/// Read a canonical file honoring this txn's staged writes — overlay
/// (`stage_state`) first, then materialized staging bytes (`stage_write`),
/// then live disk. Returns `(value, from_staging)`.
fn staged_or_disk(
    wiki: &Wiki,
    txn: &Txn,
    rel: &str,
) -> Result<(Option<Value>, bool), EngineError> {
    if let Some(v) = txn.staged_value(rel) {
        return Ok((Some(v.clone()), true));
    }
    let staged = txn.staging_path(rel);
    if staged.is_file() {
        return Ok((Some(serde_json::from_str(&std::fs::read_to_string(
            staged,
        )?)?), true));
    }
    match wiki.load_json_opt(rel)? {
        Some(v) => Ok((Some(v), false)),
        None => Ok((None, false)),
    }
}

/// `claims.load_claim` (claims.py:97) — canonical file only (no archive
/// fallback); `Txn` on invalid/unknown ids.
pub fn load_claim(wiki: &Wiki, cid: &str) -> Result<Value, EngineError> {
    if !cid.starts_with("claim_") || !crate::ids::validate(cid) {
        return Err(EngineError::Txn(format!(
            "claim id invalid: {}",
            crate::schema::py_repr_str(cid)
        )));
    }
    wiki.load_json_opt(&claim_rel(cid))?
        .ok_or_else(|| EngineError::Txn(format!("unknown claim: {cid}")))
}

/// `claims.save_claim` (claims.py:107) — version bump-once-per-commit,
/// terminal-status guards, schema validation, derived index update, and
/// the file write staged into `txn`.
pub fn save_claim(wiki: &Wiki, txn: &mut Txn, claim: &mut Value) -> Result<(), EngineError> {
    let cid = claim["id"].as_str().unwrap_or("").to_string();
    if !crate::ids::validate(&cid) {
        return Err(EngineError::Txn(format!(
            "claim id invalid: {}",
            crate::schema::py_repr(&claim["id"])
        )));
    }
    let (existing, from_staging) = staged_or_disk(wiki, txn, &claim_rel(&cid))?;
    if let Some(existing) = existing {
        if !from_staging {
            claim["version"] = serde_json::json!(existing["version"].as_i64().unwrap_or(0) + 1);
        } else {
            claim["version"] = existing["version"].clone(); // bump already applied
        }
        claim["recorded_at"] = existing["recorded_at"].clone(); // immutable
        claim["updated_at"] = serde_json::json!(crate::util::now_iso());
        let old_status = existing.get("status").and_then(Value::as_str).unwrap_or("");
        let new_status = claim.get("status").and_then(Value::as_str).unwrap_or("");
        if TERMINAL.contains(&old_status) && !TERMINAL.contains(&new_status) {
            return Err(EngineError::Txn(format!(
                "claim {cid} is {old_status}; cannot return to {new_status}"
            )));
        }
    } else {
        claim["version"] = serde_json::json!(1);
        if claim.get("recorded_at").is_none() {
            claim["recorded_at"] = serde_json::json!(crate::util::now_iso());
        }
    }
    let status = claim.get("status").and_then(Value::as_str).unwrap_or("");
    let has_reason = crate::frontmatter::truthy(
        claim.get("acceptance").and_then(|a| a.get("reason")),
    );
    if TERMINAL.contains(&status)
        && !crate::frontmatter::truthy(claim.get("supersedes"))
        && !crate::frontmatter::truthy(claim.get("superseded_by"))
        && !has_reason
    {
        return Err(EngineError::Txn(format!(
            "terminal status on {cid} requires supersedes/superseded_by/reason"
        )));
    }
    let schema: Value = serde_json::from_str(crate::schema::CLAIM_SCHEMA)?;
    let errors = crate::schema::validate(claim, &schema);
    if !errors.is_empty() {
        return Err(EngineError::Txn(format!(
            "claim {cid} invalid: {}",
            errors.join("; ")
        )));
    }
    stage_index_update(txn, claim)?;
    txn.stage_state(&claim_rel(&cid), claim)?;
    Ok(())
}

/// `_load_index_for_staging` (claims.py:55) — seed the index overlay with
/// a rebuild-shaped empty index when the on-disk file is missing/corrupt.
pub(crate) fn ensure_index_for_staging(wiki: &Wiki, txn: &mut Txn) -> Result<(), EngineError> {
    if txn.staged_value(crate::txn::CLAIMS_INDEX_REL).is_some() {
        return Ok(());
    }
    match wiki.load_json_opt(crate::txn::CLAIMS_INDEX_REL) {
        Ok(Some(idx))
            if idx.get("claims").is_some_and(Value::is_object)
                && idx.get("decisions").is_some_and(Value::is_array) =>
        {
            return Ok(()); // shape-valid: stage_state re-reads it
        }
        Ok(_) => {}
        Err(_) => {}
    }
    // Rebuild-shaped seed: complete only when both stores are truly empty.
    let empty = store_dir_empty(wiki, "claims", "claim_")
        && store_dir_empty(wiki, "decisions", "decision_");
    txn.stage_state(
        crate::txn::CLAIMS_INDEX_REL,
        &serde_json::json!({"claims": {}, "decisions": [], "complete": empty}),
    )?;
    Ok(())
}

/// `_store_dir_empty` (claims.py:45) — listing only, no file reads.
fn store_dir_empty(wiki: &Wiki, store: &str, prefix: &str) -> bool {
    match std::fs::read_dir(wiki.p(store)) {
        Ok(rd) => !rd.flatten().any(|e| {
            let n = e.file_name().to_string_lossy().into_owned();
            n.starts_with(prefix) && n.ends_with(".json")
        }),
        Err(_) => true,
    }
}

/// `_index_entry` → `_stage_index_update` (claims.py:38+81): keep the
/// derived claims index inside the claim-write transaction.
fn stage_index_update(txn: &mut Txn, claim: &Value) -> Result<(), EngineError> {
    let row = index_row_from(claim);
    let cid = claim["id"].as_str().unwrap_or("").to_string();
    ensure_index_for_staging(txn.wiki, txn)?;
    txn.stage_state_with(crate::txn::CLAIMS_INDEX_REL, move |idx| {
        if !idx["claims"].is_object() {
            idx["claims"] = serde_json::json!({});
        }
        idx["claims"].as_object_mut().unwrap().insert(
            cid.clone(),
            serde_json::json!({
                "key": format!(
                    "{}/{}",
                    row.subject.trim().to_lowercase(),
                    row.predicate.trim().to_lowercase()
                ),
                "status": row.status,
                "subject": row.subject,
                "version": row.version,
            }),
        );
    })?;
    Ok(())
}

/// `claims.supersede` (claims.py:219) — CORRECTION: old stops immediately;
/// POLICY_CHANGE: old valid until the day before `new_valid_from`.
pub fn supersede(
    old: &mut Value,
    new_id: &str,
    policy_change: bool,
    new_valid_from: Option<&str>,
) {
    old["status"] = serde_json::json!("superseded");
    old["superseded_by"] = serde_json::json!(new_id);
    if policy_change {
        if let Some(vf) = new_valid_from {
            if let Some(day) = date_minus_one(vf) {
                old["valid_to"] = serde_json::json!(day);
            }
        }
    }
}

/// `datetime.date.fromisoformat(d) - 1 day`; `None` on unparseable input
/// (Python would raise — callers gate on `new_valid_from` presence).
fn date_minus_one(d: &str) -> Option<String> {
    let y: i64 = d.get(0..4)?.parse().ok()?;
    let mo: i64 = d.get(5..7)?.parse().ok()?;
    let day: i64 = d.get(8..10)?.parse().ok()?;
    if d.len() != 10 || d.as_bytes()[4] != b'-' || d.as_bytes()[7] != b'-' {
        return None;
    }
    let days = days_from_civil(y, mo, day) - 1;
    let (y, mo, day) = civil_from_days_pub(days);
    Some(format!("{y:04}-{mo:02}-{day:02}"))
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y_adj = if m <= 2 { y - 1 } else { y };
    let era = y_adj.div_euclid(400);
    let yoe = y_adj - era * 400;
    let mp = (m + 9).rem_euclid(12);
    let doy = (153 * mp + 2) / 5 + d - 1;
    era * 146_097 + (yoe * 365 + yoe / 4 - yoe / 100 + doy) - 719_468
}

fn civil_from_days_pub(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `compile._append_evidence` (compile.py:373) — append cand's evidence
/// refs not already on target (dedup by canonical JSON key). Returns
/// whether anything was added.
pub fn append_evidence(target: &mut Value, cand: &Value) -> bool {
    let existing: std::collections::HashSet<String> = target
        .get("evidence")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|ev| crate::util::canonical_json(ev))
                .collect()
        })
        .unwrap_or_default();
    let mut added = false;
    if let Some(evs) = cand.get("evidence").and_then(Value::as_array) {
        for ev in evs {
            let key = crate::util::canonical_json(ev);
            if !existing.contains(&key) {
                if !target["evidence"].is_array() {
                    target["evidence"] = serde_json::json!([]);
                }
                target["evidence"].as_array_mut().unwrap().push(ev.clone());
                added = true;
            }
        }
    }
    added
}

/// `claims.independence` (claims.py:357) — distinct independent evidence
/// origins (NOT reference count). Fails closed: an evidence ref whose
/// manifest is missing/corrupt is excluded rather than counted as its
/// own origin.
pub fn independence(wiki: &Wiki, claim: &Value) -> Result<usize, EngineError> {
    let mut origins: std::collections::HashSet<String> = std::collections::HashSet::new();
    for ev in claim
        .get("evidence")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        if crate::frontmatter::truthy(ev.get("root_origin")) {
            origins.insert(crate::util::canonical_json(ev.get("root_origin").unwrap()));
            continue;
        }
        let Some(sid) = ev.get("source_id").and_then(Value::as_str) else {
            return Err(EngineError::Txn(format!(
                "evidence ref missing source_id: {}",
                crate::schema::py_repr(&ev)
            )));
        };
        let Ok(manifest) = wiki.load_json(&format!("sources/{sid}/manifest.json")) else {
            continue; // unreadable manifest: unverifiable, not an origin
        };
        let origin = manifest
            .get("root_origin")
            .and_then(Value::as_str)
            .unwrap_or(sid)
            .to_string();
        origins.insert(origin);
    }
    Ok(origins.len())
}
