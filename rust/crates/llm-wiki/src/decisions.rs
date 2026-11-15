//! Decision store — `decisions/decision_*.json` canonical io
//! (claims.py:230-315 + create_decision 385-428). Decisions are what the
//! project deliberately chose (PRD §3): flag-driven `decide` and
//! file-driven `record-decision` both land here.

use serde_json::{json, Value};

use crate::claims;
use crate::err::EngineError;
use crate::evidence::sources;
use crate::schema::{self, py_repr};
use crate::store::Wiki;
use crate::txn::{Txn, TxnOpts};

pub fn decision_rel(did: &str) -> String {
    format!("decisions/{did}.json")
}

/// `claims.load_decision` (claims.py:234).
pub fn load_decision(wiki: &Wiki, did: &str) -> Result<Value, EngineError> {
    if !did.starts_with("decision_") || !crate::ids::validate(did) {
        return Err(EngineError::Txn(format!(
            "decision id invalid: {}",
            schema::py_repr_str(did)
        )));
    }
    wiki.load_json_opt(&decision_rel(did))?
        .ok_or_else(|| EngineError::Txn(format!("unknown decision: {did}")))
}

/// `claims.save_decision` (claims.py:244) — staged-write-aware version
/// bump (once per commit), immutable `recorded_at`, schema validation,
/// derived index `decisions[]` append, staged file write.
pub fn save_decision(
    wiki: &Wiki,
    txn: &mut Txn,
    decision: &mut Value,
) -> Result<(), EngineError> {
    let did = decision["id"].as_str().unwrap_or("").to_string();
    if !crate::ids::validate(&did) {
        return Err(EngineError::Txn(format!(
            "decision id invalid: {}",
            py_repr(&decision["id"])
        )));
    }
    // prefer this txn's staged copy: a second save in the same txn must
    // not re-read live-disk bytes (SP3-F3 — bump-once, staged recorded_at).
    let (existing, from_staging) = {
        if let Some(v) = txn.staged_value(&decision_rel(&did)) {
            (Some(v.clone()), true)
        } else {
            let staged = txn.staging_path(&decision_rel(&did));
            if staged.is_file() {
                (
                    Some(serde_json::from_str(&std::fs::read_to_string(staged)?)?),
                    true,
                )
            } else {
                (wiki.load_json_opt(&decision_rel(&did))?, false)
            }
        }
    };
    if let Some(existing) = existing {
        if !from_staging {
            decision["version"] = json!(existing["version"].as_i64().unwrap_or(0) + 1);
        } else {
            decision["version"] = existing["version"].clone(); // bump already applied
        }
        decision["recorded_at"] = existing["recorded_at"].clone();
    } else {
        decision["version"] = json!(1);
        if decision.get("recorded_at").is_none() {
            decision["recorded_at"] = json!(crate::util::now_iso());
        }
    }
    let schema_v: Value = serde_json::from_str(schema::DECISION_SCHEMA)?;
    let errors = schema::validate(decision, &schema_v);
    if !errors.is_empty() {
        return Err(EngineError::Txn(format!(
            "decision {did} invalid: {}",
            errors.join("; ")
        )));
    }
    // `_load_index_for_staging` + decisions[] append (claims.py:277-285).
    claims::ensure_index_for_staging(wiki, txn)?;
    let did2 = did.clone();
    txn.stage_state_with(crate::txn::CLAIMS_INDEX_REL, move |idx| {
        if !idx["decisions"].is_array() {
            idx["decisions"] = json!([]);
        }
        let arr = idx["decisions"].as_array_mut().unwrap();
        if !arr.iter().any(|v| v.as_str() == Some(did2.as_str())) {
            arr.push(json!(did2));
        }
    })?;
    txn.stage_state(&decision_rel(&did), decision)?;
    Ok(())
}

/// `claims._scan_decisions` (claims.py:287) — canonical scan, sorted by
/// filename.
pub fn scan_decisions(wiki: &Wiki) -> Result<Vec<Value>, EngineError> {
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

/// `claims.create_decision` (claims.py:385) — validate + commit a
/// decision record for `record-decision`. Caller supplies
/// title/status/decided_on/claims/evidence/body/scope; the engine stamps
/// id/recorded_at/version.
pub fn create_decision(wiki: &Wiki, data: &Value) -> Result<Value, EngineError> {
    if !data.is_object() {
        return Err(EngineError::Txn(
            "decision input must be a JSON object".to_string(),
        ));
    }
    for banned in ["id", "recorded_at", "version"] {
        if data.get(banned).is_some() {
            return Err(EngineError::Txn(format!(
                "decision input must not include {} — engine-stamped",
                schema::py_repr_str(banned)
            )));
        }
    }
    if !data.get("claims").map_or(true, Value::is_array) {
        return Err(EngineError::Txn(
            "decision input 'claims' must be a list".to_string(),
        ));
    }
    if !data.get("evidence").map_or(true, Value::is_array) {
        return Err(EngineError::Txn(
            "decision input 'evidence' must be a list".to_string(),
        ));
    }
    for cid in data
        .get("claims")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
    {
        let c = claims::load_claim(wiki, cid.as_str().unwrap_or(""))?;
        let status = c.get("status").and_then(Value::as_str).unwrap_or("");
        if claims::TERMINAL.contains(&status) {
            return Err(EngineError::Txn(format!(
                "claim {} is {status}; cannot support a decision",
                cid.as_str().unwrap_or("?")
            )));
        }
    }
    for (i, ev) in data
        .get("evidence")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        if !ev.is_object() {
            return Err(EngineError::Txn(format!(
                "evidence[{i}] must be a JSON object"
            )));
        }
        let sid = ev.get("source_id").and_then(Value::as_str).unwrap_or("");
        // malformed sid → same "unknown source" surface (claims.py:410-414)
        let manifest = sources::get_manifest(wiki, sid).unwrap_or(None);
        if manifest.is_none() {
            return Err(EngineError::Txn(format!("unknown source: {sid}")));
        }
    }
    let mut decision = data.clone();
    let did = crate::ids::new("decision");
    decision["id"] = json!(did);
    decision["recorded_at"] = json!(crate::util::now_iso());
    decision["version"] = json!(1);
    let mut txn = Txn::begin_with(
        wiki,
        &crate::ids::new_run(),
        TxnOpts {
            command: Some("wiki-record-decision".to_string()),
            ..Default::default()
        },
    )?;
    save_decision(wiki, &mut txn, &mut decision)?;
    txn.changes.insert("decisions_created".to_string(), json!(1));
    let receipt = txn.commit()?;
    Ok(json!({
        "id": decision["id"],
        "decision": decision,
        "receipt": receipt,
    }))
}
