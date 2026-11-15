//! Derived views: deterministic page builders + verification gates.
//! Port of wikicore/pages.py — every page written here is script-owned and
//! rebuildable; agent-authored pages are only ever marked stale in place.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde_json::{json, Map, Value};

use crate::claims;
use crate::deps;
use crate::err::EngineError;
use crate::frontmatter::{self, PAGE_KINDS};
use crate::ids;
use crate::review;
use crate::store::Wiki;
use crate::synthesize;
use crate::txn::{Txn, TxnOpts};
use crate::util;

/// sections swept for dep-registration in `verify` (pages.py:508 — a
/// different order/tuple than PAGE_KINDS, plus decisions).
const VERIFY_SECTIONS: &[&str] = &[
    "sources",
    "concepts",
    "entities",
    "procedures",
    "questions",
    "decisions",
    "changes",
];

fn now() -> String {
    util::now_iso()
}

/// `_page` — render a fenced frontmatter + body page.
fn page(front: &Map<String, Value>, body: &str) -> Result<String, EngineError> {
    frontmatter::page_text(front, body)
}

/// `_front` — a page's fenced frontmatter; {} when absent/unparseable.
fn front(text: &str) -> Map<String, Value> {
    frontmatter::split_front(text)
        .map(|(f, _)| f)
        .unwrap_or_default()
}

/// `_page_title` — one-line title: frontmatter title, then first `# ` heading.
/// `_front` swallows parse errors for the title probe but the body scan uses
/// `_split_front` directly — a corrupt frontmatter page propagates TxnError.
fn page_title(path: &std::path::Path) -> Result<String, EngineError> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Ok("-".to_string());
    };
    let title = front(&text)
        .get("title")
        .map(util::py_str)
        .unwrap_or_default();
    if !title.trim().is_empty() {
        return Ok(title.trim().to_string());
    }
    let (_, body) = frontmatter::split_front(&text)?;
    for line in body.lines() {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("# ") {
            return Ok(rest.trim().to_string());
        }
    }
    Ok("-".to_string())
}

/// `_section_files` — non-index .md filenames in wiki/<sub>, sorted.
fn section_files(wiki: &Wiki, sub: &str) -> Vec<String> {
    let subdir = wiki.p(&format!("wiki/{sub}"));
    if !subdir.is_dir() {
        return Vec::new();
    }
    let mut out: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&subdir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.ends_with(".md") && name != "index.md" {
                out.push(name);
            }
        }
    }
    out.sort();
    out
}

/// `_subject_pages` — subject -> artifact rel path from page frontmatter.
fn subject_pages(wiki: &Wiki) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for sub in PAGE_KINDS {
        for name in section_files(wiki, sub) {
            let rel = format!("wiki/{sub}/{name}");
            let Ok(text) = std::fs::read_to_string(wiki.p(&rel)) else {
                continue;
            };
            if let Some(subj) = front(&text).get("subject").and_then(Value::as_str) {
                out.entry(subj.to_string()).or_insert(rel);
            }
        }
    }
    out
}

/// `_affected_links` — `[[id]]` for known ids, plain text otherwise.
fn affected_links(affected: &[Value], known_ids: &HashMap<String, i64>) -> String {
    let parts: Vec<String> = affected
        .iter()
        .map(|a| {
            if a.as_str().map_or(false, |s| known_ids.contains_key(s)) {
                format!("[[{}]]", a.as_str().unwrap())
            } else {
                util::py_str(a)
            }
        })
        .collect();
    if parts.is_empty() {
        "-".to_string()
    } else {
        parts.join(", ")
    }
}

fn field_or(v: Option<&Value>, dash: &str) -> String {
    match v {
        Some(x) => util::py_str(x),
        None => dash.to_string(),
    }
}
/// `wiki.read_jsonl(review.QUEUE)` — the review queue rows.
fn read_queue(wiki: &Wiki) -> Result<Vec<Value>, EngineError> {
    wiki.read_jsonl(review::QUEUE)
}


/// `_review_queue_lines` — "## Review queue" body: open/deferred counts +
/// per-item rows (R1). `intro` is the `{}`-formatted header text.
fn review_queue_lines(wiki: &Wiki, known_ids: &HashMap<String, i64>, intro: &str) -> Result<Vec<String>, EngineError> {
    let mut items: Vec<Value> = read_queue(wiki)?
        .into_iter()
        .filter(|i| {
            let s = i.get("status").and_then(Value::as_str);
            s == Some("open") || s == Some("deferred")
        })
        .collect();
    items.sort_by(|a, b| {
        (
            a.get("created_at").map(util::py_str).unwrap_or_default(),
            a.get("id").map(util::py_str).unwrap_or_default(),
        )
            .cmp(&(
                b.get("created_at").map(util::py_str).unwrap_or_default(),
                b.get("id").map(util::py_str).unwrap_or_default(),
            ))
    });
    let s = review::summary(wiki)?;
    let mut lines = vec![
        format!(
            "{} open, {} deferred, {} total{}",
            s["open"].as_i64().unwrap_or(0),
            s["deferred"].as_i64().unwrap_or(0),
            s["total"].as_i64().unwrap_or(0),
            intro
        ),
        String::new(),
    ];
    if items.is_empty() {
        lines.push("No open or deferred review items.".to_string());
        return Ok(lines);
    }
    lines.push("| Item | Kind | Status | Risk | Age (d) | Affected | Problem |".to_string());
    lines.push("|---|---|---|---|---|---|---|".to_string());
    for it in &items {
        let affected = it.get("affected").and_then(Value::as_array);
        lines.push(format!(
            "| {} | {} | {} | {} | {} | {} | {} |",
            field_or(it.get("id"), "-"),
            field_or(it.get("kind"), "-"),
            field_or(it.get("status"), "-"),
            field_or(it.get("risk"), "-"),
            review::age_days(it.get("created_at").and_then(Value::as_str)),
            affected_links(
                affected.map(|a| a.as_slice()).unwrap_or(&[]),
                known_ids
            ),
            field_or(it.get("problem"), "-").replace('|', "\\|")
        ));
    }
    Ok(lines)
}

/// `_ops_records` — .state/operations.jsonl -> receipt dicts (oldest
/// first); unparseable lines are skipped outright.
fn ops_records(wiki: &Wiki) -> Vec<Value> {
    let mut recs = Vec::new();
    let Ok(text) = std::fs::read_to_string(wiki.p(".state/operations.jsonl")) else {
        return recs;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            recs.push(v);
        }
    }
    recs
}

/// `"%s=%s" % (k, val)` where a list val joins with ",".
fn change_kv(k: &str, val: &Value) -> String {
    match val {
        Value::Array(a) => format!(
            "{k}={}",
            a.iter().map(util::py_str).collect::<Vec<_>>().join(",")
        ),
        other => format!("{k}={}", util::py_str(other)),
    }
}

fn changes_cell(rec: &Value) -> String {
    let mut parts: Vec<String> = match rec.get("changes").and_then(Value::as_object) {
        Some(m) => m.iter().map(|(k, v)| change_kv(k, v)).collect(),
        None => Vec::new(),
    };
    parts.sort();
    if parts.is_empty() {
        "-".to_string()
    } else {
        parts.join(", ")
    }
}

/// `str(rec.get("finished_at", "-"))[:10] or "-"` — date prefix of a stamp.
fn at_day(rec: &Value, key: &str) -> String {
    let s = field_or(rec.get(key), "-");
    let day: String = s.chars().take(10).collect();
    if day.is_empty() {
        "-".to_string()
    } else {
        day
    }
}

/// `write_log` — OKF-style `wiki/log.md` dated change history derived from
/// .state/operations.jsonl.
fn write_log(wiki: &Wiki) -> Result<String, EngineError> {
    let mut lines = vec![
        "# Log".to_string(),
        String::new(),
        "Chronological record of every committed operation. Script-owned — do not edit."
            .to_string(),
        String::new(),
    ];
    for rec in ops_records(wiki).into_iter().rev() {
        let rev = rec.get("revision").cloned().unwrap_or(json!({}));
        lines.push(format!(
            "- **{}** — `{}` (rev {}→{}): {}",
            at_day(&rec, "finished_at"),
            field_or(rec.get("command"), "-"),
            field_or(rev.get("before"), "-"),
            field_or(rev.get("after"), "-"),
            changes_cell(&rec)
        ));
    }
    let front = json!({"type": "log", "generated": now(), "deps": []})
        .as_object()
        .unwrap()
        .clone();
    page(&front, &lines.join("\n"))
}

/// `write_changes_index` — deterministic `wiki/changes/index.md` from
/// .state/operations.jsonl (R3; script-owned).
fn write_changes_index(wiki: &Wiki) -> Result<String, EngineError> {
    let mut lines = vec![
        "# Changes".to_string(),
        String::new(),
        "Operational log derived from `.state/operations.jsonl`. Script-owned — do not edit."
            .to_string(),
        String::new(),
        "| Revision | At | Op | Run | Affected |".to_string(),
        "|---|---|---|---|---|".to_string(),
    ];
    for rec in ops_records(wiki) {
        let rev = rec.get("revision").cloned().unwrap_or(json!({}));
        lines.push(format!(
            "| {}->{} | {} | {} | {} | {} |",
            field_or(rev.get("before"), "-"),
            field_or(rev.get("after"), "-"),
            field_or(rec.get("finished_at"), "-"),
            field_or(rec.get("command"), "-"),
            field_or(rec.get("run_id"), "-"),
            changes_cell(&rec)
        ));
    }
    let front = json!({"type": "index", "section": "changes", "generated": now(), "deps": []})
        .as_object()
        .unwrap()
        .clone();
    page(&front, &lines.join("\n"))
}

/// `_coverage_gaps` — source ids whose extraction left text unextracted (R8).
fn coverage_gaps(wiki: &Wiki) -> Vec<String> {
    let mut gaps = Vec::new();
    let sdir = wiki.p("sources");
    if !sdir.is_dir() {
        return gaps;
    }
    let mut sids: Vec<String> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&sdir) {
        for e in rd.flatten() {
            sids.push(e.file_name().to_string_lossy().into_owned());
        }
    }
    sids.sort();
    for sid in sids {
        let xpath = wiki.p(&format!("sources/{sid}/extraction.json"));
        if !xpath.is_file() {
            continue;
        }
        let cov = std::fs::read_to_string(&xpath)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| {
                v.get("extraction_report")
                    .and_then(|r| r.get("coverage"))
                    .cloned()
            })
            .unwrap_or(Value::Null);
        if cov.get("text").and_then(Value::as_str) == Some("not_extracted") {
            gaps.push(sid);
        }
    }
    gaps
}

fn txn_begin<'a>(wiki: &'a Wiki, command: &str) -> Result<Txn<'a>, EngineError> {
    let run_id = ids::new_run();
    Txn::begin_with(
        wiki,
        &run_id,
        TxnOpts {
            command: Some(command.to_string()),
            ..Default::default()
        },
    )
}

fn front_map(v: Value) -> Map<String, Value> {
    v.as_object().cloned().unwrap_or_default()
}

/// `build_pages` — (re)build deterministic views and register their
/// dependency sets.
pub fn build_pages(wiki: &Wiki, base_revision: i64) -> Result<Value, EngineError> {
    let mut txn = txn_begin(wiki, "wiki-build-pages")?;
    let mut built: Vec<String> = Vec::new();

    let decisions = claims::list_decisions(wiki)?;

    // --- index.md (reviewer inventory, R6) -------------------------------
    built.extend(render_wiki_index(wiki, &mut txn)?);

    // --- review.md (queue page, R1) --------------------------------------
    built.extend(render_review_page(wiki, &mut txn)?);

    // --- changes/index.md (R3, script-owned ops log) ----------------------
    built.extend(render_changes_index(wiki, &mut txn)?);

    // --- log.md (OKF change history) --------------------------------------
    built.extend(render_log_page(wiki, &mut txn)?);

    // --- decision pages ----------------------------------------------------
    for d in &decisions {
        built.extend(render_decision_page(wiki, &mut txn, d)?);
    }

    // --- section indexes + overview (progressive disclosure) -------------
    built.extend(render_index_fabric(wiki, &mut txn)?);

    // agent-authored pages: mark stale in place, PRESERVING the prose body —
    // regeneration is the agent's job; deterministic builds never clobber it
    for sub in ["concepts", "entities", "procedures", "questions", "sources"] {
        let subdir = wiki.p(&format!("wiki/{sub}"));
        if !subdir.is_dir() {
            continue;
        }
        let mut names: Vec<String> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&subdir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.ends_with(".md") && name != "index.md" {
                    names.push(name);
                }
            }
        }
        names.sort();
        for name in names {
            let rel = format!("wiki/{sub}/{name}");
            if deps::is_stale(wiki, &rel).unwrap_or(false) {
                let Ok(text) = std::fs::read_to_string(subdir.join(&name)) else {
                    continue;
                };
                let Ok((mut fr, body)) = frontmatter::split_front(&text) else {
                    continue; // corrupt frontmatter: verify() reports it
                };
                fr.insert("stale".to_string(), json!(true));
                txn.stage_write(&rel, page(&fr, body)?.as_bytes())?;
            }
        }
    }

    txn.changes
        .insert("pages_built".to_string(), json!(built.len() as i64));
    txn.base_revision = base_revision;
    let mut receipt = txn.commit()?;

    // clear stale flags for artifacts we just rebuilt
    let cleared: Vec<String> = deps::stale_artifacts(wiki)?
        .into_iter()
        .filter(|a| built.contains(a))
        .collect();
    if !cleared.is_empty() {
        let mut txn2 = txn_begin(wiki, "wiki-build-pages-clear")?;
        deps::clear_stale(
            &mut txn2,
            &cleared.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
        )?;
        let base = wiki.revision()?;
        txn2.base_revision = base;
        txn2.commit()?;
    }
    receipt["changes"]["pages_built"] = json!(built.len() as i64);
    Ok(receipt)
}

/// `wiki/index.md` (reviewer inventory, R6) — the subject table over live
/// claims. Standalone so doctor --fix can re-render a single artifact in a
/// caller-owned txn (§9.1 rebuild_artifact).
fn render_wiki_index(wiki: &Wiki, txn: &mut Txn) -> Result<Vec<String>, EngineError> {
    let all_claims = claims::list(wiki)?;
    let decisions = claims::list_decisions(wiki)?;
    let section_files: BTreeMap<&str, Vec<String>> = PAGE_KINDS
        .iter()
        .map(|sub| (*sub, section_files(wiki, sub)))
        .collect();
    let mut known_ids: HashMap<String, i64> = HashMap::new();
    for c in &all_claims {
        if let Some(id) = c.get("id").and_then(Value::as_str) {
            known_ids.insert(
                id.to_string(),
                c.get("version").and_then(Value::as_i64).unwrap_or(0),
            );
        }
    }
    for d in &decisions {
        if let Some(id) = d.get("id").and_then(Value::as_str) {
            known_ids.insert(
                id.to_string(),
                d.get("version").and_then(Value::as_i64).unwrap_or(0),
            );
        }
    }
    let mut claim_subject: HashMap<String, String> = HashMap::new();
    for c in &all_claims {
        if let (Some(id), Some(subj)) = (
            c.get("id").and_then(Value::as_str),
            c.get("subject").and_then(Value::as_str),
        ) {
            claim_subject.insert(id.to_string(), subj.to_string());
        }
    }
    let mut open_rev: HashMap<String, i64> = HashMap::new();
    for it in read_queue(wiki)? {
        if it.get("status").and_then(Value::as_str) != Some("open") {
            continue;
        }
        for aid in it
            .get("affected")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            if let Some(subj) = aid.as_str().and_then(|a| claim_subject.get(a)) {
                *open_rev.entry(subj.clone()).or_insert(0) += 1;
                break;
            }
        }
    }
    let subject_pages = subject_pages(wiki);
    let stale_set: HashSet<String> = deps::stale_artifacts(wiki)?.into_iter().collect();

    let mut by_subject: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    for c in &all_claims {
        by_subject
            .entry(c.get("subject").map(util::py_str).unwrap_or_default())
            .or_default()
            .push(c);
    }
    let live_order = ["candidate", "provisional", "accepted", "disputed"];
    let mut lines = vec!["# Wiki index".to_string(), String::new()];
    let mut top_links = vec![
        "[Overview](overview.md)".to_string(),
        "[Review queue](review.md)".to_string(),
        "[Log](log.md)".to_string(),
    ];
    for sub in PAGE_KINDS {
        let n = section_files[sub].len();
        if n > 0 {
            top_links.push(format!("[{sub}]({sub}/index.md) ({n})"));
        }
    }
    lines.push(top_links.join(" | "));
    lines.push(String::new());
    lines.push(
        "| Subject | Live statuses | Disputed | Open reviews | Sources | Last changed | Stale | Page |"
            .to_string(),
    );
    lines.push("|---|---|---|---|---|---|---|---|".to_string());
    for (subject, cs) in &by_subject {
        let mut counts: BTreeMap<String, i64> = BTreeMap::new();
        for c in cs {
            let st = c.get("status").and_then(Value::as_str).unwrap_or("");
            if !claims::TERMINAL.contains(&st) {
                *counts.entry(st.to_string()).or_insert(0) += 1;
            }
        }
        let live_cell = live_order
            .iter()
            .filter(|st| counts.contains_key(**st))
            .map(|st| format!("{st} {}", counts[*st]))
            .collect::<Vec<_>>()
            .join(", ");
        let live_cell = if live_cell.is_empty() {
            "-".to_string()
        } else {
            live_cell
        };
        let disputed = cs
            .iter()
            .filter(|c| c.get("status").and_then(Value::as_str) == Some("disputed"))
            .count();
        let mut srcs = HashSet::new();
        for c in cs {
            for ev in c.get("evidence").and_then(Value::as_array).cloned().unwrap_or_default() {
                if let Some(sid) = ev.get("source_id") {
                    if !sid.is_null() {
                        srcs.insert(serde_json::to_string(sid)?);
                    }
                }
            }
        }
        let last = cs
            .iter()
            .map(|c| {
                c.get("updated_at")
                    .filter(|v| frontmatter::truthy(Some(v)))
                    .or_else(|| c.get("recorded_at"))
                    .map(util::py_str)
                    .unwrap_or_default()
            })
            .max()
            .unwrap_or_default();
        let last: String = last.chars().take(10).collect();
        let last = if last.is_empty() { "-".to_string() } else { last };
        let page_rel = subject_pages.get(subject.as_str());
        let stale_cell = if page_rel.map_or(false, |p| stale_set.contains(p)) {
            "yes"
        } else {
            "-"
        };
        let mark = if disputed > 0 {
            " [DISPUTED]"
        } else if counts.is_empty() {
            " [SUPERSEDED]"
        } else {
            ""
        };
        let link = page_rel.map(|p| &p[5..]);
        lines.push(format!(
            "| {}{} | {} | {} | {} | {} | {} | {} | {} |",
            subject,
            mark,
            live_cell,
            disputed,
            open_rev.get(subject.as_str()).copied().unwrap_or(0),
            srcs.len(),
            last,
            stale_cell,
            link.map(|l| format!("[{l}]({l})")).unwrap_or_else(|| "-".to_string())
        ));
    }
    let rel = "wiki/index.md".to_string();
    let fr = front_map(json!({"type": "index", "generated": now(), "deps": []}));
    txn.stage_write(&rel, page(&fr, &lines.join("\n"))?.as_bytes())?;
    deps::replace(txn, &rel, &[])?;
    Ok(vec![rel])
}

/// `wiki/review.md` (queue page, R1).
fn render_review_page(wiki: &Wiki, txn: &mut Txn) -> Result<Vec<String>, EngineError> {
    let mut known_ids: HashMap<String, i64> = HashMap::new();
    for c in claims::list(wiki)?
        .iter()
        .chain(claims::list_decisions(wiki)?.iter())
    {
        if let Some(id) = c.get("id").and_then(Value::as_str) {
            known_ids.insert(
                id.to_string(),
                c.get("version").and_then(Value::as_i64).unwrap_or(0),
            );
        }
    }
    let rel = "wiki/review.md".to_string();
    let mut body = vec!["# Review queue".to_string(), String::new()];
    body.extend(review_queue_lines(
        wiki,
        &known_ids,
        " — full table: [review.md](review.md).",
    )?);
    let fr = front_map(json!({"type": "review", "generated": now(), "deps": []}));
    txn.stage_write(&rel, page(&fr, &body.join("\n"))?.as_bytes())?;
    deps::replace(txn, &rel, &[])?;
    Ok(vec![rel])
}

/// `wiki/changes/index.md` — script-owned ops log (R3).
fn render_changes_index(wiki: &Wiki, txn: &mut Txn) -> Result<Vec<String>, EngineError> {
    let rel = "wiki/changes/index.md".to_string();
    txn.stage_write(&rel, write_changes_index(wiki)?.as_bytes())?;
    deps::replace(txn, &rel, &[])?;
    Ok(vec![rel])
}

/// `wiki/log.md` — OKF change history.
fn render_log_page(wiki: &Wiki, txn: &mut Txn) -> Result<Vec<String>, EngineError> {
    let rel = "wiki/log.md".to_string();
    txn.stage_write(&rel, write_log(wiki)?.as_bytes())?;
    deps::replace(txn, &rel, &[])?;
    Ok(vec![rel])
}

/// One `wiki/decisions/<did>.md` page + its dep registration — the pinned
/// `deps_list` is regenerated from current claim/decision versions, so a
/// rebuild doubles as the §9.1 version-stale repin.
fn render_decision_page(
    wiki: &Wiki,
    txn: &mut Txn,
    d: &Value,
) -> Result<Vec<String>, EngineError> {
    let did = d.get("id").and_then(Value::as_str).unwrap_or("");
    let dver = d.get("version").and_then(Value::as_i64).unwrap_or(0);
    let mut deps_list = vec![format!("{did}@{dver}")];
    let mut rows = vec![
        "| Subject | Predicate | Value | Scope | Status |".to_string(),
        "|---|---|---|---|---|".to_string(),
    ];
    for cid in d.get("claims").and_then(Value::as_array).cloned().unwrap_or_default() {
        let cid = cid.as_str().unwrap_or("");
        let c = if cid.starts_with("claim_") && ids::validate(cid) {
            match wiki.load_json_opt(&claims::claim_rel(cid)) {
                Ok(Some(c)) => c,
                _ => continue,
            }
        } else {
            continue;
        };
        deps_list.push(format!(
            "{}@{}",
            c.get("id").and_then(Value::as_str).unwrap_or(""),
            c.get("version").and_then(Value::as_i64).unwrap_or(0)
        ));
        let scope = c
            .get("scope")
            .and_then(Value::as_object)
            .map(|o| {
                o.iter()
                    .map(|(k, v)| format!("{k}={}", util::py_str(v)))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        let scope = if scope.is_empty() { "-".to_string() } else { scope };
        rows.push(format!(
            "| {} | {} | {} | {} | {} |",
            util::py_str(c.get("subject").unwrap_or(&Value::Null)),
            util::py_str(c.get("predicate").unwrap_or(&Value::Null)),
            util::py_str(c.get("value").unwrap_or(&Value::Null)),
            scope,
            util::py_str(c.get("status").unwrap_or(&Value::Null)),
        ));
    }
    let mut dbody = vec![
        format!("# {}", util::py_str(&d["title"])),
        String::new(),
        format!("Status: **{}**", util::py_str(&d["status"])),
        String::new(),
    ];
    if frontmatter::truthy(d.get("body")) {
        dbody.push(util::py_str(&d["body"]));
        dbody.push(String::new());
    }
    dbody.push("## Supporting claims".to_string());
    dbody.push(String::new());
    dbody.extend(rows);
    let rel = format!("wiki/decisions/{did}.md");
    let fr = front_map(json!({
        "type": "decision", "decision": did, "generated": now(), "deps": deps_list
    }));
    txn.stage_write(&rel, page(&fr, &dbody.join("\n"))?.as_bytes())?;
    deps::replace(
        txn,
        &rel,
        &deps_list.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
    )?;
    Ok(vec![rel])
}

/// True when `rel` is a script-owned artifact the deterministic builders
/// regenerate — the §9.1 `missing_artifact`/`unresolved_dependency` "when
/// regenerable" test. Agent-authored content pages and the unrendered
/// `wiki/decisions/index.md` are not regenerable.
pub(crate) fn is_script_owned(rel: &str) -> bool {
    // Section index under a PAGE_KINDS dir (render_index_fabric renders
    // exactly these; decisions/ and changes/ are handled by their own
    // arms or not rendered at all).
    let kind_index = rel.starts_with("wiki/")
        && rel.ends_with("/index.md")
        && rel.matches('/').count() == 2
        && rel
            .split('/')
            .nth(1)
            .map(|k| PAGE_KINDS.contains(&k))
            .unwrap_or(false);
    rel == "wiki/index.md"
        || rel == "wiki/review.md"
        || rel == "wiki/overview.md"
        || rel == "wiki/log.md"
        || rel == "wiki/changes/index.md"
        || kind_index
        || (rel.starts_with("wiki/decisions/")
            && rel.ends_with(".md")
            && rel != "wiki/decisions/index.md")
}

/// Rebuild `rel` in-place inside `txn` through the same renderer
/// `build_pages` uses (byte-parity: fixed output goes through the shared
/// helpers above / `render_index_fabric`). Returns false when `rel` is not
/// script-owned (caller falls back to an edge drop).
pub(crate) fn stage_script_owned_artifact(
    wiki: &Wiki,
    txn: &mut Txn,
    rel: &str,
) -> Result<bool, EngineError> {
    match rel {
        "wiki/index.md" => {
            render_wiki_index(wiki, txn)?;
        }
        "wiki/review.md" => {
            render_review_page(wiki, txn)?;
        }
        "wiki/changes/index.md" => {
            render_changes_index(wiki, txn)?;
        }
        "wiki/log.md" => {
            render_log_page(wiki, txn)?;
        }
        "wiki/overview.md" => {
            render_index_fabric(wiki, txn)?;
        }
        _ if rel.starts_with("wiki/")
            && rel.ends_with("/index.md")
            && rel.matches('/').count() == 2
            && rel
                .split('/')
                .nth(1)
                .map(|k| PAGE_KINDS.contains(&k))
                .unwrap_or(false) =>
        {
            render_index_fabric(wiki, txn)?;
        }
        _ if rel.starts_with("wiki/decisions/")
            && rel.ends_with(".md")
            && rel != "wiki/decisions/index.md" =>
        {
            let did = rel[15..rel.len() - 3].to_string();
            let Some(d) = wiki.load_json_opt(&format!("decisions/{did}.json"))? else {
                return Ok(false); // canonical decision gone — not regenerable
            };
            render_decision_page(wiki, txn, &d)?;
        }
        _ => return Ok(false),
    }
    Ok(true)
}

/// `_render_index_fabric` — every section index + wiki/overview.md staged
/// into `txn`. Shared by build_pages and rebuild_section_indexes so both
/// emit byte-identical output. `pub(crate)`: doctor --fix reuses it for the
/// §9.1 unlisted_page/section_index_unlinked rebuild. Returns the rel paths
/// written.
pub(crate) fn render_index_fabric(wiki: &Wiki, txn: &mut Txn) -> Result<Vec<String>, EngineError> {
    let all_claims = claims::list(wiki)?;
    let decisions = claims::list_decisions(wiki)?;
    let section_files: BTreeMap<&str, Vec<String>> = PAGE_KINDS
        .iter()
        .map(|sub| (*sub, section_files(wiki, sub)))
        .collect();
    let mut known_ids: HashMap<String, i64> = HashMap::new();
    for c in all_claims.iter().chain(decisions.iter()) {
        if let Some(id) = c.get("id").and_then(Value::as_str) {
            known_ids.insert(
                id.to_string(),
                c.get("version").and_then(Value::as_i64).unwrap_or(0),
            );
        }
    }
    let stale_set: HashSet<String> = deps::stale_artifacts(wiki)?.into_iter().collect();
    let mut built: Vec<String> = Vec::new();

    // --- section indexes ---------------------------------------------------
    for sub in PAGE_KINDS {
        if *sub == "changes" {
            continue; // wiki/changes/index.md is owned by write_changes_index
        }
        let rel = format!("wiki/{sub}/index.md");
        let names = &section_files[sub];
        if names.is_empty() {
            if wiki.p(&rel).is_file() {
                txn.stage_delete(&rel)?;
                // drop the edge entry + any stale flag for the removed index
                let rel2 = rel.clone();
                txn.stage_state_with(deps::DEPS_FILE, move |data| {
                    if !data.is_object() {
                        *data = json!({});
                    }
                    if let Some(edges) = data["edges"].as_object_mut() {
                        edges.remove(&rel2);
                    }
                    if let Some(stale) = data["stale"].as_array_mut() {
                        stale.retain(|a| a.as_str() != Some(rel2.as_str()));
                    }
                })?;
            }
            continue;
        }
        let mut rows = vec!["| Page | Title |".to_string(), "|---|---|".to_string()];
        for name in names {
            rows.push(format!(
                "| [{}]({}) | {} |",
                &name[..name.len() - 3],
                name,
                page_title(&wiki.p(&format!("wiki/{sub}/{name}")))?
            ));
        }
        let fr = front_map(
            json!({"type": "index", "section": sub, "generated": now(), "deps": []}),
        );
        txn.stage_write(&rel, page(&fr, &rows.join("\n"))?.as_bytes())?;
        built.push(rel.clone());
        deps::replace(txn, &rel, &[])?;
    }

    // --- overview.md -------------------------------------------------------
    let mut overview = vec![
        "# Overview".to_string(),
        String::new(),
        "| Section | Pages |".to_string(),
        "|---|---|".to_string(),
    ];
    for sub in PAGE_KINDS {
        overview.push(format!(
            "| [{sub}]({sub}/index.md) | {} |",
            section_files[sub].len()
        ));
    }
    overview.push(format!("| decisions | {} |", decisions.len()));
    overview.push(String::new());
    overview.push("## Section indexes".to_string());
    overview.push(String::new());
    let mut section_links: Vec<String> = Vec::new();
    for sub in PAGE_KINDS {
        let n = section_files[sub].len();
        if n > 0 {
            section_links.push(format!("- [{sub}]({sub}/index.md) ({n})"));
        }
    }
    if section_links.is_empty() {
        overview.push("- (no sections yet)".to_string());
    } else {
        overview.extend(section_links);
    }
    // R8: review queue, stale pages, disputed/superseded, orphans, coverage,
    // synthesis targets — each with actionable ids/links.
    overview.push(String::new());
    overview.push("## Review queue".to_string());
    overview.push(String::new());
    overview.extend(review_queue_lines(
        wiki,
        &known_ids,
        " — full table: [review.md](review.md).",
    )?);
    overview.push(String::new());
    overview.push("## Stale pages".to_string());
    overview.push(String::new());
    let stale: Vec<String> = stale_set.iter().cloned().collect::<BTreeSet<_>>().into_iter().collect();
    if stale.is_empty() {
        overview.push("No stale artifacts.".to_string());
    } else {
        for a in stale.iter().filter(|a| a.starts_with("wiki/")) {
            let l = &a[5..];
            overview.push(format!("- [{l}]({l}) — rebuild via build-pages"));
        }
        for a in stale.iter().filter(|a| !a.starts_with("wiki/")) {
            overview.push(format!("- {a} (non-wiki artifact)"));
        }
    }
    let mut disputed: Vec<&Value> = all_claims
        .iter()
        .filter(|c| c.get("status").and_then(Value::as_str) == Some("disputed"))
        .collect();
    disputed.sort_by(|a, b| {
        a.get("id").and_then(Value::as_str).cmp(&b.get("id").and_then(Value::as_str))
    });
    overview.push(String::new());
    overview.push("## Disputed claims".to_string());
    overview.push(String::new());
    if disputed.is_empty() {
        overview.push("None.".to_string());
    } else {
        overview.push("| Claim | Subject |".to_string());
        overview.push("|---|---|".to_string());
        for c in disputed {
            overview.push(format!(
                "| [[{}]] | {} |",
                c.get("id").and_then(Value::as_str).unwrap_or(""),
                util::py_str(c.get("subject").unwrap_or(&Value::Null))
            ));
        }
    }
    let mut superseded: Vec<&Value> = all_claims
        .iter()
        .filter(|c| c.get("status").and_then(Value::as_str) == Some("superseded"))
        .collect();
    superseded.sort_by(|a, b| {
        a.get("id").and_then(Value::as_str).cmp(&b.get("id").and_then(Value::as_str))
    });
    overview.push(String::new());
    overview.push("## Superseded claims".to_string());
    overview.push(String::new());
    if superseded.is_empty() {
        overview.push("None.".to_string());
    } else {
        overview.push("| Claim | Supersedes | Superseded by |".to_string());
        overview.push("|---|---|---|".to_string());
        for c in superseded {
            let sups = c
                .get("supersedes")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .map(util::py_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default();
            let sups = if sups.is_empty() { "-".to_string() } else { sups };
            overview.push(format!(
                "| [[{}]] | {} | {} |",
                c.get("id").and_then(Value::as_str).unwrap_or(""),
                sups,
                util::py_str(&c.get("superseded_by").cloned().unwrap_or(json!("-")))
            ));
        }
    }
    let mut orphans: Vec<&Value> = all_claims
        .iter()
        .filter(|c| !frontmatter::truthy(c.get("evidence")))
        .collect();
    orphans.sort_by(|a, b| {
        a.get("id").and_then(Value::as_str).cmp(&b.get("id").and_then(Value::as_str))
    });
    overview.push(String::new());
    overview.push("## Orphan claims".to_string());
    overview.push(String::new());
    if orphans.is_empty() {
        overview.push("None.".to_string());
    } else {
        overview.push("| Claim | Subject |".to_string());
        overview.push("|---|---|".to_string());
        for c in orphans {
            overview.push(format!(
                "| [[{}]] | {} |",
                c.get("id").and_then(Value::as_str).unwrap_or(""),
                util::py_str(c.get("subject").unwrap_or(&Value::Null))
            ));
        }
    }
    let gaps = coverage_gaps(wiki);
    overview.push(String::new());
    overview.push("## Coverage gaps".to_string());
    overview.push(String::new());
    if gaps.is_empty() {
        overview.push("None.".to_string());
    } else {
        for s in &gaps {
            overview.push(format!(
                "- `{s}` — text not extracted; re-ingest with a document parser"
            ));
        }
    }
    let targets = synthesize::page_targets(wiki, None, false, 2, 2)?;
    let ready = targets.get("targets").and_then(Value::as_array).cloned().unwrap_or_default();
    let waiting = targets.get("waiting").and_then(Value::as_array).cloned().unwrap_or_default();
    overview.push(String::new());
    overview.push("## Synthesis targets".to_string());
    overview.push(String::new());
    if ready.is_empty() && waiting.is_empty() {
        overview.push("No synthesis targets.".to_string());
    } else {
        overview.push("| Subject | Kind | Claims | Origins | Artifact | Status |".to_string());
        overview.push("|---|---|---|---|---|---|---|".to_string());
        for t in &ready {
            let artifact = t["artifact"].as_str().unwrap_or("");
            let link = &artifact[5.min(artifact.len())..];
            let status = if t["stale"].as_bool().unwrap_or(false) {
                "stale — rebuild"
            } else if t["exists"].as_bool().unwrap_or(false) {
                "exists"
            } else {
                "not written"
            };
            overview.push(format!(
                "| {} | {} | {} | {} | [{}]({}) | {} |",
                t["subject"].as_str().unwrap_or("-"),
                t["kind"].as_str().unwrap_or("-"),
                t["claim_count"].as_i64().unwrap_or(0),
                t["distinct_origins"].as_i64().unwrap_or(0),
                link,
                link,
                status
            ));
        }
        for w in &waiting {
            overview.push(format!(
                "| {} | - | {} | {} | - | {} |",
                w.get("subject").map(util::py_str).unwrap_or_else(|| "-".to_string()),
                w.get("claim_count").and_then(Value::as_i64).unwrap_or(0),
                w.get("distinct_origins").and_then(Value::as_i64).unwrap_or(0),
                w.get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("below_threshold")
                    .replace('_', " ")
            ));
        }
    }
    let rel = "wiki/overview.md".to_string();
    let fr = front_map(json!({"type": "overview", "generated": now(), "deps": []}));
    txn.stage_write(&rel, page(&fr, &overview.join("\n"))?.as_bytes())?;
    built.push(rel.clone());
    deps::replace(txn, &rel, &[])?;
    Ok(built)
}

/// `rebuild_section_indexes` — regenerate every section index and
/// wiki/overview.md through the same renderer as build_pages, in one staged
/// transaction (doctor --fix-indexes repair path). wiki/changes/index.md is
/// owned by write_changes_index and is not touched.
pub fn rebuild_section_indexes(wiki: &Wiki) -> Result<Vec<String>, EngineError> {
    let mut txn = txn_begin(wiki, "wiki-rebuild-section-indexes")?;
    let mut written = render_index_fabric(wiki, &mut txn)?;
    written.sort();
    txn.changes
        .insert("indexes_written".to_string(), json!(written.len() as i64));
    txn.commit()?;
    Ok(written)
}

/// `verify` — citation + staleness gates: every citation resolves; stale
/// derived artifacts must not silently remain current. THE gate
/// (`citation_resolution`): `{ok, errors, stale}`.
pub fn verify(wiki: &Wiki) -> Result<Value, EngineError> {
    let mut errors: Vec<String> = Vec::new();
    let g = deps::graph(wiki)?;
    let edges: Map<String, Value> = g
        .get("edges")
        .and_then(|e| e.as_object())
        .cloned()
        .unwrap_or_default();
    let stale: BTreeSet<String> = g
        .get("stale")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    let mut known_ids: HashMap<String, i64> = HashMap::new();
    match claims::list(wiki).and_then(|cs| {
        claims::list_decisions(wiki)
            .map(|ds| cs.into_iter().chain(ds).collect::<Vec<_>>())
    }) {
        Ok(objs) => {
            for c in &objs {
                if let Some(id) = c.get("id").and_then(Value::as_str) {
                    known_ids.insert(
                        id.to_string(),
                        c.get("version").and_then(Value::as_i64).unwrap_or(0),
                    );
                }
            }
        }
        Err(_) => {
            // Tolerant fallback (additive `checks` contract): a corrupt
            // claim/decision file must not abort the gate — the audit's
            // claims_parse check names it under `checks` and `ok` still
            // reports false. Scan per file, keep what parses.
            for sub in ["claims", "decisions"] {
                let dir = wiki.p(sub);
                if !dir.is_dir() {
                    continue;
                }
                if let Ok(rd) = std::fs::read_dir(&dir) {
                    for e in rd.flatten() {
                        let p = e.path();
                        if !p.is_file() {
                            continue;
                        }
                        if let Ok(text) = std::fs::read_to_string(&p) {
                            if let Ok(c) = serde_json::from_str::<Value>(&text) {
                                if let Some(id) = c.get("id").and_then(Value::as_str) {
                                    known_ids.insert(
                                        id.to_string(),
                                        c.get("version")
                                            .and_then(Value::as_i64)
                                            .unwrap_or(0),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    for (artifact, _dep_list) in &edges {
        let path = wiki.p(artifact);
        if !path.is_file() {
            errors.push(format!("missing artifact: {artifact}"));
            continue;
        }
        if stale.contains(artifact) {
            errors.push(format!("stale artifact not rebuilt: {artifact}"));
        }
        let text = std::fs::read_to_string(&path)?;
        if !text.starts_with("---\n") {
            errors.push(format!("missing frontmatter: {artifact}"));
            continue;
        }
        let (fmap, body) = match frontmatter::split_front(&text) {
            Ok(x) => x,
            Err(e) => {
                errors.push(format!("invalid frontmatter in {artifact}: {e}"));
                continue;
            }
        };
        for dep in fmap
            .get("deps")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
        {
            if let Some(err) = check_dep(wiki, &dep, artifact, &known_ids, &edges)? {
                errors.push(err);
            }
        }
        for m in synthesize_link_re().captures_iter(body) {
            if !known_ids.contains_key(&m[1]) {
                errors.push(format!(
                    "unresolved wiki-link [[{}]] in {artifact}",
                    &m[1]
                ));
            }
        }
        for pl_path in synthesize::page_links(body) {
            if !wiki.p(&pl_path).is_file() {
                let inner = &pl_path[5..pl_path.len() - 3];
                let (kind, slug) = inner.split_once('/').unwrap_or((inner, ""));
                errors.push(format!(
                    "unresolved wiki-link [[wiki:{kind}/{slug}]] in {artifact}"
                ));
            }
        }
    }

    // every derived page must be registered with a dependency set
    for sub in VERIFY_SECTIONS {
        let subdir = wiki.p(&format!("wiki/{sub}"));
        if !subdir.is_dir() {
            continue;
        }
        let mut names: Vec<String> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&subdir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.ends_with(".md") {
                    names.push(name);
                }
            }
        }
        names.sort();
        for name in names {
            let rel = format!("wiki/{sub}/{name}");
            if !edges.contains_key(&rel) {
                errors.push(format!(
                    "derived page missing dependency registration: {rel}"
                ));
            }
        }
    }

    Ok(json!({
        "ok": errors.is_empty(),
        "errors": errors,
        "stale": stale.iter().cloned().collect::<Vec<_>>(),
    }))
}

fn synthesize_link_re() -> &'static regex::Regex {
    static RE: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r"\[\[([a-z]+_[0-9A-Za-z]+)\]\]").unwrap()
    });
    &RE
}

/// `_check_dep` — typed dep resolution (contract): `claim_X@N` /
/// `decision_Y@N` must match the current version; `wiki/<dir>/<slug>.md`
/// must be a registered edge or an existing file.
fn check_dep(
    wiki: &Wiki,
    dep: &Value,
    artifact: &str,
    known_ids: &HashMap<String, i64>,
    edges: &Map<String, Value>,
) -> Result<Option<String>, EngineError> {
    let dep = match dep.as_str() {
        Some(s) => s.trim().to_string(),
        None => dep.to_string(),
    };
    let (dep_id, ver) = match dep.split_once('@') {
        Some((d, v)) => (d.to_string(), v.to_string()),
        None => (dep.clone(), String::new()),
    };
    if dep_id.starts_with("wiki/") {
        if !dep_id.ends_with(".md")
            || !ver.is_empty()
            || dep_id.split('/').any(|p| p == ".." || p.is_empty())
        {
            return Ok(Some(format!(
                "malformed wiki dependency {dep:?} in {artifact}"
            )));
        }
        if edges.contains_key(&dep_id) || wiki.p(&dep_id).is_file() {
            return Ok(None);
        }
        return Ok(Some(format!(
            "unresolved dependency {dep:?} in {artifact}"
        )));
    }
    if dep_id.starts_with("review_") {
        let queue_ids: HashSet<String> = read_queue(wiki)?
            .iter()
            .filter_map(|i| i.get("id").and_then(Value::as_str).map(String::from))
            .collect();
        if !crate::ids::validate(&dep_id) || !ver.is_empty() || !queue_ids.contains(&dep_id) {
            return Ok(Some(format!(
                "unresolved dependency {dep:?} in {artifact}"
            )));
        }
        return Ok(None);
    }
    let Some(&cur) = known_ids.get(&dep_id) else {
        return Ok(Some(format!(
            "unknown dependency id {dep_id:?} in {artifact}"
        )));
    };
    if !ver.is_empty() && ver.parse::<i64>().ok() != Some(cur) {
        return Ok(Some(format!(
            "dependency {dep_id}@{ver} in {artifact} does not match current version {cur}"
        )));
    }
    Ok(None)
}
