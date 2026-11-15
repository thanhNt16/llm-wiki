//! `review` — queue commands (wiki.py:445-476 cmd_review parity):
//! `list | show | triage | apply | act`.
//!
//! - `list`: `{"items": [...], "summary": {...}}`; `--kind`/`--status`
//!   filters, `--all` lifts the status filter.
//! - `show --item <id>`: the raw queue item (Txn on unknown id).
//! - `triage [--group-by K]`: grouped rows + suggested actions.
//! - `apply --file ACTIONS.json [--base-revision N]`: batch in ONE txn.
//! - `act --item <id> --action <a> [--params P.json] [--base-revision N]`:
//!   single action; emits `{"item": ..., "run_id": ...}`.

use serde_json::{json, Value};

use crate::err::EngineError;
use crate::review;
use crate::store::Wiki;

#[derive(Default)]
pub struct ReviewArgs {
    /// review sub-command: list|show|triage|apply|act.
    pub review_cmd: Option<String>,
    /// `--item` — queue item id for show/act.
    pub item: Option<String>,
    /// `--action` — action name for act (review.ACTIONS).
    pub action: Option<String>,
    /// `--params` — JSON file with act params.
    pub params: Option<std::path::PathBuf>,
    /// `--file` — JSON actions list for apply.
    pub file: Option<std::path::PathBuf>,
    /// `--group-by` — triage group key (default: "subject").
    pub group_by: Option<String>,
    /// `--kind` — list filter.
    pub kind: Option<String>,
    /// `--status` — list filter.
    pub status: Option<String>,
    /// `--all` — include deferred/resolved in list.
    pub all: bool,
    /// `--base-revision` — optimistic-concurrency base for act/apply.
    pub base_revision: Option<i64>,
}

fn load_json_file(path: &std::path::Path) -> Result<Value, EngineError> {
    let text = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}

/// wiki.py `_needs_lock_wait` — only `act`/`apply` take commit.lock.
pub fn takes_commit_lock(args: &ReviewArgs) -> bool {
    matches!(args.review_cmd.as_deref(), Some("act") | Some("apply"))
}

pub fn run(wiki: &Wiki, args: &ReviewArgs) -> Result<Value, EngineError> {
    match args.review_cmd.as_deref() {
        Some("list") => {
            let status = if args.all {
                None
            } else {
                Some(args.status.as_deref().unwrap_or("open"))
            };
            Ok(json!({
                "items": review::list_items(wiki, args.kind.as_deref(), status)?,
                "summary": review::summary(wiki)?,
            }))
        }
        Some("show") => {
            let item = args.item.as_deref().unwrap_or("None");
            review::show(wiki, item)
        }
        Some("triage") => review::triage(
            wiki,
            args.group_by.as_deref().unwrap_or("subject"),
        ),
        Some("apply") => {
            let Some(file) = &args.file else {
                return Err(EngineError::Txn(
                    "review apply requires --file ACTIONS.json".to_string(),
                ));
            };
            let data = load_json_file(file)?;
            let actions = match &data {
                Value::Object(m) => m
                    .get("actions")
                    .cloned()
                    .unwrap_or_else(|| json!([])),
                other => other.clone(),
            };
            let Value::Array(list) = actions else {
                return Err(EngineError::Txn(
                    "--file must be a JSON list of {\"id\",\"action\",\"params\"} \
                     or {\"actions\": [...]}"
                        .to_string(),
                ));
            };
            let base = match args.base_revision {
                Some(b) => b,
                None => wiki.revision()?,
            };
            review::apply_batch(wiki, &list, base)
        }
        Some("act") => {
            let params = match &args.params {
                Some(p) => load_json_file(p)?,
                None => json!({}),
            };
            let base = match args.base_revision {
                Some(b) => b,
                None => wiki.revision()?,
            };
            let item = args.item.as_deref().unwrap_or("None");
            let action = args.action.as_deref().unwrap_or("None");
            let out = review::act(wiki, item, action, &params, base)?;
            Ok(json!({
                "item": out.get("item").cloned().unwrap_or(Value::Null),
                "run_id": out.get("run_id").cloned().unwrap_or(Value::Null),
            }))
        }
        _ => Err(EngineError::Txn(
            "review sub-command required: list|show|act|triage|apply".to_string(),
        )),
    }
}
