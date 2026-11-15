//! Compile pipeline: pending evidence -> candidate claims -> reconciliation
//! -> accepted knowledge -> dependency invalidation (PRD §30, §16; port of
//! wikicore/compile.py).
//!
//! Deterministic parts (tiered subject matching, auto-verdicts, policy
//! application, supersession, persistence) live here; the agent classifies
//! relationships between command calls.
//!
//! Rust deltas over compile.py (spec §5.2/§6.4):
//! - `reconcile_prepare` bounds `matches[]` to top-8 (tier rank, then
//!   claim id) and adds `truncated_matches` per comparison.
//! - `reconcile_prepare` persists matched-claim stamps to `stamps.json`;
//!   `reconcile_apply` verifies them and registers expect-stamps so the
//!   prepare→apply window is conflict-checked cross-process.
//! - `reconcile_apply --use-prefill` consumes `auto_prefill` blocks
//!   (confidence ≥ 0.7) as classification fills — never silently.


use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde_json::{json, Map, Value};

use crate::err::EngineError;
use crate::evidence::sources;
use crate::schema;
use crate::shorthand::{normalize_candidate, parse_candidates_file};
use crate::store::Wiki;
use crate::topics;
use crate::txn::stamps::Stamp;
use crate::txn::{Txn, TxnOpts};
use crate::{claims, decisions, deps, ids, review, util};

const CLAIMS_INDEX_REL: &str = ".state/claims-index.json";
const MATCHES_CAP: usize = 8;

const RELATIONSHIPS: &[&str] = &[
    "CORRECTION",
    "POLICY_CHANGE",
    "SCOPE_DIFFERENCE",
    "CONTRADICTION",
    "CORROBORATION",
    "DUPLICATE",
    "UNRELATED",
];

/// Relationships that mutate an existing claim and therefore require
/// target_claim_id (compile.py:_TARGETED).
const TARGETED: &[&str] = &[
    "CORRECTION",
    "POLICY_CHANGE",
    "CONTRADICTION",
    "CORROBORATION",
    "DUPLICATE",
];

const TERMINAL: &[&str] = &["superseded", "rejected"];

fn now() -> String {
    util::now_iso()
}

/// Python `json.dumps(v, ensure_ascii=False)` — default separators for
/// run-dir byte parity (candidates.jsonl, pending.json).
fn canonical(v: &Value) -> String {
    util::py_json_dumps(v)
}
/// Python `str(v)` for scalar coercion (`str(c["subject"]).strip()` parity).
fn py_str(v: &Value) -> String {
    schema::py_str(v)
}

fn py_repr(v: &Value) -> String {
    schema::py_repr(v)
}

/// Python `type(x)` name for error strings.
fn py_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python numeric-equal `==` for `same_value`/dedup keys (5 == 5.0).
fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => x.as_f64() == y.as_f64(),
        _ => a == b,
    }
}

fn write_json_indent(path: &Path, v: &Value) -> Result<(), EngineError> {
    // json.dump(indent=2, ensure_ascii=False) — no trailing newline.
    util::atomic_write(path, serde_json::to_string_pretty(v)?.as_bytes())?;
    Ok(())
}

fn write_json_compact(path: &Path, v: &Value) -> Result<(), EngineError> {
    // json.dump() — default separators (", ", ": ").
    util::atomic_write(path, util::py_json_dumps(v).as_bytes())?;
    Ok(())
}

fn load_json_path(path: &Path) -> Result<Value, EngineError> {
    let text = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}

fn load_json_opt_path(path: &Path) -> Result<Option<Value>, EngineError> {
    match std::fs::read_to_string(path) {
        Ok(text) => Ok(Some(serde_json::from_str(&text)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

// ---------------------------------------------------------------------------
// compile_plan / run dir
// ---------------------------------------------------------------------------

/// `compile_plan` (compile.py:28-31) — `{"pending": [...]}` of uncompiled
/// source versions.
pub fn compile_plan(wiki: &Wiki) -> Result<Value, EngineError> {
    Ok(json!({"pending": sources::pending_versions(wiki)?}))
}

/// `_run_dir` (compile.py:34-43) — run_id becomes a path segment; refuse
/// anything that is not a real `run_<ulid>` before joining under
/// `.state/staging`.
pub fn run_dir(wiki: &Wiki, run_id: &str) -> Result<PathBuf, EngineError> {
    if !(run_id.starts_with("run_") && ids::validate(run_id)) {
        return Err(EngineError::Txn(format!("invalid run id: {run_id:?}")));
    }
    let path = wiki.p(&format!(".state/staging/{run_id}"));
    if !path.is_dir() {
        return Err(EngineError::Txn(format!("unknown compile run: {run_id}")));
    }
    Ok(path)
}

/// `compute_corpus_fingerprint` (compile.py:671-693) — SHA-256 hex[:16]
/// over the canonical-JSON of the sorted `(relpath, sha256)` pairs of
/// every file under `raw/` and `claims/` plus `.state/manifest.json`.
/// Cross-engine contract: identical bytes to the Python engine's hash.
pub fn compute_corpus_fingerprint(wiki: &Wiki) -> Result<String, EngineError> {
    let mut entries: Vec<(String, String)> = Vec::new();
    for top in ["raw", "claims"] {
        let base = wiki.p(top);
        for path in util::collect_files(&base)? {
            // vanished/unreadable mid-scan (TOCTOU) — Python skips OSError
            let bytes = match std::fs::read(&path) {
                Ok(b) => b,
                Err(_) => continue,
            };
            let rel = path
                .strip_prefix(&wiki.dot)
                .map(|r| r.to_string_lossy().replace('\\', "/"))
                .unwrap_or_else(|_| path.to_string_lossy().into_owned());
            entries.push((rel, util::sha256_hex(&bytes)));
        }
    }
    let manifest_p = wiki.p(".state/manifest.json");
    if let Ok(bytes) = std::fs::read(&manifest_p) {
        entries.push((".state/manifest.json".to_string(), util::sha256_hex(&bytes)));
    }
    entries.sort();
    // hashing.canonical_json — sorted keys, tight separators (contractual
    // cross-engine hash input).
    let canon = util::canonical_json(&json!(entries));
    Ok(util::sha256_hex(canon.as_bytes())[..16].to_string())
}

// ---------------------------------------------------------------------------
// stage_candidates
// ---------------------------------------------------------------------------

/// `_stage_payload` (compile.py:51-75) — assign ids + root_origin, write
/// compile.json into rundir.
fn stage_payload(
    wiki: &Wiki,
    rundir: &Path,
    source_id: &str,
    source_version: i64,
    candidates: &mut Vec<Value>,
    report: &Value,
) -> Result<(), EngineError> {
    let manifest = sources::get_manifest(wiki, source_id)?
        .ok_or_else(|| EngineError::Txn(format!("unknown source: {source_id}")))?;
    for c in candidates.iter_mut() {
        let id_ok = c["id"].as_str().map_or(false, ids::validate)
            && c["id"].as_str() != Some("auto");
        if !id_ok {
            c["id"] = json!(ids::new("claim"));
        }
        if !truthy(&c["root_origin"]) {
            let ro = manifest["root_origin"].as_str().unwrap_or(source_id);
            c["root_origin"] = json!(ro);
        }
        let subj = py_str(&c["subject"]);
        c["subject"] = json!(subj.trim().to_string());
        let pred = py_str(&c["predicate"]);
        c["predicate"] = json!(pred.trim().to_string());
    }
    let payload = json!({
        "source_id": source_id,
        "source_version": source_version,
        "candidates": candidates,
        "report": report,
        "corpus_fingerprint": compute_corpus_fingerprint(wiki)?,
    });
    write_json_indent(&rundir.join("compile.json"), &payload)
}

/// `stage_candidates` (compile.py:78-129) — stage agent-extracted
/// candidates into a run dir; returns the run id. `run_id: None` mints a
/// fresh dir; `Some(R)` reuses an existing pending run dir.
pub fn stage_candidates(
    wiki: &Wiki,
    source_id: &str,
    source_version: i64,
    candidates: Vec<Value>,
    report: &Value,
    run_id: Option<&str>,
) -> Result<String, EngineError> {
    let manifest = sources::get_manifest(wiki, source_id)?
        .ok_or_else(|| EngineError::Txn(format!("unknown source: {source_id}")))?;
    // normalize regardless of input format: .json files bypass the CLI's
    // shorthand path, so field completion must happen here (compile.py:92-96).
    let root_origin = manifest["root_origin"]
        .as_str()
        .unwrap_or(source_id)
        .to_string();
    let mut normalized: Vec<Value> = candidates
        .iter()
        .map(|c| normalize_candidate(c, source_id, source_version, &root_origin))
        .collect::<Result<_, _>>()?;
    let rundir = match run_id {
        Some(rid) => {
            let d = run_dir(wiki, rid)?;
            if d.join("receipt.json").is_file() {
                return Err(EngineError::Txn(format!("run {rid} already applied")));
            }
            // re-staging into a prepared run: comparisons/classifications
            // are keyed to the OLD candidate set — drop both (SP3-F5 T3-S1).
            for stale in ["comparisons.json", "classifications.json"] {
                let p = d.join(stale);
                if p.is_file() {
                    std::fs::remove_file(&p)?;
                }
            }
            d
        }
        None => {
            // Transaction(wiki, "wiki-stage-candidates").staging_dir —
            // minted, parked, never committed.
            let txn = Txn::begin_with(
                wiki,
                &ids::new_run(),
                TxnOpts { command: Some("wiki-stage-candidates".to_string()), ..Default::default() },
            )?;
            txn.staging_dir.clone()
        }
    };
    stage_payload(wiki, &rundir, source_id, source_version, &mut normalized, report)?;
    // mirror the shorthand file a pending run/agent would have written —
    // pending.json's candidates_path keys off <run>/candidates.jsonl.
    let pending_p = rundir.join("pending.json");
    let mut cand_path = rundir.join("candidates.jsonl");
    if pending_p.is_file() {
        if let Some(cp) = load_json_path(&pending_p)?["candidates_path"].as_str() {
            cand_path = PathBuf::from(cp);
        }
    }
    {
        let mut body = String::new();
        for c in &normalized {
            body.push_str(&canonical(c));
            body.push('\n');
        }
        util::atomic_write(&cand_path, body.as_bytes())?;
    }
    Ok(rundir
        .file_name()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default())
}

// ---------------------------------------------------------------------------
// subject-key tiers (compile.py:132-167)
// ---------------------------------------------------------------------------

/// `_plural_fold` (compile.py:141-150).
fn plural_fold(seg: &str) -> &str {
    for ending in ["sses", "ches", "shes", "xes", "zes"] {
        if seg.ends_with(ending) && seg.len() - 2 >= 3 {
            return &seg[..seg.len() - 2];
        }
    }
    if seg.ends_with('s') && !seg.ends_with("ss") && seg.len() - 1 >= 3 {
        return &seg[..seg.len() - 1];
    }
    seg
}

/// `_tier_keys` (compile.py:153-158) — (t1, t2, t3) normalized subject keys.
fn tier_keys(subject: &Value) -> (String, String, String) {
    let t1 = py_str(subject).trim().to_lowercase();
    let t2: String = t1
        .chars()
        .filter(|c| !matches!(c, '.' | '_' | '/' | '-' | ' '))
        .collect();
    let t3: String = t1
        .split(|c| matches!(c, '.' | '_' | '/' | '-' | ' '))
        .map(plural_fold)
        .collect();
    (t1, t2, t3)
}

/// `claims.norm_key` (claims.py:33-35).
fn norm_key(subject: &Value, predicate: &Value) -> String {
    format!(
        "{}/{}",
        py_str(subject).trim().to_lowercase(),
        py_str(predicate).trim().to_lowercase()
    )
}

/// `_bucket_by_key` (compile.py:161-167) — key -> claims in scan order.
fn bucket_by_key(existing: Vec<Value>) -> BTreeMap<String, Vec<Value>> {
    let mut buckets: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for ex in existing {
        buckets
            .entry(norm_key(&ex["subject"], &ex["predicate"]))
            .or_default()
            .push(ex);
    }
    buckets
}

/// `claims.load_index` (claims.py:152-160) — (index, fresh); fresh iff the
/// watermark equals the current revision AND complete is true.
fn load_claims_index(wiki: &Wiki) -> Result<(Option<Value>, bool), EngineError> {
    let idx = match wiki.load_json_opt(CLAIMS_INDEX_REL) {
        Ok(v) => v,
        Err(_) => None, // corrupt/unreadable → (None, False)
    };
    let Some(idx) = idx else {
        return Ok((None, false));
    };
    if !(idx.is_object() && idx["claims"].is_object() && idx["decisions"].is_array()) {
        return Ok((None, false));
    }
    let fresh = idx["revision"].as_i64() == Some(wiki.revision()?)
        && idx["complete"].as_bool() == Some(true);
    Ok((Some(idx), fresh))
}

/// `_bucketed_existing` (compile.py:170-203) — fresh index with verified
/// membership; else one canonical scan. Index membership drift in either
/// direction falls back wholesale, never wrong.
fn bucketed_existing(wiki: &Wiki) -> Result<BTreeMap<String, Vec<Value>>, EngineError> {
    let (idx, fresh) = load_claims_index(wiki)?;
    if fresh {
        if let Some(idx) = idx {
            let on_disk: BTreeSet<String> = {
                let cdir = wiki.p("claims");
                let mut set = BTreeSet::new();
                if cdir.is_dir() {
                    for e in std::fs::read_dir(&cdir)? {
                        let name = e?.file_name().to_string_lossy().into_owned();
                        if name.starts_with("claim_") && name.ends_with(".json") {
                            set.insert(name[..name.len() - 5].to_string());
                        }
                    }
                }
                set
            };
            let indexed: BTreeSet<String> = idx["claims"]
                .as_object()
                .map(|o| o.keys().cloned().collect())
                .unwrap_or_default();
            if on_disk == indexed {
                let mut ids_sorted: Vec<String> = indexed.into_iter().collect();
                ids_sorted.sort();
                let mut existing: Vec<Value> = Vec::new();
                let mut ok = true;
                for cid in &ids_sorted {
                    match claims::load_claim(wiki, cid) {
                        Ok(c) => existing.push(c),
                        Err(_) => {
                            ok = false;
                            break;
                        }
                    }
                }
                if ok {
                    return Ok(bucket_by_key(
                        existing
                            .into_iter()
                            .filter(|c| {
                                !TERMINAL.contains(&c["status"].as_str().unwrap_or(""))
                            })
                            .collect(),
                    ));
                }
            }
        }
    }
    Ok(bucket_by_key(
        claims::list(wiki)?
            .into_iter()
            .filter(|c| !TERMINAL.contains(&c["status"].as_str().unwrap_or("")))
            .collect(),
    ))
}

// ---------------------------------------------------------------------------
// match evaluation
// ---------------------------------------------------------------------------

/// `_scopes_equal` (compile.py:206-207) — str()-coerced k/v map equality.
fn scopes_equal(a: &Value, b: &Value) -> bool {
    let norm = |v: &Value| -> BTreeMap<String, String> {
        v.as_object()
            .map(|o| {
                o.iter()
                    .map(|(k, val)| (k.clone(), py_str(val)))
                    .collect()
            })
            .unwrap_or_default()
    };
    norm(a) == norm(b)
}

/// `_temporal` (compile.py:210-213).
fn temporal(old: &Value, new: &Value) -> &'static str {
    match (old["valid_to"].as_str(), new["valid_from"].as_str()) {
        (Some(vt), Some(vf)) if vt < vf => "disjoint",
        _ => "overlapping",
    }
}

/// `_root_origins_of` (compile.py:216-220).
fn root_origins_of(claim: &Value) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for ev in claim["evidence"].as_array().cloned().unwrap_or_default() {
        let ro = ev["root_origin"].clone();
        let v = if truthy(&ro) { ro } else { ev["source_id"].clone() };
        out.insert(py_str(&v));
    }
    out
}

/// `auto_verdict` (compile.py:223-244) — deterministic reconciliation
/// verdicts; None = needs agent.
pub fn auto_verdict(cand: &Value, matches: &[Value]) -> Option<String> {
    if matches.is_empty() {
        return Some("UNRELATED".to_string());
    }
    if matches.iter().all(|m| truthy(&m["tier"])) {
        // every match is t2/t3-only — never auto-merge
        return Some("rejected_tier_match".to_string());
    }
    if matches.len() > 1 {
        return None;
    }
    let m = &matches[0];
    if m["same_scope"].as_bool() == Some(true) && m["same_value"].as_bool() == Some(true) {
        if m["temporal"].as_str() == Some("disjoint") {
            return None; // non-overlapping windows — route to the agent
        }
        let my_origin = &cand["root_origin"];
        if truthy(my_origin)
            && m["root_origins"]
                .as_array()
                .map_or(false, |ros| ros.iter().any(|r| r == my_origin))
        {
            return Some("DUPLICATE".to_string());
        }
        return Some("CORROBORATION".to_string());
    }
    None
}

/// `_hint` (compile.py:247-257).
fn hint(matches: &[Value]) -> &'static str {
    if matches.is_empty() {
        return "UNRELATED";
    }
    if matches.len() > 1 {
        return "CONTRADICTION?";
    }
    let m = &matches[0];
    if m["same_scope"].as_bool() == Some(true) && m["same_value"].as_bool() == Some(true) {
        return "DUPLICATE/CORROBORATION";
    }
    if m["same_scope"].as_bool() != Some(true) {
        return "SCOPE_DIFFERENCE?";
    }
    "CONTRADICTION?"
}

fn match_entry(ex: &Value, cand: &Value, tier: Option<i64>) -> Value {
    let mut m = json!({
        "claim_id": ex["id"],
        "same_scope": scopes_equal(&ex["scope"], &cand["scope"]),
        "same_value": json_eq(&ex["value"], &cand["value"]),
        "temporal": temporal(ex, cand),
        "status": ex["status"],
        "current_value": ex["value"],
        // snapshot for stale-apply detection at reconcile_apply
        "version": ex["version"],
        "root_origins": root_origins_of(ex).into_iter().collect::<Vec<_>>(),
    });
    if let Some(t) = tier {
        m["tier"] = json!(t);
        m["subject"] = ex["subject"].clone();
    }
    m
}

fn match_rank(m: &Value) -> i64 {
    m["tier"].as_i64().unwrap_or(1)
}

/// `reconcile_prepare` (compile.py:260-321) — bounded per spec §6.4:
/// `matches[]` top-8 by (tier rank, claim id), `truncated_matches` the
/// count beyond the bound; plus §5.2 `stamps.json` persistence.
pub fn reconcile_prepare(wiki: &Wiki, run_id: &str) -> Result<Value, EngineError> {
    let dir = run_dir(wiki, run_id)?;
    let payload = load_json_path(&dir.join("compile.json"))?;
    let buckets = bucketed_existing(wiki)?;
    // t2/t3 lookup maps, built lazily on the first exact-key miss.
    let mut tier_maps: Option<HashMap<i64, BTreeMap<String, Vec<Value>>>> = None;

    let mut comparisons: Vec<Value> = Vec::new();
    let mut stamps_out: Map<String, Value> = Map::new();
    for (i, cand) in payload["candidates"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let key = norm_key(&cand["subject"], &cand["predicate"]);
        let mut matches: Vec<Value> = buckets
            .get(&key)
            .map(|exs| exs.iter().map(|ex| match_entry(ex, cand, None)).collect())
            .unwrap_or_default();
        if matches.is_empty() {
            // t2/t3 fold the SUBJECT only; the predicate part of the map
            // key stays exact-lower (compile.py:289-298).
            if tier_maps.is_none() {
                let mut t2: BTreeMap<String, Vec<Value>> = BTreeMap::new();
                let mut t3: BTreeMap<String, Vec<Value>> = BTreeMap::new();
                for cl in buckets.values().flatten() {
                    let et = tier_keys(&cl["subject"]);
                    let pl = py_str(&cl["predicate"]).trim().to_lowercase();
                    t2.entry(format!("{}/{}", et.1, pl))
                        .or_default()
                        .push(cl.clone());
                    t3.entry(format!("{}/{}", et.2, pl))
                        .or_default()
                        .push(cl.clone());
                }
                let mut maps = HashMap::new();
                maps.insert(2i64, t2);
                maps.insert(3i64, t3);
                tier_maps = Some(maps);
            }
            let ct = tier_keys(&cand["subject"]);
            let cpred = py_str(&cand["predicate"]).trim().to_lowercase();
            let maps = tier_maps.as_ref().unwrap();
            for tier in [2i64, 3] {
                let folded = if tier == 2 { &ct.1 } else { &ct.2 };
                if let Some(exs) = maps[&tier].get(&format!("{folded}/{cpred}")) {
                    for ex in exs {
                        matches.push(match_entry(ex, cand, Some(tier)));
                    }
                }
                if !matches.is_empty() {
                    break;
                }
            }
        }
        // §6.4 bound: (tier rank, claim id) total order, keep top-8.
        matches.sort_by(|a, b| {
            (match_rank(a), a["claim_id"].as_str().unwrap_or(""))
                .cmp(&(match_rank(b), b["claim_id"].as_str().unwrap_or("")))
        });
        let truncated = matches.len().saturating_sub(MATCHES_CAP);
        matches.truncate(MATCHES_CAP);
        // §5.2: persist matched-claim stamps for cross-process verify.
        for m in &matches {
            if let Some(cid) = m["claim_id"].as_str() {
                let rel = claims::claim_rel(cid);
                if !stamps_out.contains_key(&rel) {
                    if let Some(st) = Stamp::capture_in(&wiki.p(&rel), &wiki.root)? {
                        stamps_out.insert(rel, serde_json::to_value(st)?);
                    }
                }
            }
        }
        comparisons.push(json!({
            "index": i,
            "key": key,
            "candidate_value": cand["value"],
            "scope": if cand["scope"].is_object() { cand["scope"].clone() } else { json!({}) },
            "authority": cand["authority"],
            "valid_from": cand["valid_from"],
            "matches": matches,
            "truncated_matches": truncated,
        }));
        // auto/hint derive from the bounded matches
        let cmp = comparisons.last_mut().unwrap();
        let ms: Vec<Value> = cmp["matches"].as_array().cloned().unwrap_or_default();
        let auto = auto_verdict(cand, &ms);
        cmp["auto"] = auto.map(Value::from).unwrap_or(Value::Null);
        cmp["hint"] = json!(hint(&ms));
        // §3/§6.4 auto_prefill: with the Jev env configured, one advisory
        // choice verdict rides on each comparison. ask() never raises —
        // failures leave the block absent; only a ≥0.7-confidence verdict
        // is attached (consume opt-in via reconcile-apply --use-prefill).
        if crate::jev::is_ready() {
            let top: Vec<String> = ms
                .iter()
                .take(3)
                .map(|m| {
                    format!(
                        "{}={} (same_value:{}, temporal:{}, status:{})",
                        m["claim_id"].as_str().unwrap_or(""),
                        py_str(&m["current_value"]),
                        m["same_value"].as_bool().unwrap_or(false),
                        m["temporal"].as_str().unwrap_or(""),
                        m["status"].as_str().unwrap_or(""),
                    )
                })
                .collect();
            let state = format!(
                "candidate: {} {} {}; top matches: {}",
                py_str(&cand["subject"]),
                py_str(&cand["predicate"]),
                py_str(&cand["value"]),
                if top.is_empty() { "none".to_string() } else { top.join("; ") },
            );
            let r = crate::jev::ask_with(
                Some(&wiki.root),
                &state,
                &json!({
                    "verdict": {
                        "type": "choice",
                        "options": RELATIONSHIPS,
                        "instructions": "How does this staged candidate claim relate to the existing claim it most resembles? UNRELATED when no match is the same fact.",
                    }
                }),
            );
            if r.get("ok").and_then(Value::as_bool) == Some(true) {
                let v = &r["answers"]["verdict"]["value"];
                let pick = v["pick"].as_str().unwrap_or("");
                let conf = v["distribution"][pick]
                    .as_f64()
                    .or_else(|| v["confidence"].as_f64())
                    .unwrap_or(0.0);
                if RELATIONSHIPS.contains(&pick) && conf >= 0.7 {
                    cmp["auto_prefill"] = json!({
                        "verdict": pick,
                        "confidence": conf,
                        "jev": true,
                    });
                }
            }
        }
    }
    let result = json!({
        "run_id": run_id,
        "base_revision": wiki.revision()?,
        "comparisons": comparisons,
    });
    write_json_indent(&dir.join("comparisons.json"), &result)?;
    // §5.2 cross-process stamps — tmp+rename, explicit-keys-nullable.
    util::atomic_write(
        &dir.join("stamps.json"),
        serde_json::to_string_pretty(&Value::Object(stamps_out))?.as_bytes(),
    )?;
    Ok(result)
}

// ---------------------------------------------------------------------------
// decisions helpers kept here (claims.py:287-353): the index-aware
// list_decisions and auto_decision_stub used by reconcile_apply. Claim
// persistence itself lives in crate::claims / crate::decisions.
// ---------------------------------------------------------------------------
fn scan_decisions(wiki: &Wiki) -> Result<Vec<Value>, EngineError> {
    let ddir = wiki.p("decisions");
    let mut out = Vec::new();
    if !ddir.is_dir() {
        return Ok(out);
    }
    let mut names: Vec<String> = Vec::new();
    for e in std::fs::read_dir(&ddir)? {
        let name = e?.file_name().to_string_lossy().into_owned();
        if name.starts_with("decision_") && name.ends_with(".json") {
            names.push(name);
        }
    }
    names.sort();
    for name in names {
        out.push(load_json_path(&ddir.join(name))?);
    }
    Ok(out)
}

fn load_decision(wiki: &Wiki, did: &str) -> Result<Value, EngineError> {
    if !(did.starts_with("decision_") && ids::validate(did)) {
        return Err(EngineError::Txn(format!("decision id invalid: {did:?}")));
    }
    let rel = format!("decisions/{did}.json");
    wiki.load_json(&rel)
        .map_err(|_| EngineError::Txn(format!("unknown decision: {did}")))
}

/// `claims.list_decisions` (claims.py:301-315).
fn list_decisions(wiki: &Wiki) -> Result<Vec<Value>, EngineError> {
    let (idx, fresh) = load_claims_index(wiki)?;
    if fresh {
        if let Some(idx) = idx {
            if idx["decisions"].is_array() {
                let disk: Option<BTreeSet<String>> = {
                    let d = wiki.p("decisions");
                    if d.is_dir() {
                        Some(
                            std::fs::read_dir(&d)?
                                .filter_map(|e| e.ok())
                                .map(|e| e.file_name().to_string_lossy().into_owned())
                                .filter(|n| n.starts_with("decision_") && n.ends_with(".json"))
                                .map(|n| n[..n.len() - 5].to_string())
                                .collect(),
                        )
                    } else {
                        None
                    }
                };
                let indexed: BTreeSet<String> = idx["decisions"]
                    .as_array()
                    .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
                    .unwrap_or_default();
                if disk.map_or(false, |d| !d.is_subset(&indexed)) {
                    return scan_decisions(wiki);
                }
                let mut ids_sorted: Vec<String> = indexed.into_iter().collect();
                ids_sorted.sort();
                let mut out = Vec::new();
                for did in &ids_sorted {
                    match load_decision(wiki, did) {
                        Ok(d) => out.push(d),
                        Err(_) => return scan_decisions(wiki),
                    }
                }
                return Ok(out);
            }
        }
    }
    scan_decisions(wiki)
}

/// `claims.auto_decision_stub` (claims.py:333-353) — mint an accepted
/// decision stub for an accepted claim whose authority is
/// explicit_project_decision. Idempotent via `claims[]` membership.
fn auto_decision_stub(wiki: &Wiki, txn: &mut Txn, claim: &Value) -> Result<Option<Value>, EngineError> {
    if claim["authority"]["type"].as_str() != Some("explicit_project_decision") {
        return Ok(None);
    }
    let cid = claim["id"].as_str().unwrap_or("");
    for d in list_decisions(wiki)? {
        if d["claims"]
            .as_array()
            .map_or(false, |a| a.iter().any(|x| x.as_str() == Some(cid)))
        {
            return Ok(None);
        }
    }
    let recorded = claim["recorded_at"].as_str().map(String::from);
    let mut decision = json!({
        "id": ids::new("decision"),
        "title": claim["subject"],
        "body": format!("{} = {}", py_str(&claim["predicate"]), py_str(&claim["value"])),
        "status": "accepted",
        "decided_on": recorded.unwrap_or_else(now).chars().take(10).collect::<String>(),
        "claims": [cid],
        "evidence": claim["evidence"],
        "authority": {"type": "explicit_project_decision"},
        "auto_materialized": true,
    });
    decisions::save_decision(wiki, txn, &mut decision)?;
    Ok(Some(decision))
}


// ---------------------------------------------------------------------------
// reconcile_apply
// ---------------------------------------------------------------------------

/// `_from_candidate` (compile.py:334-360).
fn from_candidate(
    cand: &Value,
    classification: &Value,
    status: &str,
    acceptance_reason: &str,
) -> Result<Value, EngineError> {
    let tid = classification["target_claim_id"].as_str();
    if let Some(tid) = tid {
        if cand["supersedes"]
            .as_array()
            .map_or(false, |a| a.iter().any(|x| x.as_str() == Some(tid)))
        {
            return Err(EngineError::Txn(
                "supersedes double-count: claim target appears in \
                 candidate.supersedes and classification; put it only in the \
                 classification"
                    .to_string(),
            ));
        }
    }
    let mut claim = json!({
        "id": cand["id"],
        "subject": cand["subject"],
        "predicate": cand["predicate"],
        "value": cand["value"],
        "scope": if cand["scope"].is_object() { cand["scope"].clone() } else { json!({}) },
        "valid_from": if truthy(&classification["valid_from"]) { classification["valid_from"].clone() } else { cand["valid_from"].clone() },
        "valid_to": if truthy(&classification["valid_to"]) { classification["valid_to"].clone() } else if truthy(&cand["valid_to"]) { cand["valid_to"].clone() } else { Value::Null },
        "recorded_at": if truthy(&cand["recorded_at"]) { cand["recorded_at"].clone() } else { json!(now()) },
        "status": status,
        "authority": if truthy(&classification["authority"]) { classification["authority"].clone() } else { cand["authority"].clone() },
        "evidence": cand["evidence"].as_array().cloned().unwrap_or_default(),
        "supersedes": cand["supersedes"].as_array().cloned().unwrap_or_default(),
        "root_origin": cand["root_origin"],
        "version": 1,
    });
    if status == "accepted" {
        claim["acceptance"] = json!({"reason": acceptance_reason, "accepted_at": now()});
    }
    Ok(claim)
}

/// `_auto_status` (compile.py:363-370).
fn auto_status(wiki: &Wiki, cand: &Value, classification: &Value) -> Result<(String, String), EngineError> {
    let cfg = wiki.config()?;
    let policy = &cfg["acceptance_policy"];
    let authority = if truthy(&classification["authority"]) {
        classification["authority"].clone()
    } else {
        cand["authority"].clone()
    };
    let atype = authority["type"].as_str().unwrap_or("agent_inference");
    let auto_accept: Vec<String> = policy["auto_accept_authority"]
        .as_array()
        .map(|a| a.iter().filter_map(|x| x.as_str().map(String::from)).collect())
        .unwrap_or_default();
    if auto_accept.iter().any(|t| t == atype) {
        return Ok(("accepted".to_string(), atype.to_string()));
    }
    Ok(("candidate".to_string(), String::new()))
}


/// `_check_classification` (compile.py:388-402) — shape-check one
/// caller-supplied classification; Txn on bad input.
fn check_classification(cls: &Value, n_candidates: usize) -> Result<(), EngineError> {
    if !cls.is_object() {
        return Err(EngineError::Txn(format!(
            "classification must be an object, got {}",
            py_type(cls)
        )));
    }
    let rel = cls["relationship"].as_str();
    if !rel.map_or(false, |r| RELATIONSHIPS.contains(&r)) {
        return Err(EngineError::Txn(format!(
            "bad relationship: {} (valid: {})",
            py_repr(&cls["relationship"]),
            RELATIONSHIPS.join(", ")
        )));
    }
    let idx = &cls["index"];
    let idx_ok = match idx {
        Value::Number(n) if n.is_u64() || n.is_i64() => {
            let i = n.as_i64().unwrap_or(-1);
            i >= 0 && (i as usize) < n_candidates
        }
        _ => false,
    };
    if !idx_ok {
        return Err(EngineError::Txn(format!(
            "classification index out of range: {} (candidates: {n_candidates})",
            py_repr(idx)
        )));
    }
    if TARGETED.contains(&rel.unwrap()) && !truthy(&cls["target_claim_id"]) {
        return Err(EngineError::Txn(format!(
            "relationship {} requires target_claim_id",
            rel.unwrap()
        )));
    }
    Ok(())
}

/// `_assert_target_fresh` (compile.py:405-431) — reject an apply whose
/// matched claim changed since reconcile_prepare (version-snapshot check;
/// stamps.json adds the full tuple on Rust-origin runs).
fn assert_target_fresh(
    target_id: &str,
    target: &Value,
    comparisons: Option<&Vec<Value>>,
) -> Result<(), EngineError> {
    let Some(rows) = comparisons else { return Ok(()) };
    let mut snap: Option<Value> = None;
    'outer: for row in rows {
        for m in row["matches"].as_array().cloned().unwrap_or_default() {
            if m["claim_id"].as_str() == Some(target_id) {
                snap = Some(m);
                break 'outer;
            }
        }
    }
    let Some(snap) = snap else { return Ok(()) };
    let version_stale = !snap["version"].is_null() && target["version"] != snap["version"];
    if version_stale || target["status"] != snap["status"] {
        return Err(EngineError::Conflict(format!(
            "claim {target_id} changed since reconcile_prepare (was {}/{}, now {}/{}); \
             re-run reconcile-prepare and re-classify",
            py_str(&snap["version"]),
            py_str(&snap["status"]),
            py_str(&target["version"]),
            py_str(&target["status"])
        )));
    }
    Ok(())
}

/// Verify `stamps.json` (§5.2): Rust-origin runs re-check every persisted
/// stamp tuple against live lstat; Python-origin runs have no file and get
/// only the version snapshot (honest degradation). Registers each rel as a
/// commit-time expect-stamp so the apply→commit window is covered too.
fn verify_run_stamps(wiki: &Wiki, rundir: &Path, txn: &mut Txn) -> Result<(), EngineError> {
    let path = rundir.join("stamps.json");
    let Some(data) = load_json_opt_path(&path)? else {
        return Ok(());
    };
    let Some(obj) = data.as_object() else {
        return Ok(());
    };
    for (rel, sv) in obj {
        let Ok(stamp) = serde_json::from_value::<Stamp>(sv.clone()) else {
            continue;
        };
        let dst = wiki.p(rel);
        crate::txn::stamps::check_no_symlink_components(&dst, &wiki.root)?;
        if !stamp.matches(&dst)? {
            return Err(EngineError::Conflict(format!(
                "expect_stamp mismatch on {rel} (changed since reconcile-prepare); \
                 re-run reconcile-prepare and re-classify"
            )));
        }
        txn.expect(rel)?;
    }
    Ok(())
}

/// `reconcile_apply` (compile.py:434-643) — validate classifications,
/// auto-fill undecided candidates, apply each relationship inside one
/// `wiki-compile` txn, mark the version compiled, return the receipt.
///
/// Rust adds `use_prefill` (§6.4): `auto_prefill {verdict, confidence}`
/// blocks at ≥0.7 confidence fill the classification set for otherwise-
/// unclassified candidates.
pub fn reconcile_apply(
    wiki: &Wiki,
    run_id: &str,
    classifications: Vec<Value>,
    base_revision: i64,
    topics_rows: Option<Vec<Value>>,
    declared_by: Option<&Value>,
    use_prefill: bool,
) -> Result<Value, EngineError> {
    // copy: auto-filled entries must not mutate the caller's list
    let mut classifications: Vec<Value> = classifications.clone();
    let rundir = run_dir(wiki, run_id)?;
    let payload = load_json_path(&rundir.join("compile.json"))?;
    let candidates: Vec<Value> = payload["candidates"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let source_id = payload["source_id"].as_str().unwrap_or("").to_string();
    let source_version = payload["source_version"].as_i64().unwrap_or(0);

    let comparisons_path = rundir.join("comparisons.json");
    let comparisons: Option<Vec<Value>> = if comparisons_path.is_file() {
        Some(
            load_json_path(&comparisons_path)?["comparisons"]
                .as_array()
                .cloned()
                .unwrap_or_default(),
        )
    } else {
        None
    };
    // validate the caller's classifications before auto-filling
    for cls in &classifications {
        check_classification(cls, candidates.len())?;
    }
    let mut classified: HashSet<usize> = classifications
        .iter()
        .filter_map(|c| c["index"].as_u64().map(|i| i as usize))
        .collect();
    let mut auto_counts = json!({
        "unrelated": 0,
        "duplicate": 0,
        "corroboration": 0,
        "rejected_tier_match": 0,
    });
    // §6.4 --use-prefill: ≥0.7-confidence auto_prefill verdicts fill the
    // classification set — never overwrites an explicit entry.
    let mut prefilled = 0i64;
    if use_prefill {
        if let Some(rows) = &comparisons {
            for row in rows {
                let i = row["index"].as_u64().unwrap_or(usize::MAX as u64) as usize;
                if classified.contains(&i) {
                    continue;
                }
                let pre = &row["auto_prefill"];
                let conf = pre["confidence"].as_f64().unwrap_or(0.0);
                let verdict = pre["verdict"].as_str().unwrap_or("");
                if conf < 0.7 || !RELATIONSHIPS.contains(&verdict) {
                    continue;
                }
                let mut entry = json!({"index": i, "relationship": verdict});
                if TARGETED.contains(&verdict) {
                    let matches = row["matches"].as_array().cloned().unwrap_or_default();
                    match matches.first().and_then(|m| m["claim_id"].as_str()) {
                        Some(cid) => entry["target_claim_id"] = json!(cid),
                        None => continue, // no target — prefill cannot apply
                    }
                }
                classifications.push(entry);
                classified.insert(i);
                prefilled += 1;
            }
        }
    }
    for (i, cand) in candidates.iter().enumerate() {
        if classified.contains(&i) {
            continue;
        }
        let matches: Vec<Value> = comparisons
            .as_ref()
            .and_then(|rows| rows.get(i))
            .and_then(|r| r["matches"].as_array().cloned())
            .unwrap_or_default();
        let verdict = auto_verdict(cand, &matches);
        let Some(verdict) = verdict else {
            return Err(EngineError::Txn(format!(
                "candidate index {i} ({}/{}) needs a classification — \
                 no auto verdict applies",
                py_str(&cand["subject"]),
                py_str(&cand["predicate"])
            )));
        };
        let mut entry = json!({"index": i, "relationship": verdict});
        if verdict == "DUPLICATE" || verdict == "CORROBORATION" {
            // both branches mutate the matched claim — carry its id
            if let Some(cid) = matches.first().and_then(|m| m["claim_id"].as_str()) {
                entry["target_claim_id"] = json!(cid);
            }
        }
        classifications.push(entry);
        let key = verdict.to_lowercase();
        auto_counts[key.as_str()] = json!(auto_counts[key.as_str()].as_i64().unwrap_or(0) + 1);
    }

    let mut txn = Txn::begin_with(
        wiki,
        &ids::new_run(),
        TxnOpts {
            command: Some("wiki-compile".to_string()),
            ..Default::default()
        },
    )?;
    txn.base_revision = base_revision;

    // §5.2 stamps.json — verify persisted match stamps + register expects.
    verify_run_stamps(wiki, &rundir, &mut txn)?;

    let mut topic_out = json!({
        "declared": 0,
        "registered": 0,
        "auto_registered": [],
        "kind_conflicts": [],
        "decision_topics": [],
        "unclaimed": [],
        "warnings": [],
    });
    if let Some(declared_by) = declared_by {
        if let Some(rows) = &topics_rows {
            let out = topics::merge_rows(&mut txn, rows, declared_by)?;
            for k in ["declared", "registered", "kind_conflicts", "warnings"] {
                topic_out[k] = out[k].clone();
            }
        }
        let title = declared_by["title"].as_str().unwrap_or(&source_id);
        let slug = topics::slugify(&py_str(&json!(title)));
        let subject = format!("source.{}", if slug.is_empty() { "source" } else { &slug });
        topics::ensure_source_topic(&mut txn, &subject, title, declared_by)?;
        let mut decision_topics: Vec<String> = topics_rows
            .iter()
            .flatten()
            .filter(|r| topics::norm_kind(r["kind"].as_str().unwrap_or("")) == "decision")
            .filter_map(|r| {
                let s = py_str(&r["subject"]).trim().to_string();
                if s.is_empty() { None } else { Some(s) }
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        decision_topics.sort();
        topic_out["decision_topics"] = json!(decision_topics);
    }
    let mut changes = json!({
        "claims_created": 0,
        "claims_updated": 0,
        "claims_superseded": 0,
        "review_items": 0,
        "decisions_materialized": 0,
    });
    let kind_conflicts_n = topic_out["kind_conflicts"].as_array().map_or(0, |a| a.len()) as i64;
    changes["review_items"] = json!(changes["review_items"].as_i64().unwrap_or(0) + kind_conflicts_n);
    let mut conflicts: Vec<Value> = Vec::new();
    let bump = |k: &str, changes: &mut Value, n: i64| {
        changes[k] = json!(changes[k].as_i64().unwrap_or(0) + n);
    };
    for cls in &classifications {
        let rel = cls["relationship"].as_str().unwrap_or("").to_string();
        let idx = cls["index"].as_u64().unwrap_or(usize::MAX as u64) as usize;
        let cand = candidates
            .get(idx)
            .cloned()
            .ok_or_else(|| EngineError::Txn(format!("candidate index {idx} out of range")))?;
        let mut target: Option<Value> = None;
        if truthy(&cls["target_claim_id"]) {
            let tid = cls["target_claim_id"].as_str().unwrap_or("");
            let t = claims::load_claim(wiki, tid)?;
            assert_target_fresh(tid, &t, comparisons.as_ref())?;
            target = Some(t);
        }

        if rel == "rejected_tier_match" {
            // t2/t3-only match — NEVER auto-merges: review queue only.
            let matches = comparisons
                .as_ref()
                .and_then(|rows| rows.get(idx))
                .and_then(|r| r["matches"].as_array().cloned())
                .unwrap_or_default();
            let tm = matches.iter().find(|m| truthy(&m["tier"]));
            let rid = if let Some(tm) = tm {
                let ex_subject = if truthy(&tm["subject"]) {
                    py_str(&tm["subject"])
                } else {
                    tm["claim_id"].as_str().unwrap_or("").to_string()
                };
                review::add_item(
                    &mut txn,
                    "needs_review",
                    &format!(
                        "tier-{} normalization match: {} ~ {} — agent adjudication required",
                        py_str(&tm["tier"]),
                        py_repr(&cand["subject"]),
                        py_repr(&json!(ex_subject)),
                    ),
                    vec![
                        cand["id"].clone(),
                        tm["claim_id"].clone(),
                    ],
                    vec![tm["claim_id"].clone()],
                    "medium",
                    "confirm same claim (merge evidence) or distinct subject (re-classify)",
                )?
            } else {
                review::add_item(
                    &mut txn,
                    "needs_review",
                    &format!(
                        "tier normalization match: {} — agent adjudication required",
                        py_repr(&cand["subject"])
                    ),
                    vec![cand["id"].clone()],
                    vec![cand["id"].clone()],
                    "medium",
                    "re-run reconcile-prepare, then adjudicate",
                )?
            };
            bump("review_items", &mut changes, 1);
            conflicts.push(json!(rid));
        } else if rel == "UNRELATED" || rel == "SCOPE_DIFFERENCE" {
            let (status, reason) = auto_status(wiki, &cand, cls)?;
            let mut claim = from_candidate(&cand, cls, &status, &reason)?;
            claims::save_claim(wiki, &mut txn, &mut claim)?;
            if auto_decision_stub(wiki, &mut txn, &claim)?.is_some() {
                bump("decisions_materialized", &mut changes, 1);
            }
            bump("claims_created", &mut changes, 1);
            let cid = cand["id"].as_str().unwrap_or("").to_string();
            deps::invalidate(wiki, &mut txn, &[cid.as_str()])?;
        } else if rel == "DUPLICATE" {
            let mut t = target.clone().unwrap_or_else(|| json!({}));
            if claims::append_evidence(&mut t, &cand) {
                claims::save_claim(wiki, &mut txn, &mut t)?;
                bump("claims_updated", &mut changes, 1);
                let tid = t["id"].as_str().unwrap_or("").to_string();
                deps::invalidate(wiki, &mut txn, &[tid.as_str()])?;
            }
            let mut origins: HashSet<String> = HashSet::new();
            for ev in t["evidence"].as_array().cloned().unwrap_or_default() {
                let ro = ev["root_origin"].clone();
                let v = if truthy(&ro) { ro } else { ev["source_id"].clone() };
                origins.insert(py_str(&v));
            }
            if origins.len() > 1 {
                let tid = t["id"].as_str().unwrap_or("").to_string();
                let rid = review::add_item(
                    &mut txn,
                    "duplicate_identity",
                    &format!("claim {tid} now has evidence from multiple origins"),
                    vec![json!(tid.clone())],
                    vec![json!(tid.clone())],
                    "low",
                    "confirm these are copies, not independent sources",
                )?;
                bump("review_items", &mut changes, 1);
                conflicts.push(json!(rid));
            }
        } else if rel == "CORROBORATION" {
            let mut t = target.clone().unwrap_or_else(|| json!({}));
            claims::append_evidence(&mut t, &cand);
            let indep = claims::independence(wiki, &t)?;
            let cfg = wiki.config()?;
            let threshold = cfg["acceptance_policy"]["corroboration_auto_accept"]
                .as_i64()
                .unwrap_or(2);
            let status = t["status"].as_str().unwrap_or("");
            if (status == "candidate" || status == "provisional")
                && indep as i64 >= threshold
            {
                t["status"] = json!("accepted");
                t["acceptance"] = json!({
                    "reason": "independent_corroboration",
                    "accepted_at": now(),
                });
            }
            claims::save_claim(wiki, &mut txn, &mut t)?;
            bump("claims_updated", &mut changes, 1);
            let tid = t["id"].as_str().unwrap_or("").to_string();
            deps::invalidate(wiki, &mut txn, &[tid.as_str()])?;
        } else if rel == "CORRECTION" || rel == "POLICY_CHANGE" {
            let (status, reason) = auto_status(wiki, &cand, cls)?;
            let mut new_claim = from_candidate(&cand, cls, &status, &reason)?;
            let tid = target.as_ref().map(|t| t["id"].as_str().unwrap_or("").to_string()).unwrap_or_default();
            new_claim["supersedes"]
                .as_array_mut()
                .map(|a| a.push(json!(tid.clone())));
            let new_id = new_claim["id"].as_str().unwrap_or("").to_string();
            claims::save_claim(wiki, &mut txn, &mut new_claim)?;
            if auto_decision_stub(wiki, &mut txn, &new_claim)?.is_some() {
                bump("decisions_materialized", &mut changes, 1);
            }
            let mut old = target.clone().unwrap_or_else(|| json!({}));
            claims::supersede(
                &mut old,
                &new_id,
                rel == "POLICY_CHANGE",
                cls["valid_from"].as_str(),
            );
            claims::save_claim(wiki, &mut txn, &mut old)?;
            bump("claims_created", &mut changes, 1);
            bump("claims_superseded", &mut changes, 1);
            deps::invalidate(wiki, &mut txn, &[tid.as_str(), new_id.as_str()])?;
        } else if rel == "CONTRADICTION" {
            let mut new_claim = from_candidate(&cand, cls, "disputed", "")?;
            claims::save_claim(wiki, &mut txn, &mut new_claim)?;
            bump("claims_created", &mut changes, 1);
            let tid = target.as_ref().map(|t| t["id"].as_str().unwrap_or("").to_string()).unwrap_or_default();
            let new_id = new_claim["id"].as_str().unwrap_or("").to_string();
            let rid = review::add_item(
                &mut txn,
                "possible_contradiction",
                &format!(
                    "{}/{} conflicts with existing claim {tid}",
                    py_str(&cand["subject"]),
                    py_str(&cand["predicate"])
                ),
                vec![json!(tid.clone()), json!(new_id.clone())],
                vec![json!(tid.clone()), json!(new_id.clone())],
                "high",
                "decide which value is correct or whether scopes differ",
            )?;
            bump("review_items", &mut changes, 1);
            conflicts.push(json!(rid));
            deps::invalidate(wiki, &mut txn, &[new_id.as_str()])?;
        }
    }

    if let Some(declared_by) = declared_by {
        let declared_subjects: HashSet<String> = topics_rows
            .iter()
            .flatten()
            .map(|r| py_str(&r["subject"]).trim().to_string())
            .collect();
        let mut undeclared: BTreeSet<String> = candidates
            .iter()
            .map(|c| py_str(&c["subject"]).trim().to_string())
            .collect::<BTreeSet<_>>()
            .difference(&declared_subjects.iter().cloned().collect())
            .cloned()
            .collect();
        // read the staged registry (overlay wins) for membership
        // (compile.py:606 reads txn._state_overlay["topics.json"] directly).
        let reg_topics: Map<String, Value> = match txn.staged_value(topics::TOPICS_FILE) {
            Some(v) => v["topics"].as_object().cloned().unwrap_or_default(),
            None => topics::load(wiki)?.topics,
        };
        undeclared.retain(|s| !reg_topics.contains_key(s));
        if !undeclared.is_empty() {
            let subs: Vec<String> = undeclared.iter().cloned().collect();
            let out = topics::auto_register(&mut txn, &subs, declared_by)?;
            if let Some(arr) = out["auto_registered"].as_array() {
                topic_out["auto_registered"] = json!(arr.clone());
            }
            for s in &subs {
                topic_out["warnings"]
                    .as_array_mut()
                    .map(|w| w.push(json!(format!(
                        "subject {} not declared in topics.jsonl; auto-registered as concept",
                        py_repr(&json!(s))
                    ))));
            }
        }
    }

    // persist extraction for this version and mark it compiled
    let extraction = json!({
        "source_version": source_version,
        "extraction_report": payload["report"],
        "candidates": candidates,
        "compiled": true,
    });
    txn.stage_state(
        &format!("sources/{source_id}/extraction_v{source_version}.json"),
        &extraction,
    )?;
    let manifest = wiki.load_json(&format!("sources/{source_id}/manifest.json"))?;
    let latest = manifest["versions"]
        .as_array()
        .and_then(|a| a.last())
        .and_then(|v| v["version"].as_i64());
    if latest == Some(source_version) {
        txn.stage_state(&format!("sources/{source_id}/extraction.json"), &extraction)?;
    }

    let sid = source_id.clone();
    txn.stage_state_with(".state/manifest.json", move |state| {
        if !state["compiled"].is_object() {
            state["compiled"] = json!({});
        }
        let compiled = state["compiled"].as_object_mut().unwrap();
        if !compiled.contains_key(&sid) || !compiled[&sid].is_array() {
            compiled.insert(sid.clone(), json!([]));
        }
        let arr = compiled[&sid].as_array().unwrap();
        if !arr.iter().any(|x| x.as_i64() == Some(source_version)) {
            compiled
                .get_mut(&sid)
                .and_then(Value::as_array_mut)
                .unwrap()
                .push(json!(source_version));
        }
    })?;

    for (k, v) in changes.as_object().unwrap() {
        txn.changes.insert(k.clone(), v.clone());
    }
    txn.conflicts = conflicts;

    let mut receipt = txn.commit()?;
    receipt["topics"] = topic_out;
    if prefilled > 0 {
        auto_counts["prefill"] = json!(prefilled);
    }
    receipt["auto"] = auto_counts;
    // staging compile.json is consumed
    let compile_leftover = rundir.join("compile.json");
    if compile_leftover.is_file() {
        std::fs::remove_file(&compile_leftover)?;
    }
    Ok(receipt)
}

// ---------------------------------------------------------------------------
// staging run state + umbrella state machine (compile.py:652-1036)
// ---------------------------------------------------------------------------

fn staging_runs(wiki: &Wiki) -> Result<Vec<String>, EngineError> {
    let base = wiki.p(".state/staging");
    let mut out = Vec::new();
    if !base.is_dir() {
        return Ok(out);
    }
    for e in std::fs::read_dir(&base)? {
        let e = e?;
        if e.file_type()?.is_dir() {
            out.push(e.file_name().to_string_lossy().into_owned());
        }
    }
    out.sort();
    Ok(out)
}

fn staging_state(wiki: &Wiki, run_id: &str) -> Option<&'static str> {
    let d = wiki.p(&format!(".state/staging/{run_id}"));
    if d.join("comparisons.json").is_file() {
        return Some("prepared");
    }
    if d.join("compile.json").is_file() {
        return Some("staged");
    }
    if d.join("candidates.jsonl").is_file() {
        return Some("candidates_ready");
    }
    None
}

fn staged_corpus_fingerprint(wiki: &Wiki, run_id: &str) -> Option<String> {
    let d = wiki.p(&format!(".state/staging/{run_id}"));
    for name in ["compile.json", "pending.json"] {
        let p = d.join(name);
        if p.is_file() {
            if let Ok(Some(v)) = load_json_opt_path(&p) {
                if let Some(fp) = v["corpus_fingerprint"].as_str() {
                    if !fp.is_empty() {
                        return Some(fp.to_string());
                    }
                }
            }
        }
    }
    None
}

/// `_orphan_candidate_runs` (compile.py:711-728) — staging dirs holding
/// staged candidates (compile.json) but NO pending.json.
fn orphan_candidate_runs(wiki: &Wiki) -> Result<HashMap<(String, i64), String>, EngineError> {
    let mut out = HashMap::new();
    for rid in staging_runs(wiki)? {
        let d = wiki.p(&format!(".state/staging/{rid}"));
        if d.join("pending.json").is_file() {
            continue;
        }
        let cj = d.join("compile.json");
        if !cj.is_file() {
            continue;
        }
        let payload = load_json_path(&cj)?;
        out.insert(
            (
                payload["source_id"].as_str().unwrap_or("").to_string(),
                payload["source_version"].as_i64().unwrap_or(0),
            ),
            rid,
        );
    }
    Ok(out)
}

/// `_adopt_orphan_run` (compile.py:731-778) — give an orphan candidates run
/// the pending.json its dir lacks.
fn adopt_orphan_run(wiki: &Wiki, run_id: &str) -> Result<Value, EngineError> {
    let d = wiki.p(&format!(".state/staging/{run_id}"));
    let compile_p = d.join("compile.json");
    let candidates_path = d.join("candidates.jsonl");
    let (sid, ver) = if compile_p.is_file() {
        let payload = load_json_path(&compile_p)?;
        if !candidates_path.is_file() {
            let mut body = String::new();
            for c in payload["candidates"].as_array().cloned().unwrap_or_default() {
                body.push_str(&canonical(&c));
                body.push('\n');
            }
            util::atomic_write(&candidates_path, body.as_bytes())?;
        }
        (
            payload["source_id"].as_str().unwrap_or("").to_string(),
            payload["source_version"].as_i64().unwrap_or(0),
        )
    } else {
        // bare candidates.jsonl — evidence-stamped source identity
        let raw = parse_candidates_file(&candidates_path)?;
        let ev = raw
            .first()
            .and_then(|c| c["evidence"].as_array().and_then(|a| a.first()).cloned());
        match ev {
            Some(ev) if truthy(&ev["source_id"]) => (
                ev["source_id"].as_str().unwrap_or("").to_string(),
                ev["source_version"].as_i64().unwrap_or(0),
            ),
            _ => {
                return Err(EngineError::Txn(format!(
                    "run {run_id}: candidates.jsonl has no compile.json and no \
                     evidence-stamped source id — cannot adopt orphan run"
                )))
            }
        }
    };
    let manifest = sources::get_manifest(wiki, &sid)?.unwrap_or(json!({}));
    let ver_entry = manifest["versions"]
        .as_array()
        .and_then(|a| {
            a.iter()
                .find(|v| v["version"].as_i64() == Some(ver))
                .cloned()
        })
        .unwrap_or(json!({}));
    let rel = ver_entry["normalized_path"]
        .as_str()
        .map(String::from)
        .unwrap_or_else(|| format!("sources/{sid}/content.md"));
    let meta = json!({
        "source_id": sid,
        "source_version": ver,
        "run_id": run_id,
        "content_path": wiki.p(&rel),
        "candidates_path": candidates_path,
        "classifications_path": d.join("classifications.json"),
    });
    write_json_compact(&d.join("pending.json"), &meta)?;
    Ok(meta)
}

/// `topics.parse_topics_file` (topics.py:176-201) — (rows, declared).
fn parse_topics_file(path: &Path) -> Result<(Vec<Value>, BTreeSet<String>), EngineError> {
    let text = std::fs::read_to_string(path)?;
    let mut rows = Vec::new();
    let mut declared = BTreeSet::new();
    for (i, line) in text.lines().enumerate() {
        let i = i + 1;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut row: Value = serde_json::from_str(line).map_err(|e| {
            EngineError::Txn(format!("topics.jsonl line {i}: invalid JSON: {e}"))
        })?;
        let Some(obj) = row.as_object_mut() else {
            return Err(EngineError::Txn(format!(
                "topics.jsonl line {i}: row must be an object"
            )));
        };
        for field in ["subject", "kind", "title"] {
            if py_str(&obj[field]).trim().is_empty() {
                return Err(EngineError::Txn(format!(
                    "topics.jsonl line {i}: missing '{field}'"
                )));
            }
        }
        let k = topics::norm_kind(&py_str(&obj["kind"]));
        if !topics::KINDS.contains(&k.as_str()) || k == "source" {
            let valid: Vec<&str> = topics::KINDS
                .iter()
                .filter(|x| **x != "source")
                .copied()
                .collect();
            return Err(EngineError::Txn(format!(
                "topics.jsonl line {i}: bad kind {} (valid: {})",
                py_repr(&obj["kind"]),
                valid.join(", ")
            )));
        }
        obj.insert("kind".to_string(), json!(k));
        declared.insert(py_str(&obj["subject"]).trim().to_string());
        rows.push(row.clone());
    }
    Ok((rows, declared))
}

/// `compile_umbrella` (compile.py:781-1036) — spec §1 3-phase state machine
/// over staging dirs (extract -> classify -> apply), resumable across CLI
/// invocations.
pub fn compile_umbrella(wiki: &Wiki, resume: bool) -> Result<Value, EngineError> {
    if !resume {
        let plan = compile_plan(wiki)?;
        // idempotent phase 1: reuse a run dir already minted for the same
        // (source_id, source_version) instead of creating a duplicate.
        let mut existing: HashMap<(String, i64), Value> = HashMap::new();
        let base = wiki.p(".state/staging");
        if base.is_dir() {
            for e in std::fs::read_dir(&base)? {
                let rid = e?.file_name().to_string_lossy().into_owned();
                let pj = base.join(&rid).join("pending.json");
                if pj.is_file() {
                    let m = load_json_path(&pj)?;
                    existing.insert(
                        (
                            m["source_id"].as_str().unwrap_or("").to_string(),
                            m["source_version"].as_i64().unwrap_or(0),
                        ),
                        m,
                    );
                }
            }
        }
        let mut pending: Vec<Value> = Vec::new();
        let orphans = orphan_candidate_runs(wiki)?;
        for entry in plan["pending"].as_array().cloned().unwrap_or_default() {
            let sid = entry["source_id"].as_str().unwrap_or("").to_string();
            let manifest = sources::get_manifest(wiki, &sid)?
                .ok_or_else(|| EngineError::Txn(format!("unknown source: {sid}")))?;
            let ver = if entry.get("version").is_some() {
                entry["version"].as_i64().unwrap_or(0)
            } else {
                manifest["versions"]
                    .as_array()
                    .and_then(|a| a.last())
                    .and_then(|v| v["version"].as_i64())
                    .unwrap_or(0)
            };
            let mut meta = existing.get(&(sid.clone(), ver)).cloned();
            let orphan = orphans.get(&(sid.clone(), ver));
            if let Some(orphan_id) = orphan {
                let orphan_usable = meta.is_none()
                    || !meta
                        .as_ref()
                        .and_then(|m| m["candidates_path"].as_str())
                        .map_or(false, |cp| Path::new(cp).is_file());
                if orphan_usable {
                    if let Some(m) = &meta {
                        if m["run_id"].as_str() != Some(orphan_id.as_str()) {
                            let d0 = wiki.p(&format!(
                                ".state/staging/{}",
                                m["run_id"].as_str().unwrap_or("")
                            ));
                            let only_pending = std::fs::read_dir(&d0)
                                .map(|it| {
                                    it.filter_map(|e| e.ok())
                                        .map(|e| e.file_name().to_string_lossy().into_owned())
                                        .collect::<Vec<_>>()
                                        == ["pending.json".to_string()]
                                })
                                .unwrap_or(false);
                            if only_pending {
                                let _ = std::fs::remove_file(d0.join("pending.json"));
                                let _ = std::fs::remove_dir(&d0);
                            }
                        }
                    }
                    meta = Some(adopt_orphan_run(wiki, orphan_id)?);
                }
            }
            if let Some(mut m) = meta {
                m["resumed_run"] = json!(true);
                pending.push(m);
                continue;
            }
            let ver_entry = manifest["versions"]
                .as_array()
                .and_then(|a| {
                    a.iter()
                        .find(|v| v["version"].as_i64() == Some(ver))
                        .cloned()
                })
                .unwrap_or(json!({}));
            let run_id = ids::new_run();
            let d = wiki.p(&format!(".state/staging/{run_id}"));
            std::fs::create_dir_all(&d)?;
            let rel = ver_entry["normalized_path"]
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| format!("sources/{sid}/content.md"));
            let meta = json!({
                "source_id": sid,
                "source_version": ver,
                "run_id": run_id,
                "content_path": wiki.p(&rel),
                "topics_path": d.join("topics.jsonl"),
                "candidates_path": d.join("candidates.jsonl"),
                "classifications_path": d.join("classifications.json"),
                "corpus_fingerprint": compute_corpus_fingerprint(wiki)?,
            });
            write_json_compact(&d.join("pending.json"), &meta)?;
            pending.push(meta);
        }
        return Ok(json!({"phase": "extract", "pending": pending}));
    }

    // resume: advance every run dir one phase
    let runs = staging_runs(wiki)?;

    // KI-1: staged comparisons reflect the corpus at stage time — discard
    // stale runs rather than resume against drifted evidence.
    let corpus_fp = compute_corpus_fingerprint(wiki)?;
    let mut stale_invalidated: Vec<String> = Vec::new();
    for run_id in &runs {
        let d = wiki.p(&format!(".state/staging/{run_id}"));
        if d.join("receipt.json").is_file() && !d.join("compile.json").is_file() {
            continue;
        }
        if let Some(staged_fp) = staged_corpus_fingerprint(wiki, run_id) {
            if staged_fp != corpus_fp {
                let _ = std::fs::remove_dir_all(&d);
                stale_invalidated.push(run_id.clone());
            }
        }
    }

    let mut needs_review_rows: Vec<Value> = Vec::new();
    let mut applied: Vec<Value> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut auto_totals = json!({
        "unrelated": 0,
        "duplicate": 0,
        "corroboration": 0,
        "rejected_tier_match": 0,
    });

    fn accumulate(auto_totals: &mut Value, receipt: &Value) {
        if let Some(obj) = receipt["auto"].as_object() {
            for (k, v) in obj {
                let cur = auto_totals[k].as_i64().unwrap_or(0);
                auto_totals[k] = json!(cur + v.as_i64().unwrap_or(0));
            }
        }
    }

    for run_id in &runs {
        let d = wiki.p(&format!(".state/staging/{run_id}"));
        let meta_p = d.join("pending.json");
        let meta: Value;
        if !meta_p.is_file() {
            // orphan run dirs: adoptable when they hold staged candidates
            // and are not already applied/prepared (compile.py:921-931).
            let adoptable = (d.join("compile.json").is_file()
                || d.join("candidates.jsonl").is_file())
                && !d.join("receipt.json").is_file()
                && !d.join("comparisons.json").is_file();
            if adoptable {
                match adopt_orphan_run(wiki, run_id) {
                    Ok(m) => meta = m,
                    Err(EngineError::Txn(msg)) => {
                        // one malformed orphan must not abort the whole
                        // resume — skip it, surface as a warning (SP3-F5 T3-N2)
                        warnings.push(format!("run {run_id} skipped: {msg}"));
                        continue;
                    }
                    Err(e) => return Err(e),
                }
            } else {
                continue;
            }
        } else {
            meta = load_json_path(&meta_p)?;
        }
        let cls_p = meta["classifications_path"]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or_else(|| d.join("classifications.json"));
        let cand_p = meta["candidates_path"]
            .as_str()
            .map(PathBuf::from)
            .unwrap_or_else(|| d.join("candidates.jsonl"));
        let compile_p = d.join("compile.json");
        let state = staging_state(wiki, run_id);

        let apply_run = |wiki: &Wiki,
                             run_id: &str,
                             d: &Path,
                             meta: &Value,
                             classifications: Vec<Value>,
                             topics_rows: Option<Vec<Value>>,
                             topics_missing: bool,
                             applied: &mut Vec<Value>,
                             auto_totals: &mut Value|
         -> Result<(), EngineError> {
            let manifest = sources::get_manifest(wiki, meta["source_id"].as_str().unwrap_or(""))?
                .unwrap_or(json!({}));
            let declared_by = json!({
                "source_id": meta["source_id"],
                "version": meta["source_version"],
                "title": if truthy(&manifest["title"]) { manifest["title"].clone() } else { meta["source_id"].clone() },
            });
            let mut receipt = reconcile_apply(
                wiki,
                run_id,
                classifications,
                wiki.revision()?,
                topics_rows,
                Some(&declared_by),
                false,
            )?;
            if topics_missing {
                receipt["topics"]["warnings"]
                    .as_array_mut()
                    .map(|w| w.push(json!(format!("no_topics_declared ({run_id})"))));
            }
            receipt["topics"]["unclaimed"] = json!(topics::unclaimed(wiki)?);
            write_json_indent(&d.join("receipt.json"), &receipt)?;
            applied.push(receipt.clone());
            accumulate(auto_totals, &receipt);
            Ok(())
        };

        if state == Some("prepared") && !compile_p.is_file() {
            // applied on an earlier resume — replay the cached receipt
            let rcpt_p = d.join("receipt.json");
            if !rcpt_p.is_file() {
                return Err(EngineError::Txn(format!(
                    "run {run_id} looks applied (compile.json gone) but {} is missing; \
                     cannot resume — delete {} to re-stage",
                    rcpt_p.display(),
                    d.display()
                )));
            }
            let receipt = load_json_path(&rcpt_p)?;
            applied.push(receipt.clone());
            accumulate(&mut auto_totals, &receipt);
            continue;
        }

        let topics_p = d.join("topics.jsonl");
        let (topics_rows, topics_missing) = if topics_p.is_file() {
            (Some(parse_topics_file(&topics_p)?.0), false)
        } else {
            (None, true)
        };

        if cls_p.is_file() {
            let is_jsonl = cls_p.to_string_lossy().ends_with(".jsonl");
            let mut classifications = if is_jsonl {
                parse_candidates_file(&cls_p)?
            } else {
                let v = load_json_path(&cls_p)?;
                match v {
                    Value::Object(o) => o
                        .get("classifications")
                        .cloned()
                        .unwrap_or(json!([]))
                        .as_array()
                        .cloned()
                        .unwrap_or_default(),
                    Value::Array(a) => a,
                    // Python iterates the raw value here — a non-dict/
                    // non-list file crashes mid-apply; reject up front.
                    other => {
                        return Err(EngineError::Txn(format!(
                            "{}: classifications must be an array or object, got {}",
                            cls_p.display(),
                            py_type(&other)
                        )))
                    }
                }
            };
            apply_run(wiki, run_id, &d, &meta, std::mem::take(&mut classifications), topics_rows, topics_missing, &mut applied, &mut auto_totals)?;
            continue;
        }

        if !matches!(state, Some("staged") | Some("prepared")) {
            if !cand_p.is_file() {
                return Err(EngineError::Txn(format!(
                    "run {run_id}: expected {} (write shorthand candidates there)",
                    cand_p.display()
                )));
            }
            let raw = parse_candidates_file(&cand_p)?;
            let manifest =
                sources::get_manifest(wiki, meta["source_id"].as_str().unwrap_or(""))?
                    .unwrap_or(json!({}));
            let root_origin = manifest["root_origin"]
                .as_str()
                .map(String::from)
                .unwrap_or_else(|| meta["source_id"].as_str().unwrap_or("").to_string());
            let mut cands: Vec<Value> = raw
                .iter()
                .map(|c| {
                    normalize_candidate(
                        c,
                        meta["source_id"].as_str().unwrap_or(""),
                        meta["source_version"].as_i64().unwrap_or(0),
                        &root_origin,
                    )
                })
                .collect::<Result<_, _>>()?;
            let report_p = d.join("report.json");
            let report = if report_p.is_file() {
                load_json_path(&report_p)?
            } else {
                json!({
                    "source_id": meta["source_id"],
                    "source_version": meta["source_version"],
                    "coverage": {"text": "complete"},
                    "warnings": [],
                    "parser": {"name": "agent", "version": "shorthand-jsonl"},
                })
            };
            // stage into THIS run dir via the shared writer.
            stage_payload(
                wiki,
                &d,
                meta["source_id"].as_str().unwrap_or(""),
                meta["source_version"].as_i64().unwrap_or(0),
                &mut cands,
                &report,
            )?;
        }

        if !d.join("comparisons.json").is_file() {
            reconcile_prepare(wiki, run_id)?;
        }
        let cmp = load_json_path(&d.join("comparisons.json"))?["comparisons"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let rows: Vec<Value> = cmp
            .into_iter()
            .filter(|r| r["auto"].is_null())
            .map(|mut r| {
                r["run_id"] = json!(run_id);
                r["source_id"] = meta["source_id"].clone();
                r
            })
            .collect();
        let needs_agent = !rows.is_empty();
        needs_review_rows.extend(rows);
        if !needs_agent {
            // all auto — apply immediately with empty classifications
            apply_run(wiki, run_id, &d, &meta, Vec::new(), topics_rows, topics_missing, &mut applied, &mut auto_totals)?;
        }
    }

    if !needs_review_rows.is_empty() {
        return Ok(json!({
            "phase": "classify",
            "needs_classifications": true,
            "needs_review": needs_review_rows,
            "auto": auto_totals,
            "warnings": warnings,
            "stale_invalidated": stale_invalidated,
        }));
    }

    if !applied.is_empty() {
        crate::pages::build_pages(wiki, wiki.revision()?)?;
        let verify = crate::pages::verify(wiki)?;
        let mut total = json!({
            "claims_created": 0,
            "claims_updated": 0,
            "claims_superseded": 0,
            "review_items": 0,
        });
        let mut conflicts: Vec<Value> = Vec::new();
        for r in &applied {
            for k in ["claims_created", "claims_updated", "claims_superseded", "review_items"] {
                total[k] = json!(
                    total[k].as_i64().unwrap_or(0)
                        + r["changes"][k].as_i64().unwrap_or(0)
                );
            }
            conflicts.extend(r["conflicts"].as_array().cloned().unwrap_or_default());
        }
        let mut topics_total = json!({
            "declared": 0,
            "registered": 0,
            "auto_registered": [],
            "kind_conflicts": [],
            "decision_topics": [],
            "unclaimed": [],
            "warnings": [],
        });
        for r in &applied {
            let t = &r["topics"];
            topics_total["declared"] = json!(
                topics_total["declared"].as_i64().unwrap_or(0)
                    + t["declared"].as_i64().unwrap_or(0)
            );
            topics_total["registered"] = json!(
                topics_total["registered"].as_i64().unwrap_or(0)
                    + t["registered"].as_i64().unwrap_or(0)
            );
            for k in ["auto_registered", "kind_conflicts", "decision_topics", "warnings"] {
                let have: Vec<Value> = topics_total[k].as_array().cloned().unwrap_or_default();
                let mut merged = have;
                for v in t[k].as_array().cloned().unwrap_or_default() {
                    if !merged.contains(&v) {
                        merged.push(v);
                    }
                }
                topics_total[k] = json!(merged);
            }
        }
        topics_total["unclaimed"] = json!(topics::unclaimed(wiki)?);
        return Ok(json!({
            "phase": "done",
            "needs_classifications": false,
            "needs_review": needs_review_rows,
            "auto": auto_totals,
            "changes": total,
            "conflicts": conflicts,
            "verify": verify,
            "receipts": applied,
            "warnings": warnings,
            "stale_invalidated": stale_invalidated,
            "topics": topics_total,
        }));
    }

    Ok(json!({
        "phase": "waiting",
        "needs_classifications": false,
        "auto": auto_totals,
        "warnings": warnings,
        "stale_invalidated": stale_invalidated,
        "message": "no staged runs — run `compile` first or write candidates.jsonl",
    }))
}
