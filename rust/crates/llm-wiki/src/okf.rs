//! Deterministic OKF v0.2 export — port of `wikicore/okf.py`.
//!
//! Model-free and byte-stable for a given store: every timestamp comes from
//! claim objects (max recorded_at/updated_at), never the wall clock. Layout
//! mirrors the bundles openwiki's visualize consumed: root index.md
//! declaring okf_version, concepts/ pages with `type: concept` front
//! matter, `.claims/` provenance sidecars, relative internal links.
//!
//! Front matter is hand-emitted (double-quoted okf_version, nested
//! generated block): yamlite cannot express nesting.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{json, Value};

use crate::claims;
use crate::err::EngineError;
use crate::frontmatter::dump_scalar_str;
use crate::store::Wiki;
use crate::util::{atomic_replace_dir, collect_files, make_tmp_dir, py_str, TmpDir};

/// okf.py:23 — `](<rel>.md)` markdown links inside concept pages.
static LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\]\(([^)#]+\.md)\)").unwrap());

/// `_slug` — subject → concept filename stem.
fn slug(subject: &str) -> String {
    subject
        .trim()
        .to_lowercase()
        .replace('/', "-")
        .replace('.', "-")
        .replace(' ', "-")
}

/// `_source_ids` — sorted distinct source ids cited by the claims'
/// evidence (dict refs use `source_id`; bare entries stringify).
fn source_ids<'a, I: Iterator<Item = &'a Value>>(cs: I) -> Vec<String> {
    let mut out = BTreeSet::new();
    for c in cs {
        for e in c
            .get("evidence")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let sid = if e.is_object() {
                e.get("source_id")
                    .and_then(Value::as_str)
                    .map(|s| s.to_string())
            } else {
                Some(py_str(e))
            };
            if let Some(s) = sid {
                if !s.is_empty() {
                    out.insert(s);
                }
            }
        }
    }
    out.into_iter().collect()
}

/// `_generated_at` — export clock = latest claim write (manifest carries
/// no timestamp); content-derived, never wall clock.
fn generated_at(all_claims: &[Value]) -> String {
    // `c.get("updated_at") or c.get("recorded_at")`, falsy values dropped
    // (`[t for t in ts if t]`) — max() of the survivors, epoch fallback.
    let mut ts: Vec<String> = Vec::new();
    for c in all_claims {
        for key in ["updated_at", "recorded_at"] {
            if let Some(t) = c.get(key).and_then(Value::as_str) {
                if !t.is_empty() {
                    ts.push(t.to_string());
                    break;
                }
            }
        }
    }
    ts.into_iter()
        .max()
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

/// Front-matter value shapes `_front_matter` emits.
enum Fm<'a> {
    /// scalar string (dump_scalar quoting; `"0.2"`-style passes verbatim)
    Str(&'a str),
    /// list of scalars (items emitted verbatim — okf.py's `%s` item style)
    List(Vec<String>),
    /// one-level nested mapping, insertion ordered
    Map(Vec<(&'a str, String)>),
}

/// `_front_matter` — flat scalars, scalar lists, one-level nested mappings.
fn front_matter(pairs: &[(&str, Fm<'_>)]) -> String {
    let mut lines: Vec<String> = Vec::new();
    for (key, value) in pairs {
        match value {
            Fm::Map(items) => {
                lines.push(format!("{key}:"));
                for (k, v) in items {
                    lines.push(format!("  {k}: {v}"));
                }
            }
            Fm::List(items) => {
                if items.is_empty() {
                    lines.push(format!("{key}: []"));
                } else {
                    lines.push(format!("{key}:"));
                    for item in items {
                        lines.push(format!("  - {item}"));
                    }
                }
            }
            Fm::Str(v) => {
                // '"0.2"'-style values pass through verbatim (fixture
                // shape); everything else reuses yamlite's quoting rules
                let rendered = if v.starts_with('"') {
                    v.to_string()
                } else {
                    dump_scalar_str(v)
                };
                lines.push(format!("{key}: {rendered}"));
            }
        }
    }
    lines.join("\n")
}

/// `_subject_of` — subject owning claim id `cid` (linear scan, grouped map).
fn subject_of<'a>(by_subject: &'a BTreeMap<String, Vec<Value>>, cid: &str) -> Option<&'a str> {
    for (subject, cs) in by_subject {
        if cs.iter().any(|c| c["id"].as_str().unwrap_or("") == cid) {
            return Some(subject);
        }
    }
    None
}

/// `okf.export(wiki, out_dir)` → receipt `{"pages","links","bytes"}`.
pub fn export(wiki: &Wiki, out_dir: &Path) -> Result<Value, EngineError> {
    let all_claims = claims::list(wiki)?;
    if all_claims.is_empty() {
        return Err(EngineError::Txn(
            "wiki has no claims: nothing to export".to_string(),
        ));
    }
    let tmp = make_tmp_dir("okf-export-", out_dir.parent().unwrap_or(Path::new(".")))?;
    let _guard = TmpDir(tmp.clone());
    fs::create_dir_all(tmp.join("concepts"))?;
    fs::create_dir_all(tmp.join(".claims"))?;

    let mut by_subject: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for c in &all_claims {
        by_subject
            .entry(c["subject"].as_str().unwrap_or("").to_string())
            .or_default()
            .push(c.clone());
    }
    let slugs: BTreeSet<String> = by_subject.keys().map(|s| slug(s)).collect();
    let generated = vec![
        ("by", "llm-wiki".to_string()),
        ("at", generated_at(&all_claims)),
    ];
    let mut links = 0i64;
    let mut index_links: Vec<String> = Vec::new();
    for (subject, cs) in &by_subject {
        let slug_s = slug(subject);
        let mut sorted_cs: Vec<&Value> = cs.iter().collect();
        sorted_cs.sort_by(|a, b| a["id"].as_str().unwrap_or("").cmp(b["id"].as_str().unwrap_or("")));
        let mut body = vec![format!("# {subject}"), String::new()];
        for c in &sorted_cs {
            body.push(format!(
                "- `{}` = **{}** — status: {}, version: {}",
                c["predicate"].as_str().unwrap_or(""),
                py_str(c.get("value").unwrap_or(&Value::Null)),
                c["status"].as_str().unwrap_or(""),
                py_str(&c.get("version").cloned().unwrap_or(json!(1)))
            ));
        }
        // supersession cross-links between exported concepts
        for c in &sorted_cs {
            let mut refs: Vec<Value> = Vec::new();
            if let Some(r) = c.get("superseded_by") {
                refs.push(r.clone());
            }
            if let Some(arr) = c.get("supersedes").and_then(Value::as_array) {
                refs.extend(arr.iter().cloned());
            }
            for r in refs {
                if !crate::frontmatter::truthy(Some(&r)) {
                    continue;
                }
                let rstr = match r.as_str() {
                    Some(s) => s.to_string(),
                    None => continue,
                };
                let mut target = rstr.clone();
                if rstr.starts_with("claim_") {
                    match subject_of(&by_subject, &rstr) {
                        Some(subj) => target = subj.to_string(),
                        None => continue,
                    }
                }
                let tgt = slug(&target);
                if slugs.contains(&tgt) && tgt != slug_s {
                    body.push(format!("- supersedes chain: [{tgt}]({tgt}.md)"));
                    links += 1;
                }
            }
        }
        let sources = source_ids(sorted_cs.iter().copied());
        let front = front_matter(&[
            ("type", Fm::Str("concept")),
            ("title", Fm::Str(subject)),
            ("generated", Fm::Map(generated.clone())),
            ("sources", Fm::List(sources)),
        ]);
        fs::write(
            tmp.join("concepts").join(format!("{slug_s}.md")),
            format!("---\n{front}\n---\n\n{}\n", body.join("\n")),
        )?;
        let sidecar: Vec<Value> = sorted_cs
            .iter()
            .map(|c| {
                json!({
                    "id": c["id"],
                    "predicate": c["predicate"],
                    "value": c.get("value").cloned().unwrap_or(Value::Null),
                    "status": c["status"],
                    "sources": source_ids(std::iter::once(*c)),
                    "valid_from": c.get("valid_from").cloned().unwrap_or(Value::Null),
                    "valid_to": c.get("valid_to").cloned().unwrap_or(Value::Null),
                })
            })
            .collect();
        fs::write(
            tmp.join(".claims").join(format!("{slug_s}.json")),
            format!("{}\n", serde_json::to_string_pretty(&sidecar)?),
        )?;
        index_links.push(format!("- [{subject}](concepts/{slug_s}.md)"));
    }
    let root_front = front_matter(&[
        ("okf_version", Fm::Str("\"0.2\"")),
        ("type", Fm::Str("index")),
        ("generated", Fm::Map(generated)),
    ]);
    index_links.sort();
    fs::write(
        tmp.join("index.md"),
        format!(
            "---\n{root_front}\n---\n\n# Wiki index\n\n{}\n",
            index_links.join("\n")
        ),
    )?;

    // link-validation pass: an orphan link means the export lied
    let concepts_dir = tmp.join("concepts");
    let mut files: Vec<String> = Vec::new();
    for e in fs::read_dir(&concepts_dir)? {
        files.push(e?.file_name().to_string_lossy().into_owned());
    }
    files.sort();
    for fname in files {
        let text = fs::read_to_string(concepts_dir.join(&fname))?;
        for cap in LINK_RE.captures_iter(&text) {
            let target = &cap[1];
            if !concepts_dir.join(target).is_file() {
                return Err(EngineError::Txn(format!(
                    "orphan link in {fname}: {target}"
                )));
            }
        }
    }

    atomic_replace_dir(&tmp, out_dir)?;
    let files = collect_files(out_dir)?;
    let bytes: u64 = files
        .iter()
        .filter_map(|p| fs::metadata(p).ok().map(|m| m.len()))
        .sum();
    Ok(json!({
        "pages": by_subject.len() as i64,
        "links": links,
        "bytes": bytes as i64,
    }))
}
