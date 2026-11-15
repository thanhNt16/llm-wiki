//! `jev` — read-only Jev advisory helper (wiki.py:cmd_jev 676-698 parity).
//! `--check` answers "configured?" and always exits 0; `--state` +
//! `--question` run one typed query and print the normalized `ask` result
//! (disabled/failed included — never raises). Answers cache into
//! `.state/index.sqlite` when `root` hosts a wiki (spec §3).

use std::path::Path;

use serde_json::{json, Value};

use crate::err::EngineError;
use crate::jev;

#[derive(Default)]
pub struct JevArgs {
    /// report Jev readiness (exit 0 either way)
    pub check: bool,
    /// state text for a one-shot query
    pub state: Option<String>,
    /// questions JSON object, e.g. '{"q":{"type":"noul","instructions":"..."}}'
    pub question: Option<String>,
}

pub fn run(root: &Path, args: &JevArgs) -> Result<Value, EngineError> {
    if args.check {
        let missing = jev::missing_env();
        if !missing.is_empty() {
            return Ok(json!({
                "ready": false,
                "reason": format!("{} unset", missing.join(" and ")),
            }));
        }
        return Ok(json!({
            "ready": true,
            "model": std::env::var(jev::MODEL_ENV).unwrap_or_default(),
            "endpoint": jev::endpoint(),
        }));
    }
    let Some(question_text) = &args.question else {
        return Err(EngineError::Txn("provide --check or --question <json>".into()));
    };
    let question: Value = serde_json::from_str(question_text).map_err(|e| {
        EngineError::Txn(format!("--question is not valid JSON: {e}"))
    })?;
    if !question.is_object() {
        return Err(EngineError::Txn(
            "--question must be a JSON object {name: question}".into(),
        ));
    }
    Ok(jev::ask_with(
        Some(root),
        args.state.as_deref().unwrap_or(""),
        &question,
    ))
}
