//! `wiki-dream`: corpus reconciliation + garbage collection — port of
//! wikicore/dream.py with the spec-pinned deltas:
//!
//! - `ghost` appended to `PHASES` (spec §8): a `wiki/<kind>/*.md` page
//!   (excl. `index.md`) is deleted iff it has ≥1 registered dep in
//!   `.state/dependencies.json` and EVERY dep is dead. Zero-dep pages are
//!   never eligible, pages named by an open review item's `affected[]`
//!   (path or dep ids) go to `skip`, and unparseable dep strings count as
//!   live. Deletion is `stage_delete` + dep-edge drop in the same commit —
//!   survivors' references to the page are dropped BEFORE the page's own
//!   `edges[page]` entry is removed.
//! - §5.4/ES7: `journal*.json` and `.backup-*` are never `delete` items —
//!   they are deferred to `skip` with reason `repair-required` (the repair
//!   path owns restore-then-delete).
//! - §5.5: staging run dirs matching a live phase AND younger than 24 h go
//!   to `skip`, never `delete`. Age floor: `.live` mtime when present,
//!   else newest file mtime in the run dir.
//! - §5.6/§10.2: `commit.lock`/`index.lock` are engine-owned and never
//!   planned for deletion; a held `commit.lock` still lands in `skip`
//!   (dream.py parity). Other `*.lock` files are stale junk.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};

use fs4::fs_std::FileExt;
use serde_json::{json, Map, Value};

use crate::deps;
use crate::err::EngineError;
use crate::frontmatter;
use crate::index::Index;
use crate::schema;
use crate::store::Wiki;
use crate::txn::{self, Txn, TxnOpts};
use crate::util;
use crate::claims;

/// dream.py:26-27 + spec §8 — `ghost` appended.
pub const PHASES: [&str; 8] = [
    "supersede", "expire", "dedupe", "orphan", "sources", "junk", "derived",
    "ghost",
];

pub const RETENTION_DAYS: i64 = 90;
const ARCHIVE_CLAIMS: &str = "claims/_archive";
const ARCHIVE_SOURCES: &str = ".state/dream-archive/sources";
/// §5.5 age floor.
const LIVE_RUN_MAX_AGE_SECS: i64 = 24 * 3600;

// ---------------------------------------------------------------- dates

/// Proleptic-Gregorian date (days since 1970-01-01) — `datetime.date`
/// parity for `valid_to`/`dreamed_at` comparisons without a chrono dep.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Date(i64);

impl Date {
    pub fn today() -> Date {
        Date((util::unix_secs() / 86_400) as i64)
    }

    /// `datetime.date.fromisoformat` for the `YYYY-MM-DD` form — strict:
    /// anything else (bad digits, nonexistent day like Feb 30) is None.
    pub fn parse(s: &str) -> Option<Date> {
        let b = s.as_bytes();
        if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
            return None;
        }
        let dig = |i: usize, n: usize| -> Option<i64> {
            let mut v: i64 = 0;
            for c in &b[i..i + n] {
                if !c.is_ascii_digit() {
                    return None;
                }
                v = v * 10 + (c - b'0') as i64;
            }
            Some(v)
        };
        let y = dig(0, 4)?;
        let m = dig(5, 2)? as u32;
        let d = dig(8, 2)? as u32;
        if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
            return None;
        }
        let days = days_from_civil(y, m, d);
        let (ry, rm, rd) = civil_from_days(days);
        if (ry, rm, rd) != (y, m, d) {
            return None; // e.g. 2026-02-30 — fromisoformat rejects
        }
        Some(Date(days))
    }

    pub fn iso(&self) -> String {
        let (y, m, d) = civil_from_days(self.0);
        format!("{y:04}-{m:02}-{d:02}")
    }

    fn minus_days(&self, n: i64) -> Date {
        Date(self.0 - n)
    }
}

// Howard Hinnant's civil calendar conversions (public domain).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

// ---------------------------------------------------------------- helpers

fn jstr(v: Option<&Value>) -> String {
    util::py_str(v.unwrap_or(&Value::Null))
}

fn truthy(v: Option<&Value>) -> bool {
    frontmatter::truthy(v)
}

/// hashing.canonical_json — sorted keys, `,`/`:` separators, unescaped
/// non-ASCII (serde_json compact emits exactly that).
fn canonical(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_default()
}

fn sorted_dir_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    if let Ok(rd) = fs::read_dir(dir) {
        for e in rd.flatten() {
            names.push(e.file_name().to_string_lossy().into_owned());
        }
    }
    names.sort();
    names
}

fn push(plan: &mut Map<String, Value>, phase: &str, bucket: &str, item: Value) {
    if let Some(arr) = plan
        .get_mut(phase)
        .and_then(|p| p.get_mut(bucket))
        .and_then(Value::as_array_mut)
    {
        arr.push(item);
    }
}

fn bucket<'v>(phases: &'v Value, phase: &str, name: &str) -> &'v [Value] {
    phases[phase][name].as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn archive_ids(phases: &Value, which: &[&str]) -> HashSet<String> {
    which
        .iter()
        .flat_map(|ph| bucket(phases, ph, "archive").iter())
        .filter_map(|a| a["id"].as_str().map(String::from))
        .collect()
}

/// dream.py `_queue_items` — `.state/review-queue.jsonl`, bad lines skipped.
fn queue_items(wiki: &Wiki) -> Vec<Value> {
    let mut out = Vec::new();
    let Ok(text) = fs::read_to_string(wiki.p(deps::REVIEW_QUEUE)) else {
        return out;
    };
    for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            out.push(v);
        }
    }
    out
}

/// claims.py `_scan_decisions` — canonical `decisions/decision_*.json`,
/// sorted by filename; parse errors propagate like `_scan_claims`.
fn scan_decisions(wiki: &Wiki) -> Result<Vec<Value>, EngineError> {
    let mut out = Vec::new();
    let ddir = wiki.p("decisions");
    if !ddir.is_dir() {
        return Ok(out);
    }
    for name in sorted_dir_names(&ddir) {
        if !(name.starts_with("decision_") && name.ends_with(".json")) {
            continue;
        }
        let text = fs::read_to_string(ddir.join(&name))?;
        out.push(serde_json::from_str(&text)?);
    }
    Ok(out)
}

/// dream.py:59-62 — ids under open review: `affected[]` entries with the
/// `@version` suffix stripped.
fn under_review_set(items: &[Value]) -> HashSet<String> {
    items
        .iter()
        .filter(|i| i.get("status").and_then(Value::as_str) == Some("open"))
        .flat_map(|i| {
            i.get("affected")
                .and_then(Value::as_array)
                .map(|a| a.as_slice())
                .unwrap_or(&[])
                .iter()
                .map(|a| {
                    jstr(Some(a))
                        .split('@')
                        .next()
                        .unwrap_or("")
                        .to_string()
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

/// dream.py `_lock_held` — non-blocking flock probe; True when the lock is
/// actually held (open failure → False, contention → True).
fn lock_held(path: &Path) -> bool {
    let Ok(file) = OpenOptions::new().read(true).write(true).open(path) else {
        return false;
    };
    match file.try_lock_exclusive() {
        Ok(true) => {
            let _ = file.unlock();
            false
        }
        _ => true,
    }
}

/// §5.5 liveness for a staging run dir: live-phase file combination
/// (txn::recovery::staging_is_live) AND younger than the 24 h floor.
/// The floor measures `.live` mtime when present else the newest file
/// mtime in the run dir (dir mtime when empty). `pub(crate)`: doctor
/// --fix applies the same dead-run rule for `staging_leftover` (§9.1).
pub(crate) fn staging_run_live(dir: &Path) -> bool {
    if !txn::recovery::staging_is_live(dir) {
        return false;
    }
    let newest = newest_mtime(dir).unwrap_or(0);
    let now = util::unix_secs() as i64;
    now.saturating_sub(newest) < LIVE_RUN_MAX_AGE_SECS
}

fn newest_mtime(dir: &Path) -> Option<i64> {
    let live = dir.join(".live");
    if let Ok(md) = fs::metadata(&live) {
        return md
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64);
    }
    let mut newest: Option<i64> = None;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            let Ok(ft) = e.file_type() else { continue };
            if ft.is_dir() {
                stack.push(p);
            } else if let Ok(md) = fs::metadata(&p) {
                if let Ok(t) = md.modified() {
                    if let Ok(age) = t.duration_since(std::time::UNIX_EPOCH) {
                        newest = Some(newest.map_or(age.as_secs() as i64, |n| n.max(age.as_secs() as i64)));
                    }
                }
            }
        }
    }
    newest.or_else(|| {
        fs::metadata(dir)
            .ok()
            .and_then(|md| md.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
    })
}

// ---------------------------------------------------------------- plan

/// `dream.collect_plan` — detect every dream action without touching the
/// filesystem. `phases: None`/empty = all PHASES.
pub fn collect_plan(
    wiki: &Wiki,
    phases: Option<&[&str]>,
    now: Option<Date>,
) -> Result<Value, EngineError> {
    let now = now.unwrap_or_else(Date::today);
    let active = |p: &str| phases.map_or(true, |s| s.is_empty() || s.contains(&p));
    let claim_objs = claims::list(wiki)?;
    let decision_objs = scan_decisions(wiki)?;
    let items = queue_items(wiki);
    let under_review = under_review_set(&items);
    // dream.py:63-64 — live CANONICAL ids only (claims + decisions); wiki/
    // and review_ deps resolve through namespace rules, not this set.
    let mut canon_live: HashSet<String> = HashSet::new();
    for c in &claim_objs {
        if let Some(id) = c.get("id").and_then(Value::as_str) {
            canon_live.insert(id.to_string());
        }
    }
    for d in &decision_objs {
        if let Some(id) = d.get("id").and_then(Value::as_str) {
            canon_live.insert(id.to_string());
        }
    }
    let live = deps::live_ids(wiki)?;
    let queue_ids: HashSet<String> = items
        .iter()
        .filter_map(|i| i.get("id").and_then(Value::as_str).map(String::from))
        .collect();

    let mut plan: Map<String, Value> = Map::new();
    for p in PHASES {
        plan.insert(
            p.to_string(),
            json!({"archive": [], "delete": [], "move": [],
                   "edge_drops": [], "skip": []}),
        );
    }

    if active("supersede") {
        for c in &claim_objs {
            let superseded = c.get("status").and_then(Value::as_str) == Some("superseded")
                || truthy(c.get("superseded_by"));
            if !superseded {
                continue;
            }
            let id = jstr(c.get("id"));
            let by = c
                .get("superseded_by")
                .and_then(Value::as_str)
                .unwrap_or("?");
            push(&mut plan, "supersede", "archive", json!({
                "id": id,
                "reason": format!("superseded_by {by}"),
            }));
        }
    }

    if active("expire") {
        let superseded = archive_ids(&Value::Object(plan.clone()), &["supersede"]);
        for c in &claim_objs {
            let id = jstr(c.get("id"));
            if !truthy(c.get("valid_to")) || superseded.contains(&id) {
                continue;
            }
            let status = c.get("status").and_then(Value::as_str).unwrap_or("");
            if claims::TERMINAL.contains(&status) {
                continue;
            }
            let vt = jstr(c.get("valid_to"));
            let vt10: String = vt.chars().take(10).collect();
            match Date::parse(&vt10) {
                Some(d) if d < now => push(&mut plan, "expire", "archive", json!({
                    "id": id,
                    "reason": format!("valid_to {vt} passed"),
                })),
                Some(_) => {}
                None => push(&mut plan, "expire", "skip", json!({
                    "id": id,
                    "reason": format!(
                        "unparseable valid_to {}",
                        schema::py_repr(c.get("valid_to").unwrap_or(&Value::Null))
                    ),
                })),
            }
        }
    }

    if active("dedupe") {
        let already = archive_ids(&Value::Object(plan.clone()), &["supersede", "expire"]);
        let mut index: HashMap<String, usize> = HashMap::new();
        let mut groups: Vec<Vec<&Value>> = Vec::new();
        for c in &claim_objs {
            let status = c.get("status").and_then(Value::as_str).unwrap_or("");
            let id = jstr(c.get("id"));
            if claims::TERMINAL.contains(&status) || !truthy(c.get("id")) || already.contains(&id) {
                continue;
            }
            let scope = match c.get("scope") {
                Some(v) if truthy(Some(v)) => v.clone(),
                _ => json!({}),
            };
            let key = format!(
                "{}\u{1}{}\u{1}{}\u{1}{}",
                canonical(c.get("subject").unwrap_or(&Value::Null)),
                canonical(c.get("predicate").unwrap_or(&Value::Null)),
                canonical(&scope),
                canonical(c.get("value").unwrap_or(&Value::Null)),
            );
            match index.get(&key) {
                Some(&i) => groups[i].push(c),
                None => {
                    index.insert(key, groups.len());
                    groups.push(vec![c]);
                }
            }
        }
        for group in groups.iter_mut() {
            if group.len() < 2 {
                continue;
            }
            group.sort_by(|a, b| {
                // dream.py `c.get("recorded_at") or ""` — missing sorts FIRST.
                a.get("recorded_at")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .cmp(b.get("recorded_at").and_then(Value::as_str).unwrap_or(""))
            });
            let keep = group[group.len() - 1];
            let keep_id = jstr(keep.get("id"));
            for c in &group[..group.len() - 1] {
                push(&mut plan, "dedupe", "archive", json!({
                    "id": jstr(c.get("id")),
                    "reason": format!("duplicate of {keep_id}"),
                }));
            }
        }
    }

    if active("orphan") {
        let already = archive_ids(
            &Value::Object(plan.clone()),
            &["supersede", "expire", "dedupe"],
        );
        for c in &claim_objs {
            if truthy(c.get("evidence")) {
                continue;
            }
            if !truthy(c.get("id")) {
                continue;
            }
            let id = jstr(c.get("id"));
            if already.contains(&id) {
                continue;
            }
            if under_review.contains(&id) {
                push(&mut plan, "orphan", "skip", json!({
                    "id": id, "reason": "open review item",
                }));
                continue;
            }
            let status = jstr(c.get("status"));
            if status == "candidate" {
                push(&mut plan, "orphan", "archive", json!({
                    "id": id, "reason": "candidate with no evidence",
                }));
            } else {
                push(&mut plan, "orphan", "skip", json!({
                    "id": id,
                    "reason": format!("{status} claim lacks evidence — human review"),
                }));
            }
        }
    }

    if active("sources") {
        let cited: HashSet<String> = claim_objs
            .iter()
            .flat_map(|c| {
                c.get("evidence")
                    .and_then(Value::as_array)
                    .map(|a| a.as_slice())
                    .unwrap_or(&[])
                    .iter()
                    .filter_map(|ev| {
                        ev.get("source_id").and_then(Value::as_str).map(String::from)
                    })
                    .collect::<Vec<_>>()
            })
            .collect();
        let sdir = wiki.p("sources");
        if sdir.is_dir() {
            for sid in sorted_dir_names(&sdir) {
                let full = sdir.join(&sid);
                if !full.is_dir() || sid.starts_with('_') {
                    continue;
                }
                if !cited.contains(&sid) {
                    push(&mut plan, "sources", "move", json!({
                        "id": sid,
                        "reason": "no claim cites this source",
                    }));
                }
            }
        }
    }

    if active("junk") {
        let state = wiki.p(".state");
        let staging = state.join("staging");
        if staging.is_dir() {
            for entry in sorted_dir_names(&staging) {
                let rel = format!(".state/staging/{entry}");
                if staging_run_live(&staging.join(&entry)) {
                    push(&mut plan, "junk", "skip", json!({
                        "path": rel, "reason": "live staging run",
                    }));
                } else {
                    push(&mut plan, "junk", "delete", json!({
                        "path": rel, "reason": "staging run leftover",
                    }));
                }
            }
        }
        for name in sorted_dir_names(&state) {
            if name.starts_with("journal") && name.ends_with(".json") {
                // §5.4/ES7: journals are repair breadcrumbs — restore path
                // owns them; dream defers, never deletes.
                push(&mut plan, "junk", "skip", json!({
                    "path": format!(".state/{name}"),
                    "reason": "repair-required",
                }));
            } else if name.starts_with(".backup-") {
                push(&mut plan, "junk", "skip", json!({
                    "path": format!(".state/{name}"),
                    "reason": "repair-required",
                }));
            }
        }
        let locks = state.join("locks");
        if locks.is_dir() {
            for entry in sorted_dir_names(&locks) {
                let rel = format!(".state/locks/{entry}");
                if entry == "commit.lock" {
                    if lock_held(&locks.join(&entry)) {
                        push(&mut plan, "junk", "skip", json!({
                            "path": rel, "reason": "commit.lock is held",
                        }));
                    }
                    // §5.6: engine-owned — never a delete candidate.
                } else if entry == "index.lock" {
                    // §10.2: same never-delete lifecycle.
                } else {
                    push(&mut plan, "junk", "delete", json!({
                        "path": rel, "reason": "stale lock file",
                    }));
                }
            }
        }
    }

    // deps registry shared by derived + ghost.
    let g = deps::graph(wiki)?;
    let edges: Map<String, Value> = g["edges"].as_object().cloned().unwrap_or_default();

    if active("derived") {
        let mut artifacts: Vec<&String> = edges.keys().collect();
        artifacts.sort();
        for artifact in artifacts {
            if !wiki.p(artifact).is_file() {
                push(&mut plan, "derived", "edge_drops", json!({
                    "artifact": artifact,
                    "reason": "registered artifact missing",
                }));
                continue;
            }
            let Some(dep_list) = edges.get(artifact).and_then(Value::as_array) else {
                continue;
            };
            for dep in dep_list {
                let dep_id = jstr(Some(dep)).split('@').next().unwrap_or("").to_string();
                if dep_id.starts_with("wiki/") {
                    if !edges.contains_key(&dep_id) && !wiki.p(&dep_id).is_file() {
                        push(&mut plan, "derived", "edge_drops", json!({
                            "artifact": artifact, "dep": dep,
                            "reason": "unresolved page dep",
                        }));
                    }
                } else if dep_id.starts_with("review_") {
                    if !queue_ids.contains(&dep_id) {
                        push(&mut plan, "derived", "edge_drops", json!({
                            "artifact": artifact, "dep": dep,
                            "reason": "unresolved review dep",
                        }));
                    }
                } else if !canon_live.contains(&dep_id) {
                    push(&mut plan, "derived", "edge_drops", json!({
                        "artifact": artifact, "dep": dep,
                        "reason": "dep id not live",
                    }));
                }
            }
        }
    }

    if active("ghost") {
        for kind in frontmatter::PAGE_KINDS {
            let dir = wiki.p(&format!("wiki/{kind}"));
            if !dir.is_dir() {
                continue;
            }
            for name in sorted_dir_names(&dir) {
                if name == "index.md" || !name.ends_with(".md") {
                    continue;
                }
                if !dir.join(&name).is_file() {
                    continue;
                }
                let rel = format!("wiki/{kind}/{name}");
                // spec §8: ≥1 REGISTERED deps entry required; a page with
                // no edges entry (or an empty list) is never eligible.
                let dep_list = edges
                    .get(&rel)
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if dep_list.is_empty() {
                    continue;
                }
                let all_dead = dep_list
                    .iter()
                    .all(|d| !deps::is_live_dep(&jstr(Some(d)), &live));
                if !all_dead {
                    continue;
                }
                let reviewed = under_review.contains(&rel)
                    || dep_list.iter().any(|d| {
                        under_review.contains(
                            jstr(Some(d)).split('@').next().unwrap_or(""),
                        )
                    });
                if reviewed {
                    push(&mut plan, "ghost", "skip", json!({
                        "path": rel, "reason": "open review item",
                    }));
                    continue;
                }
                push(&mut plan, "ghost", "delete", json!({
                    "path": rel,
                    "reason": "all registered deps are dead",
                }));
                // spec §8 edge-drop order is realized at apply time
                // (survivors' dep lists first, own edges entry last) — the
                // plan records both shapes; apply's _upd sorts by kind.
                push(&mut plan, "ghost", "edge_drops", json!({
                    "artifact": rel,
                    "reason": "ghost page deleted",
                }));
                for (a, ds) in &edges {
                    if *a == rel {
                        continue;
                    }
                    let Some(dep_arr) = ds.as_array() else { continue };
                    for d in dep_arr {
                        if jstr(Some(d)).split('@').next().unwrap_or("") == rel {
                            push(&mut plan, "ghost", "edge_drops", json!({
                                "artifact": a, "dep": d,
                                "reason": "dep on ghost page",
                            }));
                        }
                    }
                }
            }
        }
    }

    let phases_v = Value::Object(plan);
    let mut counts = Map::new();
    for p in PHASES {
        let n = bucket(&phases_v, p, "archive").len()
            + bucket(&phases_v, p, "delete").len()
            + bucket(&phases_v, p, "move").len()
            + bucket(&phases_v, p, "edge_drops").len();
        counts.insert(p.to_string(), json!(n));
    }
    Ok(json!({
        "ok": true, "applied": false,
        "phases": phases_v, "counts": Value::Object(counts),
        "purge": null, "as_of": now.iso(),
    }))
}

// ---------------------------------------------------------------- apply

/// `dream.run` — dry-run by default; `apply` executes the plan in one txn.
pub fn run(
    wiki: &Wiki,
    apply: bool,
    phases: Option<&[&str]>,
    now: Option<Date>,
) -> Result<Value, EngineError> {
    let plan = collect_plan(wiki, phases, now)?;
    if !apply {
        return Ok(plan);
    }
    let now = now.unwrap_or_else(Date::today);
    let dreamed_at = format!("{}T00:00:00Z", now.iso());

    // junk: direct fs deletes (all under .state — non-canonical).
    let mut deleted: Vec<String> = Vec::new();
    for item in bucket(&plan["phases"], "junk", "delete") {
        let Some(rel) = item["path"].as_str() else { continue };
        let full = wiki.p(rel);
        let Ok(md) = full.symlink_metadata() else { continue };
        if md.is_dir() && !md.file_type().is_symlink() {
            let _ = fs::remove_dir_all(&full); // shutil.rmtree(ignore_errors)
        } else {
            fs::remove_file(&full)?;
        }
        deleted.push(rel.to_string());
    }

    // sources: dir move can't ride the file-level txn; pre-commit, recorded.
    let mut moved: Vec<String> = Vec::new();
    for item in bucket(&plan["phases"], "sources", "move") {
        let Some(sid) = item["id"].as_str() else { continue };
        let src = wiki.p(&format!("sources/{sid}"));
        let dst = wiki.p(&format!("{ARCHIVE_SOURCES}/{sid}"));
        if !src.is_dir() {
            continue;
        }
        if let Some(parent) = dst.parent() {
            fs::create_dir_all(parent)?;
        }
        let marker = json!({
            "id": sid,
            "dreamed_at": dreamed_at,
            "dream_reason": item["reason"].clone(),
        });
        fs::write(
            src.join(".dream.json"),
            serde_json::to_string_pretty(&marker)?,
        )?;
        fs::rename(&src, &dst)?;
        moved.push(dst.to_string_lossy().into_owned());
    }

    // canonical moves + edge cleanup in one transaction.
    let mut txn = Txn::begin_with(
        wiki,
        &txn::mint_run_id(),
        TxnOpts {
            command: Some("wiki-dream".to_string()),
            ..TxnOpts::default()
        },
    )?;
    let mut archived: Vec<String> = Vec::new();
    for phase in ["supersede", "expire", "dedupe", "orphan"] {
        for item in bucket(&plan["phases"], phase, "archive") {
            let Some(cid) = item["id"].as_str() else { continue };
            let rel = format!("claims/{cid}.json");
            let Some(mut data) = wiki.load_json_opt(&rel)? else {
                continue;
            };
            data["dreamed_at"] = json!(dreamed_at);
            data["dream_reason"] = item["reason"].clone();
            txn.stage_state(&format!("{ARCHIVE_CLAIMS}/{cid}.json"), &data)?;
            txn.stage_delete(&rel)?;
            archived.push(cid.to_string());
        }
    }

    // ghost: page deletes ride the txn — no page archive (spec §8).
    let mut ghost_deleted: Vec<String> = Vec::new();
    for item in bucket(&plan["phases"], "ghost", "delete") {
        let Some(rel) = item["path"].as_str() else { continue };
        if !wiki.p(rel).is_file() {
            continue;
        }
        txn.stage_delete(rel)?;
        ghost_deleted.push(rel.to_string());
    }

    // edge drops: derived + ghost in one stage_state update. Pinned order
    // (spec §8): drop dead deps from SURVIVING artifacts first, then
    // remove whole edges[<artifact>] entries — survivors never dangle on
    // the just-deleted page.
    let mut drops: Vec<Value> = bucket(&plan["phases"], "derived", "edge_drops").to_vec();
    drops.extend(bucket(&plan["phases"], "ghost", "edge_drops").iter().cloned());
    if !drops.is_empty() {
        txn.stage_state_with(deps::DEPS_FILE, |data| {
            if !data.is_object() {
                *data = json!({});
            }
            let mut edges = data
                .get("edges")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let mut dep_drops: HashMap<String, Vec<Value>> = HashMap::new();
            let mut missing: Vec<String> = Vec::new();
            for d in &drops {
                let a = jstr(d.get("artifact"));
                match d.get("dep") {
                    Some(dep) => dep_drops.entry(a).or_default().push(dep.clone()),
                    None => missing.push(a),
                }
            }
            // survivors first: filter the dead dep out of each list.
            for (a, dead) in dep_drops {
                let kept: Vec<Value> = edges
                    .get(&a)
                    .and_then(Value::as_array)
                    .map(|arr| {
                        arr.iter()
                            .filter(|x| !dead.contains(x))
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                edges.insert(a, Value::Array(kept));
            }
            // then whole-entry removal (missing artifact / ghost's own).
            for a in missing {
                edges.remove(&a);
            }
            data["edges"] = Value::Object(edges);
        })?;
    }

    let mut skipped = Map::new();
    for p in PHASES {
        skipped.insert(p.to_string(), plan["phases"][p]["skip"].clone());
    }
    let mut report = json!({
        "ok": true, "applied": true,
        "phases": plan["phases"].clone(),
        "counts": plan["counts"].clone(),
        "archived": archived,
        "ghost_deleted": ghost_deleted,
        "moved_sources": moved,
        "deleted": deleted,
        "edge_drops": drops.len(),
        "skipped": Value::Object(skipped),
        "dreamed_at": dreamed_at,
        "purge": null,
    });
    txn.stage_state(".state/dream_report.json", &report)?;
    txn.changes.insert(
        "claims_archived".to_string(),
        json!(report["archived"].as_array().map_or(0, Vec::len)),
    );
    txn.changes.insert(
        "ghost_pages_deleted".to_string(),
        json!(ghost_deleted_len(&report)),
    );
    txn.changes.insert(
        "sources_moved".to_string(),
        json!(report["moved_sources"].as_array().map_or(0, Vec::len)),
    );
    txn.changes.insert(
        "junk_deleted".to_string(),
        json!(report["deleted"].as_array().map_or(0, Vec::len)),
    );
    txn.changes.insert("edges_dropped".to_string(), json!(drops.len()));
    let receipt = txn.commit()?;
    let new_rev = receipt["revision"]["after"].as_i64().unwrap_or(0);
    report["run_id"] = receipt["run_id"].clone();
    report["revision"] = receipt["revision"]["after"].clone();
    // §8/§10.2: index rebuild in the same dream run — archived claims and
    // deleted pages leave the tables before this returns.
    Index::rebuild_under_index_lock(wiki, new_rev)?;
    Ok(report)
}

fn ghost_deleted_len(report: &Value) -> usize {
    report["ghost_deleted"].as_array().map_or(0, Vec::len)
}

// ---------------------------------------------------------------- purge

/// `dream._live_references` — every id a live claim/decision/queue
/// item/dep edge still points at.
fn live_references(wiki: &Wiki) -> Result<HashSet<String>, EngineError> {
    let mut refs = HashSet::new();
    for c in claims::list(wiki)? {
        if let Some(a) = c.get("supersedes").and_then(Value::as_array) {
            for s in a {
                refs.insert(jstr(Some(s)));
            }
        }
        if truthy(c.get("superseded_by")) {
            refs.insert(jstr(c.get("superseded_by")));
        }
        if let Some(evs) = c.get("evidence").and_then(Value::as_array) {
            for ev in evs {
                if let Some(sid) = ev.get("source_id").and_then(Value::as_str) {
                    refs.insert(sid.to_string());
                }
            }
        }
    }
    for d in scan_decisions(wiki)? {
        if let Some(a) = d.get("claims").and_then(Value::as_array) {
            for cid in a {
                refs.insert(jstr(Some(cid)));
            }
        }
    }
    for it in queue_items(wiki) {
        if let Some(a) = it.get("affected").and_then(Value::as_array) {
            for x in a {
                refs.insert(jstr(Some(x)).split('@').next().unwrap_or("").to_string());
            }
        }
    }
    let g = deps::graph(wiki)?;
    if let Some(edges) = g["edges"].as_object() {
        for (artifact, ds) in edges {
            refs.insert(artifact.clone());
            if let Some(arr) = ds.as_array() {
                for x in arr {
                    refs.insert(jstr(Some(x)).split('@').next().unwrap_or("").to_string());
                }
            }
        }
    }
    Ok(refs)
}

/// `dream._archived_at` — `dreamed_at` (first 10 chars) of an archive
/// entry: JSON field or `.dream.json` marker; "" on any failure.
fn archived_at(path: &Path) -> String {
    let p: PathBuf = if path.is_file() {
        path.to_path_buf()
    } else {
        path.join(".dream.json")
    };
    let Ok(text) = fs::read_to_string(&p) else {
        return String::new();
    };
    let Ok(data) = serde_json::from_str::<Value>(&text) else {
        return String::new();
    };
    data.get("dreamed_at")
        .map(|v| jstr(Some(v)))
        .unwrap_or_default()
        .chars()
        .take(10)
        .collect()
}

/// `dream.purge` — delete archive entries older than `retention_days`
/// and unreferenced.
pub fn purge(
    wiki: &Wiki,
    dry_run: bool,
    now: Option<Date>,
    retention_days: i64,
) -> Result<Value, EngineError> {
    let now = now.unwrap_or_else(Date::today);
    let cutoff = now.minus_days(retention_days);
    let refs = live_references(wiki)?;

    let mut entries: Vec<(String, String)> = Vec::new();
    let adir = wiki.p(ARCHIVE_CLAIMS);
    if adir.is_dir() {
        for name in sorted_dir_names(&adir) {
            if name.ends_with(".json") {
                entries.push((
                    format!("{ARCHIVE_CLAIMS}/{name}"),
                    name.trim_end_matches(".json").to_string(),
                ));
            }
        }
    }
    let sdir = wiki.p(ARCHIVE_SOURCES);
    if sdir.is_dir() {
        for sid in sorted_dir_names(&sdir) {
            entries.push((format!("{ARCHIVE_SOURCES}/{sid}"), sid));
        }
    }

    let mut purged: Vec<String> = Vec::new();
    let mut skipped: Vec<Value> = Vec::new();
    for (rel, eid) in entries {
        let full = wiki.p(&rel);
        let at = archived_at(&full);
        let old = Date::parse(&at).map_or(false, |d| d < cutoff);
        if !old {
            skipped.push(json!({
                "path": rel, "reason": "too_recent",
                "dreamed_at": if at.is_empty() { "unknown".into() } else { at },
            }));
        } else if refs.contains(&eid) {
            skipped.push(json!({"path": rel, "reason": "still_referenced"}));
        } else {
            purged.push(rel);
        }
    }

    if !dry_run && !purged.is_empty() {
        let mut txn = Txn::begin_with(
            wiki,
            &txn::mint_run_id(),
            TxnOpts {
                command: Some("wiki-dream-purge".to_string()),
                ..TxnOpts::default()
            },
        )?;
        for rel in &purged {
            let full = wiki.p(rel);
            if full.is_dir() {
                let _ = fs::remove_dir_all(&full); // shutil.rmtree parity
            } else {
                txn.stage_delete(rel)?;
            }
        }
        txn.stage_state(
            ".state/dream_report.json",
            &json!({
                "ok": true, "applied": true, "phases": {}, "counts": {},
                "purge": {"purged": purged, "skipped": skipped},
            }),
        )?;
        txn.changes
            .insert("archive_purged".to_string(), json!(purged.len()));
        let receipt = txn.commit()?;
        let new_rev = receipt["revision"]["after"].as_i64().unwrap_or(0);
        Index::rebuild_under_index_lock(wiki, new_rev)?;
    }

    Ok(json!({
        "ok": true, "applied": !dry_run, "phases": {}, "counts": {},
        "purge": {
            "retention_days": retention_days,
            "cutoff": cutoff.iso(),
            "purged": purged,
            "skipped": skipped,
        },
    }))
}
