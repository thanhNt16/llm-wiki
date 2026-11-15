//! Context packs: the smallest useful context for a specific task, under a
//! hard token budget (PRD §19). Everything here is derived from committed
//! state; a pack is itself a registered artifact so staleness propagates
//! (PRD §17). Port of `wikicore/contextpack.py` verbatim.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{json, Value};

use crate::err::EngineError;
use crate::frontmatter::{self, dump_scalar_str};
use crate::store::Wiki;
use crate::txn::{Txn, TxnOpts};
use crate::util::{now_iso, py_str};
use crate::{claims, deps, ids};

fn stopwords() -> &'static HashSet<&'static str> {
    static STOPWORDS: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
        "a an and are as at be but by for from has have how i in is it its of on or
that the this to was we what when where which who why will with you our us do does did"
            .split_whitespace()
            .collect()
    });
    &STOPWORDS
}

const PAGE_LIMIT: usize = 10;
const SOURCE_LIMIT: usize = 10;
// Pack frontmatter deps and receipt sources are bounded to keep receipts
// readable, but the bound is deliberately high; any truncation is recorded
// explicitly in the receipt instead of dropped silently.
const DEPS_LIMIT: usize = 500;
const RECEIPT_SOURCES_LIMIT: usize = 500;

// Loose ISO-8601 "YYYY-MM-DD[ T]HH:MM[:SS]" recognizer for --changes-since;
// the trailing "Z"/offset is tolerated and dropped (wiki timestamps are UTC).
static TS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"^\d{4}-\d{2}-\d{2}(?:[T ]\d{2}:\d{2}(?::\d{2})?(?:\.\d+)?(?:Z|[+-]\d{2}:?\d{2})?)?$",
    )
    .unwrap()
});

/// `contextpack._norm_since_ts` — normalize an ISO-8601 timestamp for
/// lexicographic comparison with operations' finished_at
/// ("%Y-%m-%dT%H:%M:%SZ"). Returns None when the value is a context id or
/// anything unrecognized.
fn norm_since_ts(value: &str) -> Option<String> {
    let v = value.trim();
    if !TS_RE.is_match(v) {
        return None;
    }
    let date = &v[..10];
    if v.len() == 10 {
        return Some(date.to_string()); // date-only: ops on that date compare after it
    }
    let mut time_part = v[11..19.min(v.len())].to_string();
    if time_part.len() < 8 {
        time_part.push_str(&":00:00"[..8 - time_part.len()]);
    }
    Some(format!("{date}T{time_part}Z"))
}

/// `contextpack._est` — estimated tokens: ceil(chars / 4), minimum 1.
/// Python `len()` counts code points, so `chars().count()` not bytes.
fn est(text: &str) -> i64 {
    let n = text.chars().count() as i64;
    ((n + 3) / 4).max(1)
}

/// `contextpack._fmt_claim`.
fn fmt_claim(c: &Value) -> String {
    let scope = c
        .get("scope")
        .and_then(Value::as_object)
        .map(|o| {
            let mut kvs: Vec<(&String, &Value)> = o.iter().collect();
            kvs.sort_by(|a, b| a.0.cmp(b.0));
            kvs.iter()
                .map(|(k, v)| format!("{k}={}", py_str(v)))
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default();
    let mut parts = format!(
        "- {} / {} = {}",
        py_str(c.get("subject").unwrap_or(&Value::Null)),
        py_str(c.get("predicate").unwrap_or(&Value::Null)),
        py_str(c.get("value").unwrap_or(&Value::Null)),
    );
    if let Some(unit) = c.get("unit") {
        if frontmatter::truthy(Some(unit)) {
            parts.push_str(&format!(" {}", py_str(unit)));
        }
    }
    if !scope.is_empty() {
        parts.push_str(&format!(" [scope: {scope}]"));
    }
    if frontmatter::truthy(c.get("valid_from")) {
        parts.push_str(&format!(" (valid from {}", py_str(&c["valid_from"])));
        if frontmatter::truthy(c.get("valid_to")) {
            parts.push_str(&format!(", to {})", py_str(&c["valid_to"])));
        } else {
            parts.push(')');
        }
    }
    let ev = c
        .get("evidence")
        .and_then(Value::as_array)
        .and_then(|a| a.first());
    if let Some(ev) = ev {
        let sid = py_str(ev.get("source_id").unwrap_or(&Value::Null));
        let sid = &sid[..sid.char_indices().nth(18).map(|(i, _)| i).unwrap_or(sid.len())];
        let locator = ev.get("locator").cloned().unwrap_or(Value::Null);
        let value = locator.get("value").cloned().unwrap_or(Value::Null);
        let lstr = if frontmatter::truthy(Some(&value)) {
            py_str(&value)
        } else {
            py_str(locator.get("type").unwrap_or(&Value::Null))
        };
        parts.push_str(&format!(
            " — evidence: {}@{} {}",
            sid,
            py_str(ev.get("source_version").unwrap_or(&Value::Null)),
            lstr
        ));
    }
    parts
}

/// `text.norm_tokens` — lowercase, split on non-alnum, drop tokens < 3
/// chars, strip one conservative trailing suffix ('s' leaves ≥3, others ≥4).
static TOKEN_SPLIT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^0-9a-z]+").unwrap());
const TOKEN_SUFFIXES: &[&str] = &["tion", "ment", "ing", "ed", "ly", "s"];

fn norm_tokens(s: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    for tok in TOKEN_SPLIT_RE.split(&s.to_lowercase()) {
        if tok.len() < 3 {
            continue;
        }
        let mut t = tok.to_string();
        for suf in TOKEN_SUFFIXES {
            if t.ends_with(suf)
                && t.len() - suf.len() >= (if *suf == "s" { 3 } else { 4 })
            {
                t.truncate(t.len() - suf.len());
                break;
            }
        }
        out.insert(t);
    }
    out
}

/// `(title, lines, claim_ids, other_deps)` — one candidate pack section in
/// PRD §19 priority order.
type Section = (String, Vec<String>, Vec<String>, Vec<String>);

/// `(title, reason)` markers rendered under "## Omitted sections".
type Omitted = Vec<(String, String)>;

/// `contextpack._sections` — candidate sections in PRD §19 priority order.
/// Sections whose every item is off-topic for a narrow task are dropped and
/// reported as (title, "off-topic") in the returned omitted list (Q5).
fn sections(wiki: &Wiki, task: &str) -> Result<(Vec<Section>, Omitted), EngineError> {
    let cfg = wiki.config()?;
    let policy = &cfg["acceptance_policy"];
    let mut ttask = norm_tokens(task);
    ttask.retain(|t| !stopwords().contains(t.as_str()));
    let narrow = !ttask.is_empty();

    let hits = |text: &str| -> usize {
        if !narrow {
            return 0;
        }
        ttask.intersection(&norm_tokens(&text.replace('_', " "))).count()
    };

    let filtered = |title: &str,
                    items: Vec<Value>,
                    key: &dyn Fn(&Value) -> String,
                    omitted: &mut Omitted|
     -> Vec<Value> {
        if !narrow {
            return items;
        }
        let kept: Vec<Value> = items
            .iter()
            .filter(|x| hits(&key(x)) > 0)
            .cloned()
            .collect();
        if !items.is_empty() && kept.is_empty() {
            omitted.push((title.to_string(), "off-topic".to_string()));
        }
        kept
    };

    let ctext = |c: &Value| -> String {
        format!(
            "{} {} {}",
            py_str(c.get("subject").unwrap_or(&Value::Null)),
            py_str(c.get("predicate").unwrap_or(&Value::Null)),
            py_str(c.get("value").unwrap_or(&Value::Null))
        )
    };

    let mut omitted: Omitted = Vec::new();
    let all_claims = claims::list(wiki)?;
    // auto-materialized stubs are claim-derived placeholders, not authored
    // decisions — the claim itself already surfaces under Relevant claims
    let decisions: Vec<Value> = claims::list_decisions(wiki)?
        .into_iter()
        .filter(|d| !frontmatter::truthy(d.get("auto_materialized")))
        .collect();
    let review = wiki.read_jsonl(".state/review-queue.jsonl")?;
    let open_items: Vec<Value> = review
        .into_iter()
        .filter(|r| r.get("status").and_then(Value::as_str) == Some("open"))
        .collect();

    let mut out: Vec<Section> = Vec::new();

    let auto: HashSet<&str> = policy["auto_accept_authority"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let constraints: Vec<Value> = all_claims
        .iter()
        .filter(|c| {
            c.get("status").and_then(Value::as_str) == Some("accepted")
                && auto.contains(
                    c.get("authority")
                        .and_then(|a| a.get("type"))
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                )
        })
        .cloned()
        .collect();
    let constraints = filtered("Critical constraints", constraints, &ctext, &mut omitted);
    if !constraints.is_empty() {
        out.push((
            "Critical constraints".to_string(),
            constraints.iter().map(fmt_claim).collect(),
            constraints
                .iter()
                .map(|c| py_str(c.get("id").unwrap_or(&Value::Null)))
                .collect(),
            vec![],
        ));
    }

    let decisions = filtered(
        "Accepted decisions",
        decisions,
        &|d: &Value| {
            format!(
                "{} {}",
                py_str(d.get("title").unwrap_or(&Value::Null)),
                d.get("body").map(py_str).unwrap_or_default()
            )
        },
        &mut omitted,
    );
    if !decisions.is_empty() {
        let mut lines = Vec::new();
        for d in &decisions {
            let body = d.get("body").and_then(Value::as_str).unwrap_or("");
            let title = py_str(d.get("title").unwrap_or(&Value::Null));
            let first = if frontmatter::truthy(d.get("body")) {
                body.trim().lines().next().unwrap_or("").to_string()
            } else {
                title.clone()
            };
            lines.push(format!("- {title}: {first}"));
        }
        out.push((
            "Accepted decisions".to_string(),
            lines,
            vec![],
            decisions
                .iter()
                .map(|d| py_str(d.get("id").unwrap_or(&Value::Null)))
                .collect(),
        ));
    }

    let mut relevant: Vec<&Value> = all_claims
        .iter()
        .filter(|c| {
            matches!(
                c.get("status").and_then(Value::as_str),
                Some("accepted") | Some("provisional") | Some("disputed") | Some("candidate")
            )
        })
        .collect();
    let scored: Vec<(usize, bool)> = relevant
        .iter()
        .map(|c| {
            (
                hits(&ctext(c)),
                c.get("status").and_then(Value::as_str) == Some("accepted"),
            )
        })
        .collect();
    // stable sort desc on (hits, accepted) — Python sort(key, reverse=True)
    // keeps original file order inside equal keys.
    let mut order: Vec<usize> = (0..relevant.len()).collect();
    order.sort_by(|&a, &b| scored[b].cmp(&scored[a]));
    relevant = order.into_iter().map(|i| relevant[i]).collect();
    let relevant: Vec<Value> = relevant
        .into_iter()
        .filter(|c| hits(&ctext(c)) > 0 || !narrow)
        .take(15)
        .cloned()
        .collect();
    if !relevant.is_empty() {
        out.push((
            "Relevant claims".to_string(),
            relevant.iter().map(fmt_claim).collect(),
            relevant
                .iter()
                .map(|c| py_str(c.get("id").unwrap_or(&Value::Null)))
                .collect(),
            vec![],
        ));
    }

    let impls: Vec<Value> = all_claims
        .iter()
        .filter(|c| {
            frontmatter::truthy(c.get("evidence"))
                && c.get("evidence")
                    .and_then(Value::as_array)
                    .and_then(|a| a.first())
                    .and_then(|e| e.get("locator"))
                    .and_then(|l| l.get("type"))
                    .and_then(Value::as_str)
                    == Some("repo_path")
        })
        .cloned()
        .collect();
    let impls = filtered("Implementation references", impls, &ctext, &mut omitted);
    if !impls.is_empty() {
        out.push((
            "Implementation references".to_string(),
            impls.iter().map(fmt_claim).collect(),
            impls.iter()
                .map(|c| py_str(c.get("id").unwrap_or(&Value::Null)))
                .collect(),
            vec![],
        ));
    }

    let open_items = filtered(
        "Unresolved conflicts",
        open_items,
        &|r: &Value| r.get("problem").map(py_str).unwrap_or_default(),
        &mut omitted,
    );
    if !open_items.is_empty() {
        let lines: Vec<String> = open_items
            .iter()
            .map(|r| {
                let affected = r
                    .get("affected")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().map(py_str).collect::<Vec<_>>().join(", "))
                    .unwrap_or_default();
                format!(
                    "- [{}] {} (affected: {})",
                    py_str(r.get("kind").unwrap_or(&Value::Null)),
                    py_str(r.get("problem").unwrap_or(&Value::Null)),
                    affected
                )
            })
            .collect();
        out.push((
            "Unresolved conflicts".to_string(),
            lines,
            vec![],
            open_items
                .iter()
                .map(|r| py_str(r.get("id").unwrap_or(&Value::Null)))
                .collect(),
        ));
    }

    // Related concepts: relevance-scored on title + body + subject tokens,
    // not filename only (Q5).
    let mut scored_pages: Vec<(usize, String)> = Vec::new();
    let cdir = wiki.p("wiki/concepts");
    if cdir.is_dir() {
        let mut names: Vec<String> = Vec::new();
        for e in std::fs::read_dir(&cdir)? {
            let e = e?;
            names.push(e.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        for name in names {
            if !name.ends_with(".md") {
                continue;
            }
            let rel = format!("wiki/concepts/{name}");
            let text = match std::fs::read_to_string(wiki.p(&rel)) {
                Ok(t) => t,
                Err(_) => continue, // Python `except OSError: continue`
            };
            let (front, body) = frontmatter::split_front(&text)?;
            let title = front
                .get("title")
                .filter(|v| frontmatter::truthy(Some(v)))
                .map(py_str)
                .unwrap_or_else(|| name[..name.len() - 3].to_string());
            let subject = front
                .get("subject")
                .map(py_str)
                .unwrap_or_default();
            let score = hits(&format!("{title} {subject} {body}"));
            if narrow && score == 0 {
                continue;
            }
            scored_pages.push((score, rel));
        }
    }
    scored_pages.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
    let concept_pages: Vec<String> =
        scored_pages.iter().take(PAGE_LIMIT).map(|(_, r)| r.clone()).collect();
    if !concept_pages.is_empty() {
        out.push((
            "Related concepts".to_string(),
            concept_pages.iter().map(|p| format!("- {p}")).collect(),
            vec![],
            concept_pages,
        ));
    }

    let srcs_dir = wiki.p("sources");
    if srcs_dir.is_dir() {
        let mut names: Vec<String> = Vec::new();
        for e in std::fs::read_dir(&srcs_dir)? {
            let e = e?;
            names.push(e.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        let mut manifests: Vec<Value> = Vec::new();
        for sid in names.into_iter().take(SOURCE_LIMIT) {
            if wiki.p(&format!("sources/{sid}/manifest.json")).is_file() {
                manifests.push(wiki.load_json(&format!("sources/{sid}/manifest.json"))?);
            }
        }
        let manifests = filtered(
            "Supplementary evidence",
            manifests,
            &|m: &Value| m.get("title").map(py_str).unwrap_or_default(),
            &mut omitted,
        );
        let names: Vec<String> = manifests
            .iter()
            .map(|m| {
                // m.get("title", m["id"]) — absent title falls back to id;
                // present-but-null/empty title passes through verbatim.
                let title = m
                    .get("title")
                    .map(py_str)
                    .unwrap_or_else(|| py_str(m.get("id").unwrap_or(&Value::Null)));

                format!(
                    "- {} ({}, {} version(s))",
                    title,
                    py_str(m.get("kind").unwrap_or(&Value::Null)),
                    m.get("versions").and_then(Value::as_array).map_or(0, Vec::len)
                )
            })
            .collect();
        if !names.is_empty() {
            out.push((
                "Supplementary evidence".to_string(),
                names,
                vec![],
                vec![],
            ));
        }
    }
    Ok((out, omitted))
}

/// `contextpack._claim_version_map`.
fn claim_version_map(wiki: &Wiki) -> Result<BTreeMap<String, i64>, EngineError> {
    let mut out = BTreeMap::new();
    for c in claims::list(wiki)? {
        out.insert(
            py_str(c.get("id").unwrap_or(&Value::Null)),
            c.get("version").and_then(Value::as_i64).unwrap_or(1),
        );
    }
    for d in claims::list_decisions(wiki)? {
        out.insert(
            py_str(d.get("id").unwrap_or(&Value::Null)),
            d.get("version").and_then(Value::as_i64).unwrap_or(1),
        );
    }
    Ok(out)
}

/// Python dict-order `yamlite.dumps({"type","context","deps"})` — Python
/// emits keys in insertion order; serde_json::Map sorts, so the front is
/// rendered by hand to keep pack bytes identical.
fn front_yaml(context_id: &str, deps_list: &[String]) -> String {
    let mut lines = vec![
        "type: context-pack".to_string(),
        format!("context: {}", dump_scalar_str(context_id)),
    ];
    if deps_list.is_empty() {
        lines.push("deps: []".to_string());
    } else {
        lines.push("deps:".to_string());
        for d in deps_list {
            lines.push(format!("  - {}", dump_scalar_str(d)));
        }
    }
    lines.join("\n")
}

/// `contextpack.build` — assemble + commit a pack and its receipt.
pub fn build(
    wiki: &Wiki,
    task: &str,
    budget: Option<i64>,
    resume: bool,
    changes_since: Option<&str>,
) -> Result<Value, EngineError> {
    if !wiki.exists() {
        return Err(EngineError::Txn("wiki not initialized".to_string()));
    }
    let cfg = wiki.config()?;
    let budget = match budget {
        None => cfg["context_budget_default"]
            .as_i64()
            .or_else(|| cfg["context_budget_default"].as_f64().map(|f| f as i64))
            .unwrap_or(0),
        Some(b) => b,
    };
    if budget <= 0 {
        // 0 is an explicit (invalid) budget, not "unset": refuse rather than
        // silently fall back to the configured default
        return Err(EngineError::Txn(format!(
            "budget must be a positive token count (got {budget})"
        )));
    }
    let context_id = ids::new("context");
    let versions = claim_version_map(wiki)?;

    let task_label = if !task.is_empty() {
        task.to_string()
    } else if resume {
        "resume briefing".to_string()
    } else {
        "change briefing".to_string()
    };

    let mut refr: Option<Value> = None;
    let mut omitted: Omitted;
    let blocks: Vec<Section>;
    if resume || changes_since.is_some_and(|s| !s.is_empty()) {
        let (r, briefing) = briefing(wiki, changes_since)?;
        refr = Some(r);
        blocks = vec![(
            "Change briefing".to_string(),
            briefing.0,
            briefing.1,
            vec![],
        )];
        omitted = Vec::new();
    } else {
        let (b, o) = sections(wiki, task)?;
        blocks = b;
        omitted = o;
    }

    let evidence_ref = refr
        .as_ref()
        .map(|r| py_str(r.get("context_id").unwrap_or(&Value::Null)))
        .unwrap_or_else(|| "current state".to_string());
    let header = vec![
        format!("# Context pack {context_id}"),
        String::new(),
        format!("- Task: {task_label}"),
        format!("- Generated: {}", now_iso()),
        format!("- Budget: {budget} (estimated tokens, hard)"),
        format!("- Evidence reference: {evidence_ref}"),
        String::new(),
    ];

    // Greedy inclusion in priority order, then drop-from-end until the FULL
    // rendered file fits the budget (two-pass hard gate). Sections dropped
    // for budget — like sections filtered as off-topic — are recorded as
    // omitted markers so the agent knows what it did not see (Q5).
    let mut included = blocks;
    let (pack_text, claim_ids, other_deps, deps_list, deps_truncated) = loop {
        let mut body = header.clone();
        let mut claim_ids_l: Vec<String> = Vec::new();
        let mut other_deps_l: Vec<String> = Vec::new();
        for (title, lines, cids, others) in &included {
            body.push(format!("## {title}"));
            body.push(String::new());
            body.extend(lines.iter().cloned());
            body.push(String::new());
            claim_ids_l.extend(cids.iter().cloned());
            other_deps_l.extend(others.iter().cloned());
        }
        if !omitted.is_empty() {
            body.push("## Omitted sections".to_string());
            body.push(String::new());
            for (t, reason) in &omitted {
                body.push(format!("- {t} ({reason})"));
            }
            body.push(String::new());
        }
        let mut seen = HashSet::new();
        claim_ids_l.retain(|c| seen.insert(c.clone()));
        let mut deps_l: Vec<String> = claim_ids_l
            .iter()
            .map(|c| format!("{}@{}", c, versions.get(c).copied().unwrap_or(1)))
            .collect();
        for d in &other_deps_l {
            if d.contains('@') {
                deps_l.push(d.clone());
            } else {
                deps_l.push(format!("{}@{}", d, versions.get(d).copied().unwrap_or(1)));
            }
        }
        let mut seen2 = HashSet::new();
        deps_l.retain(|d| seen2.insert(d.clone()));
        let truncated = deps_l.len().saturating_sub(DEPS_LIMIT);
        let front = front_yaml(&context_id, &deps_l[..deps_l.len().min(DEPS_LIMIT)]);
        let text = format!("---\n{front}\n---\n\n{}\n", body.join("\n"));
        if est(&text) <= budget || included.is_empty() {
            break (text, claim_ids_l, other_deps_l, deps_l, truncated);
        }
        let dropped = included.pop().unwrap(); // lowest-priority section dropped; try again
        omitted.push((dropped.0, "budget".to_string()));
    };

    let est = est(&pack_text);
    if est > budget {
        return Err(EngineError::Txn(format!(
            "cannot fit context pack in budget {budget} (minimum required is \
             {est} estimated tokens)"
        )));
    }

    let mut txn = Txn::begin_with(
        wiki,
        &ids::new_run(),
        TxnOpts {
            command: Some("wiki-context".to_string()),
            ..Default::default()
        },
    )?;
    let pack_rel = format!("context/{context_id}.md");
    txn.stage_write(&pack_rel, pack_text.as_bytes())?;

    let id_set: HashSet<&str> = claim_ids.iter().map(String::as_str).collect();
    let mut src_ids: BTreeSet<String> = BTreeSet::new();
    for c in claims::list(wiki)? {
        if !id_set.contains(py_str(c.get("id").unwrap_or(&Value::Null)).as_str()) {
            continue;
        }
        for ev in c.get("evidence").and_then(Value::as_array).cloned().unwrap_or_default() {
            if let Some(sid) = ev.get("source_id").and_then(Value::as_str) {
                src_ids.insert(sid.to_string());
            }
        }
    }
    let src_ids: Vec<String> = src_ids.into_iter().collect();
    let receipt = json!({
        "context_id": context_id,
        "task": task_label,
        "generated_at": now_iso(),
        "budget": {"requested": budget, "estimated": est},
        // spec context_budget gate metric (additive key, spec §6.1).
        "budget_used": est,
        "claims": claim_ids
            .iter()
            .map(|c| format!("{}@{}", c, versions.get(c).copied().unwrap_or(1)))
            .collect::<Vec<_>>(),
        "decisions": other_deps
            .iter()
            .filter(|d| d.starts_with("decision_"))
            .map(|d| format!("{}@{}", d, versions.get(d).copied().unwrap_or(1)))
            .collect::<Vec<_>>(),
        "sources": &src_ids[..src_ids.len().min(RECEIPT_SOURCES_LIMIT)],
        "sources_truncated": src_ids.len().saturating_sub(RECEIPT_SOURCES_LIMIT),
        "deps_truncated": deps_truncated,
        "omitted": omitted
            .iter()
            .map(|(t, r)| format!("{t} ({r})"))
            .collect::<Vec<_>>(),
    });
    txn.stage_state(&format!("context/{context_id}.receipt.json"), &receipt)?;
    deps::register(&mut txn, &pack_rel, &deps_list.iter().map(String::as_str).collect::<Vec<_>>())?;
    txn.changes.insert("context_packs".to_string(), json!(1));
    txn.commit()?;
    Ok(json!({
        "context_id": context_id,
        "pack_path": pack_rel,
        "receipt_path": format!("context/{context_id}.receipt.json"),
        "receipt": receipt,
    }))
}

/// `contextpack._ops_after` — second-granularity timestamps: include ops
/// committed in the same second.
fn ops_after(wiki: &Wiki, since_ts: &str) -> Result<Vec<Value>, EngineError> {
    Ok(wiki
        .read_jsonl(".state/operations.jsonl")?
        .into_iter()
        .filter(|op| {
            !since_ts.is_empty()
                && op.get("finished_at").and_then(Value::as_str).unwrap_or("") >= since_ts
        })
        .collect())
}

/// `contextpack._last_receipt` — reference point for a change briefing.
/// With a context id: the receipt recorded for exactly that pack. With an
/// ISO-8601 timestamp: a synthetic reference at that time. Unset: the most
/// recent receipt, falling back to project start.
fn last_receipt(wiki: &Wiki, before_id: Option<&str>) -> Result<Value, EngineError> {
    let ctx_dir = wiki.p("context");
    let mut receipts: Vec<Value> = Vec::new();
    if ctx_dir.is_dir() {
        let mut names: Vec<String> = Vec::new();
        for e in std::fs::read_dir(&ctx_dir)? {
            let e = e?;
            names.push(e.file_name().to_string_lossy().into_owned());
        }
        names.sort();
        for name in names {
            if !name.ends_with(".receipt.json") {
                continue;
            }
            let text = match std::fs::read_to_string(ctx_dir.join(&name)) {
                Ok(t) => t,
                Err(_) => continue,
            };
            let r: Value = match serde_json::from_str(&text) {
                Ok(v) => v,
                Err(_) => continue,
            };
            if r.get("context_id").is_some() && r.get("generated_at").is_some() {
                receipts.push(r);
            }
        }
    }
    if let Some(before) = before_id {
        if !before.is_empty() {
            // the requested pack's own receipt is the intended diff base
            if let Some(m) = receipts
                .iter()
                .find(|r| r.get("context_id").and_then(Value::as_str) == Some(before))
            {
                return Ok(m.clone());
            }
            if let Some(ts) = norm_since_ts(before) {
                return Ok(json!({"generated_at": ts, "context_id": before}));
            }
            // unrecognized reference: diff everything rather than silently guess
            return Ok(json!({"generated_at": "", "context_id": before}));
        }
    }
    if !receipts.is_empty() {
        let best = receipts
            .iter()
            .max_by(|a, b| {
                let ka = (
                    a.get("generated_at").and_then(Value::as_str).unwrap_or(""),
                    a.get("context_id").and_then(Value::as_str).unwrap_or(""),
                );
                let kb = (
                    b.get("generated_at").and_then(Value::as_str).unwrap_or(""),
                    b.get("context_id").and_then(Value::as_str).unwrap_or(""),
                );
                ka.cmp(&kb)
            })
            .unwrap();
        return Ok(best.clone());
    }
    Ok(json!({"generated_at": "", "context_id": "project-start"}))
}

/// `contextpack._replacement_of` — resolve the claim that replaced a
/// superseded one. Prefer the explicit superseded_by pointer; fall back to
/// scanning supersedes lists for wikis written before that field was
/// populated. Self-references are ignored.
fn replacement_of<'a>(all_claims: &'a [Value], c: &Value) -> Option<&'a Value> {
    let cid = c.get("id").unwrap_or(&Value::Null);
    if let Some(rid) = c.get("superseded_by").and_then(Value::as_str) {
        if Some(rid) != cid.as_str() {
            if let Some(r) = all_claims
                .iter()
                .find(|s| s.get("id").and_then(Value::as_str) == Some(rid))
            {
                return Some(r);
            }
        }
    }
    all_claims.iter().find(|s| {
        s.get("id") != c.get("id")
            && s.get("supersedes")
                .and_then(Value::as_array)
                .is_some_and(|l| l.iter().any(|v| v == cid))
    })
}

/// `contextpack._briefing` — (ref, (lines, claim_ids)).
fn briefing(
    wiki: &Wiki,
    changes_since: Option<&str>,
) -> Result<(Value, (Vec<String>, Vec<String>)), EngineError> {
    let refr = last_receipt(wiki, changes_since)?;
    let since = refr.get("generated_at").and_then(Value::as_str).unwrap_or("");
    let ops = ops_after(wiki, since)?;
    let mut lines: Vec<String> = Vec::new();
    if ops.is_empty() {
        let ctx = refr
            .get("context_id")
            .and_then(Value::as_str)
            .unwrap_or("project start");
        lines.push(format!("No committed wiki operations since reference {ctx}."));
        return Ok((refr, (lines, vec![])));
    }

    let all_claims = claims::list(wiki)?;
    // commit-time (wall clock), not semantic recorded_at: fixtures pin
    // recorded_at to content dates, so use updated_at /
    // acceptance.accepted_at as commit signal
    let commit_ts = |c: &Value| -> String {
        if frontmatter::truthy(c.get("updated_at")) {
            return py_str(c.get("updated_at").unwrap());
        }
        let accepted = c.get("acceptance").and_then(|a| a.get("accepted_at"));
        if let Some(a) = accepted {
            if frontmatter::truthy(Some(a)) {
                return py_str(a);
            }
        }
        py_str(c.get("recorded_at").unwrap_or(&Value::Null))
    };
    let changed: Vec<&Value> = all_claims
        .iter()
        .filter(|c| commit_ts(c).as_str() >= since)
        .collect();
    let superseded: Vec<&Value> = all_claims
        .iter()
        .filter(|c| {
            c.get("status").and_then(Value::as_str) == Some("superseded")
                && commit_ts(c).as_str() >= since
        })
        .collect();

    lines.push(format!(
        "Operations since reference {}: {}",
        py_str(refr.get("context_id").unwrap_or(&Value::Null)),
        ops.len()
    ));
    if !changed.is_empty() {
        lines.push(String::new());
        lines.push("### New or changed claims".to_string());
        lines.extend(changed.iter().map(|c| fmt_claim(c)));
    }
    if !superseded.is_empty() {
        lines.push(String::new());
        lines.push("### Recently superseded assumptions".to_string());
        for c in &superseded {
            let repl = replacement_of(&all_claims, c);
            lines.push(format!(
                "- {} ({} / {}, was {} → now {})",
                py_str(c.get("id").unwrap_or(&Value::Null)),
                py_str(c.get("subject").unwrap_or(&Value::Null)),
                py_str(c.get("predicate").unwrap_or(&Value::Null)),
                py_str(c.get("value").unwrap_or(&Value::Null)),
                repl.map(|r| py_str(r.get("value").unwrap_or(&Value::Null)))
                    .unwrap_or_else(|| "?".to_string()),
            ));
        }
    }
    let open_items: Vec<Value> = wiki
        .read_jsonl(".state/review-queue.jsonl")?
        .into_iter()
        .filter(|r| r.get("status").and_then(Value::as_str) == Some("open"))
        .collect();
    if !open_items.is_empty() {
        lines.push(String::new());
        lines.push("### Open questions / known problems".to_string());
        lines.extend(open_items.iter().map(|r| {
            format!(
                "- [{}] {}",
                py_str(r.get("kind").unwrap_or(&Value::Null)),
                py_str(r.get("problem").unwrap_or(&Value::Null)),
            )
        }));
    }
    lines.push(String::new());
    lines.push("### Snapshot".to_string());
    lines.push(format!(
        "- claims: {}, decisions: {}",
        all_claims.len(),
        claims::list_decisions(wiki)?.len()
    ));
    let claim_ids = changed
        .iter()
        .map(|c| py_str(c.get("id").unwrap_or(&Value::Null)))
        .collect();
    Ok((refr, (lines, claim_ids)))
}
