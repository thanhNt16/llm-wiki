//! `page-targets` — synthesis targets over the eligible-claim grouping
//! (wiki.py:cmd_page_targets 305-335 parity). Read-only; emits
//! `synthesize.page_targets` verbatim.
//!
//! `--decide` is advisory: when the Jev env is configured it attaches a
//! kind verdict to heuristic-routed targets. `jev::ask_with` never raises
//! — transport or parse failures simply leave the advisory field absent,
//! identical to running with the env unset.

use serde_json::Value;

use crate::err::EngineError;
use crate::jev;
use crate::store::Wiki;
use crate::synthesize;

#[derive(Default)]
pub struct PageTargetsArgs {
    pub kind: Option<String>,
    pub stale_only: bool,
    pub min_claims: i64,
    pub min_sources: i64,
    /// attach advisory Jev kind verdicts to heuristic-routed targets
    /// (no-op when Jev env is unset)
    pub decide: bool,
}

pub fn run(wiki: &Wiki, args: &PageTargetsArgs) -> Result<Value, EngineError> {
    let mut out = synthesize::page_targets(
        wiki,
        args.kind.as_deref(),
        args.stale_only,
        args.min_claims,
        args.min_sources,
    )?;
    if args.decide && jev::is_ready() {
        // advisory only: heuristic-routed targets get a Jev kind verdict;
        // any failure simply leaves the target without a decision field.
        if let Some(targets) = out.get_mut("targets").and_then(Value::as_array_mut) {
            for t in targets {
                let matched = t
                    .get("matched_rule")
                    .map(|v| {
                        v.as_str()
                            .map(str::to_string)
                            .unwrap_or_else(|| v.to_string())
                    })
                    .unwrap_or_default();
                if !matched.starts_with("heuristic:") {
                    continue;
                }
                let mut preds: Vec<String> = t
                    .get("claim_details")
                    .and_then(Value::as_array)
                    .map(|ds| {
                        ds.iter()
                            .filter_map(|d| {
                                d.get("predicate")
                                    .and_then(Value::as_str)
                                    .map(str::to_string)
                            })
                            .collect()
                    })
                    .unwrap_or_default();
                preds.sort();
                preds.dedup();
                let subject = t.get("subject").and_then(Value::as_str).unwrap_or("");
                let state = format!("subject: {subject}; predicates: {}", preds.join(", "));
                let r = jev::ask_with(
                    Some(&wiki.root),
                    &state,
                    &serde_json::json!({
                        "kind": {
                            "type": "choice",
                            "options": ["concepts", "entities", "procedures",
                                        "questions"],
                            "instructions": "Which wiki page kind fits this subject best?"
                        }
                    }),
                );
                if r.get("ok").and_then(Value::as_bool) == Some(true) {
                    let v = r
                        .get("answers")
                        .and_then(|a| a.get("kind"))
                        .and_then(|a| a.get("value"))
                        .cloned()
                        .unwrap_or(Value::Null);
                    let dist = v.get("distribution").cloned().unwrap_or_else(|| serde_json::json!({}));
                    let pick = v.get("pick").cloned().unwrap_or(Value::Null);
                    let conf = pick
                        .as_str()
                        .and_then(|p| dist.get(p).and_then(Value::as_f64))
                        .unwrap_or(0.0);
                    if let Some(obj) = t.as_object_mut() {
                        obj.insert(
                            "decision".to_string(),
                            serde_json::json!({
                                "suggested_kind": pick,
                                "confidence": conf,
                            }),
                        );
                    }
                }
            }
        }
    }
    Ok(out)
}
