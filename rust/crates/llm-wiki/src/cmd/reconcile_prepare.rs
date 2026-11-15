//! `reconcile-prepare` — tier-matched comparisons for one staged run
//! (wiki.py:cmd_reconcile_prepare 282-285 parity; bounded matches[]
//! per spec §6.4). Writes `comparisons.json` + `stamps.json` into the
//! run dir and echoes the same payload on stdout.

use serde_json::Value;

use crate::compile;
use crate::err::EngineError;
use crate::store::Wiki;

pub struct ReconcilePrepareArgs {
    /// run id minted by `compile` or `stage-candidates` (required)
    pub run: String,
}

pub fn run(wiki: &Wiki, args: &ReconcilePrepareArgs) -> Result<Value, EngineError> {
    compile::reconcile_prepare(wiki, &args.run)
}
