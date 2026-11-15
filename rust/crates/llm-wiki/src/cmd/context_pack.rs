//! `context-pack` — budgeted context assembly + receipt
//! (wiki.py:433-442 cmd_context_pack, parser block wiki.py:862-868).
//! Mutating (`_MUTATING_COMMANDS` member — commits the pack artifact).

use serde_json::Value;

use crate::contextpack;
use crate::err::EngineError;
use crate::store::Wiki;

#[derive(Default)]
pub struct ContextPackArgs {
    /// `--task` — task description driving relevance filtering.
    pub task: Option<String>,
    /// `--budget` — hard token budget (default: context_budget_default).
    pub budget: Option<i64>,
    /// `--resume` — change briefing since the latest pack.
    pub resume: bool,
    /// `--changes-since` — context id or ISO-8601 timestamp to diff from.
    pub changes_since: Option<String>,
    /// `--print-pack` — print the pack file instead of the JSON payload
    /// (handled in main, like Python's `print(f.read())`).
    pub print_pack: bool,
}

pub fn run(wiki: &Wiki, args: &ContextPackArgs) -> Result<Value, EngineError> {
    contextpack::build(
        wiki,
        args.task.as_deref().unwrap_or(""),
        args.budget,
        args.resume,
        args.changes_since.as_deref(),
    )
}
