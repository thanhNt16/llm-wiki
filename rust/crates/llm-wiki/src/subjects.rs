//! `subjects` rollup — port of `wikicore/query.py:391-425` (`query.subjects`).
//!
//! Read-only per-subject rollup for `wiki.py subjects`: predicate set,
//! status counts, distinct evidence sources, and stale derived pages.
//! Stale pages attribute to their declared `subject` first, then fall back
//! to the subject of the first dep id in their front matter; sorted by
//! subject, bounded output.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::{json, Value};

use crate::claims;
use crate::err::EngineError;
use crate::frontmatter::{self, PAGE_KINDS};
use crate::store::Wiki;

/// query.py:27-28 — output bounds.
const SUBJECTS_LIMIT: usize = 100;
const PAGE_BOUND: usize = 20;

/// `query.subjects(wiki, prefix)` — grouped rollup rows.
pub fn subjects(wiki: &Wiki, prefix: Option<&str>) -> Result<Vec<Value>, EngineError> {
    let mut grouped: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut claim_subject: HashMap<String, String> = HashMap::new();
    for c in claims::list(wiki)? {
        let subject = c["subject"].as_str().unwrap_or("").to_string();
        let id = c["id"].as_str().unwrap_or("").to_string();
        grouped.entry(subject.clone()).or_default().push(c);
        claim_subject.insert(id, subject);
    }

    // Stale derived pages: subject attribution falls back to the first
    // deps[] entry whose bare id names a known claim (query.py:417-423).
    let mut stale_pages: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for kind in PAGE_KINDS {
        let pdir = wiki.p("wiki").join(kind);
        if !pdir.is_dir() {
            continue;
        }
        let mut names: Vec<String> = Vec::new();
        for e in std::fs::read_dir(&pdir)? {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            if name.ends_with(".md") && name != "index.md" {
                names.push(name);
            }
        }
        names.sort();
        for name in names {
            let rel = format!("wiki/{kind}/{name}");
            let text = match std::fs::read_to_string(wiki.p(&rel)) {
                Ok(t) => t,
                Err(_) => continue, // OSError parity
            };
            let (front, _body) = frontmatter::split_front(&text)?;
            if !frontmatter::truthy(front.get("stale")) {
                continue;
            }
            let mut subject = front.get("subject").and_then(Value::as_str);
            if !subject
                .map(|s| grouped.contains_key(s))
                .unwrap_or(false)
            {
                subject = front
                    .get("deps")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .map(|d| d.split('@').next().unwrap_or(""))
                    .find_map(|id| claim_subject.get(id))
                    .map(String::as_str);
            }
            if let Some(s) = subject {
                if grouped.contains_key(s) {
                    stale_pages
                        .entry(s.to_string())
                        .or_default()
                        .push(rel.clone());
                }
            }
        }
    }

    let mut out: Vec<Value> = Vec::new();
    for (subject, cs) in &grouped {
        if let Some(p) = prefix {
            if !subject.starts_with(p) {
                continue;
            }
        }
        let mut statuses = serde_json::Map::new();
        for c in cs {
            let st = c["status"].as_str().unwrap_or("");
            let entry = statuses.entry(st.to_string()).or_insert(json!(0));
            *entry = json!(entry.as_i64().unwrap_or(0) + 1);
        }
        let predicates: BTreeSet<String> = cs
            .iter()
            .map(|c| c["predicate"].as_str().unwrap_or("").to_string())
            .collect();
        let mut sources = BTreeSet::new();
        for c in cs {
            for ev in c
                .get("evidence")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(sid) = ev.get("source_id").and_then(Value::as_str) {
                    if !sid.is_empty() {
                        sources.insert(sid.to_string());
                    }
                }
            }
        }
        let mut stale: Vec<String> =
            stale_pages.get(subject).cloned().unwrap_or_default();
        stale.sort();
        stale.truncate(PAGE_BOUND);
        out.push(json!({
            "subject": subject,
            "predicates": predicates.into_iter().collect::<Vec<_>>(),
            "statuses": statuses,
            "claim_count": cs.len() as i64,
            "source_count": sources.len() as i64,
            "stale_pages": stale,
        }));
        if out.len() >= SUBJECTS_LIMIT {
            break;
        }
    }
    Ok(out)
}
