//! `compile-plan` — read-only pending-evidence listing (wiki.py:
//! cmd_compile_plan 244-247 parity). Emits `{"pending": [...]}` of
//! source versions not yet marked compiled.

use serde_json::Value;

use crate::compile;
use crate::err::EngineError;
use crate::store::Wiki;

pub fn run(wiki: &Wiki) -> Result<Value, EngineError> {
    compile::compile_plan(wiki)
}
