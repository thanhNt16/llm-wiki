//! `subjects` — read-only subject rollup (wiki.py:cmd_subjects 426-429
//! parity). Emits `{"subjects": [...]}` from `query.subjects(wiki, prefix)`.

use serde_json::{json, Value};

use crate::err::EngineError;
use crate::store::Wiki;
use crate::subjects;

#[derive(Default)]
pub struct SubjectsArgs {
    /// only subjects starting with this prefix
    pub prefix: Option<String>,
}

pub fn run(wiki: &Wiki, args: &SubjectsArgs) -> Result<Value, EngineError> {
    Ok(json!({
        "subjects": subjects::subjects(wiki, args.prefix.as_deref())?,
    }))
}
