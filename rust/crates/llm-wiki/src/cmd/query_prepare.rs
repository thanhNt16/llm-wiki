//! `query-prepare` — read-only query preparation (wiki.py
//! cmd_query_prepare 421-424 parity): emits `query.prepare(wiki, question,
//! as_of)` verbatim, exit 0 always — an unsupported question is a domain
//! answer (`gaps[]`/`superseded[]`), not an error.

use serde_json::Value;

use crate::err::EngineError;
use crate::query;
use crate::store::Wiki;

#[derive(Default)]
pub struct QueryPrepareArgs {
    /// the question to prepare evidence for (required)
    pub question: String,
    /// historical query: answer as of this ISO date/datetime
    pub as_of: Option<String>,
}

pub fn run(wiki: &Wiki, args: &QueryPrepareArgs) -> Result<Value, EngineError> {
    query::prepare(wiki, &args.question, args.as_of.as_deref())
}
