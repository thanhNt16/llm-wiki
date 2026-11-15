//! `decide` — flag-driven project-decision write path (wiki.py:552-583
//! cmd_decide, the second/winning Python definition; parser block
//! wiki.py:891-905). `record-decision` covers the file-driven path.

use serde_json::{json, Value};

use crate::claims;
use crate::decisions;
use crate::err::EngineError;
use crate::evidence::sources;
use crate::store::Wiki;
use crate::txn::{Txn, TxnOpts};

#[derive(Default)]
pub struct DecideArgs {
    /// `--title` (required).
    pub title: String,
    /// `--claims` — comma-separated claim_<id>s the decision resolves.
    pub claims: Option<String>,
    /// `--evidence` — repeatable source_<id>@<version> refs.
    pub evidence: Vec<String>,
    /// `--authority` — authority label, e.g. explicit_project_decision.
    pub authority: Option<String>,
    /// `--status` — proposed|accepted|superseded|retracted (default
    /// accepted).
    pub status: String,
    /// `--decided-on` — YYYY-MM-DD (default: today, UTC).
    pub decided_on: Option<String>,
    /// `--body` — rationale text.
    pub body: Option<String>,
    /// `--base-revision` — optimistic-concurrency base.
    pub base_revision: Option<i64>,
}

/// `wiki.py:_parse_decision_evidence` — each spec is
/// `source_<id>@<version>`; the version must exist in the source
/// manifest.
fn parse_decision_evidence(wiki: &Wiki, specs: &[String]) -> Result<Vec<Value>, EngineError> {
    let mut out = Vec::new();
    for spec in specs {
        let (sid, ver) = match spec.split_once('@') {
            Some((s, v)) => (s, v),
            None => {
                return Err(EngineError::Txn(format!(
                    "--evidence refs must look like source_<id>@<version>: {spec:?}"
                )))
            }
        };
        if !crate::ids::validate(sid)
            || !sid.starts_with("source_")
            || ver.is_empty()
            || !ver.chars().all(|c| c.is_ascii_digit())
        {
            return Err(EngineError::Txn(format!(
                "--evidence refs must look like source_<id>@<version>: {spec:?}"
            )));
        }
        let manifest = sources::get_manifest(wiki, sid)?
            .ok_or_else(|| EngineError::Txn(format!("unknown evidence source: {sid}")))?;
        let version: i64 = ver.parse().unwrap_or(i64::MAX);
        let exists = manifest
            .get("versions")
            .and_then(Value::as_array)
            .map(|vs| {
                vs.iter()
                    .any(|v| v.get("version").and_then(Value::as_i64) == Some(version))
            })
            .unwrap_or(false);
        if !exists {
            return Err(EngineError::Txn(format!(
                "unknown version {version} of {sid}"
            )));
        }
        out.push(json!({
            "source_id": sid,
            "source_version": version,
            "locator": {
                "type": "text",
                "value": manifest
                    .get("origin")
                    .and_then(Value::as_str)
                    .unwrap_or(sid),
            },
        }));
    }
    Ok(out)
}

pub fn run(wiki: &Wiki, args: &DecideArgs) -> Result<Value, EngineError> {
    if !wiki.exists() {
        return Err(EngineError::Txn(
            "wiki not initialized; run wiki-init first".to_string(),
        ));
    }
    let claim_ids: Vec<String> = args
        .claims
        .as_deref()
        .unwrap_or("")
        .split(',')
        .map(|c| c.trim().to_string())
        .filter(|c| !c.is_empty())
        .collect();
    for cid in &claim_ids {
        if !cid.starts_with("claim_") || !crate::ids::validate(cid) {
            return Err(EngineError::Txn(format!(
                "--claims entries must be claim_<id>: {cid:?}"
            )));
        }
        claims::load_claim(wiki, cid)?; // TxnError on unknown id
    }
    let evidence = parse_decision_evidence(wiki, &args.evidence)?;
    let mut sorted: Vec<String> = claim_ids
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    sorted.sort();
    let mut decision = json!({
        "id": crate::ids::new("decision"),
        "title": args.title,
        "status": args.status,
        "decided_on": args
            .decided_on
            .clone()
            .unwrap_or_else(crate::util::today),
        "claims": sorted,
        "evidence": evidence,
    });
    if let Some(body) = &args.body {
        if !body.is_empty() {
            decision["body"] = json!(body);
        }
    }
    if let Some(authority) = &args.authority {
        if !authority.is_empty() {
            decision["authority"] = json!({"type": authority});
        }
    }
    let mut txn = Txn::begin_with(
        wiki,
        &crate::ids::new_run(),
        TxnOpts {
            command: Some("wiki-decide".to_string()),
            ..Default::default()
        },
    )?;
    decisions::save_decision(wiki, &mut txn, &mut decision)?;
    txn.changes.insert("decisions_created".to_string(), json!(1));
    let base = match args.base_revision {
        Some(b) => b,
        None => wiki.revision()?,
    };
    txn.base_revision = base;
    let receipt = txn.commit()?;
    Ok(json!({
        "decision": decision,
        "run_id": receipt.get("run_id").cloned().unwrap_or(Value::Null),
        "revision": receipt.get("revision").cloned().unwrap_or(Value::Null),
    }))
}
