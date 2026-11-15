//! `topics` — registry listing and `--backfill` (wiki.py:cmd_topics
//! 628-645 parity).
//!
//! List payload: `{"version": int, "topics": [{subject, kind, status,
//! title, claim_count, declared_by, auto_registered}]}` sorted by subject;
//! `claim_count` is a claims/* scan rollup. `--backfill` rebuilds the
//! registry from claims and emits `{topics_registered, warnings}`.

use serde_json::{json, Value};

use crate::claims;
use crate::err::EngineError;
use crate::store::Wiki;
use crate::topics;

#[derive(Default)]
pub struct TopicsArgs {
    /// Rebuild the registry from the claims scan (topics.py:backfill).
    pub backfill: bool,
}

pub fn run(wiki: &Wiki, args: &TopicsArgs) -> Result<Value, EngineError> {
    if args.backfill {
        return topics::backfill(wiki);
    }
    let reg = topics::load(wiki)?;
    let mut claim_counts: std::collections::BTreeMap<String, i64> =
        std::collections::BTreeMap::new();
    for c in claims::list(wiki)? {
        *claim_counts
            .entry(c["subject"].as_str().unwrap_or("").to_string())
            .or_insert(0) += 1;
    }
    let mut out: Vec<Value> = Vec::with_capacity(reg.topics.len());
    for (s, e) in &reg.topics {
        out.push(json!({
            "subject": s,
            "kind": e.get("kind").cloned().unwrap_or(Value::Null),
            "status": e.get("status").cloned().unwrap_or(Value::Null),
            "title": e.get("title").cloned().unwrap_or(Value::Null),
            "claim_count": claim_counts.get(s).copied().unwrap_or(0),
            "declared_by": e
                .get("declared_by")
                .and_then(Value::as_array)
                .map(|a| a.len() as i64)
                .unwrap_or(0),
            "auto_registered": e
                .get("auto_registered")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }));
    }
    // reg.topics is a BTreeMap — already sorted by subject.
    Ok(json!({"version": reg.version, "topics": out}))
}
