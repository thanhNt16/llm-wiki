//! `reconcile-apply` — commit one prepared run's classifications
//! (wiki.py:cmd_reconcile_apply 288-295 parity). `--classifications`
//! accepts a JSON array or `{"classifications": [...]}`;
//! `--base-revision` defaults to the current revision;
//! `--use-prefill` (spec §6.4) lets ≥0.7-confidence `auto_prefill`
//! blocks fill otherwise-unclassified candidates.

use std::path::PathBuf;

use serde_json::Value;

use crate::compile;
use crate::err::EngineError;
use crate::store::Wiki;

pub struct ReconcileApplyArgs {
    /// run id minted by `compile` or `stage-candidates` (required)
    pub run: String,
    /// classifications JSON file (required)
    pub classifications: PathBuf,
    /// optimistic-concurrency base revision (default: current)
    pub base_revision: Option<i64>,
    /// consume ≥0.7-confidence auto_prefill blocks (spec §6.4)
    pub use_prefill: bool,
}

pub fn run(wiki: &Wiki, args: &ReconcileApplyArgs) -> Result<Value, EngineError> {
    let text = std::fs::read_to_string(&args.classifications).map_err(|e| {
        EngineError::Txn(format!(
            "cannot read {}: {e}",
            args.classifications.display()
        ))
    })?;
    let data: Value = serde_json::from_str(&text)?;
    let classifications: Vec<Value> = match data {
        Value::Object(o) => o
            .get("classifications")
            .cloned()
            .unwrap_or_else(|| serde_json::json!([]))
            .as_array()
            .cloned()
            .unwrap_or_default(),
        Value::Array(a) => a,
        _ => Vec::new(),
    };
    let base = match args.base_revision {
        Some(b) => b,
        None => wiki.revision()?,
    };
    compile::reconcile_apply(
        wiki,
        &args.run,
        classifications,
        base,
        None,
        None,
        args.use_prefill,
    )
}
