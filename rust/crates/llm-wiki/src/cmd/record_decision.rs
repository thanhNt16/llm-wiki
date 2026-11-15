//! `record-decision` — file-driven decision write path
//! (wiki.py:586-589 cmd_record_decision): `--file` JSON is validated and
//! committed via `decisions::create_decision`; the engine stamps
//! id/recorded_at/version.

use serde_json::Value;

use crate::decisions;
use crate::err::EngineError;
use crate::store::Wiki;

pub fn run(wiki: &Wiki, file: &std::path::Path) -> Result<Value, EngineError> {
    let text = std::fs::read_to_string(file)?;
    let data: Value = serde_json::from_str(&text)?;
    decisions::create_decision(wiki, &data)
}
