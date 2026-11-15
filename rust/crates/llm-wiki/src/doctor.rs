//! `doctor` — static + semantic diagnostics over the wiki
//! (PRD §37/§24; port of `wikicore/doctor.py::run`) plus the §9 fix path.
//! Findings are `{severity, code, detail, fix}` sorted by severity rank
//! then code; `ok` = no error-severity finding. `doctor --fix` plans from
//! the §9.1 FIXABLE table, applies file fixes in one transaction, then
//! rebuilds `index.sqlite` post-commit (§9.2).
//!
//! Rust additions over doctor.py (spec §9.1/§10.3): `index_stale` emitted
//! from `index::staleness_probe` (missing `index.sqlite` emits nothing —
//! absence is normal degradation), and `legacy_state` for the Python-era
//! derived files `.state/claims-index.json` + `.state/fts.sqlite`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::Path;

use fs4::fs_std::FileExt;
use serde_json::{json, Map, Value};

use crate::claims;
use crate::deps;
use crate::dream;
use crate::frontmatter;
use crate::err::EngineError;
use crate::index::{self, Index};
use crate::pages;
use crate::schema::{self, py_repr, py_repr_str, py_str};
use crate::store::Wiki;
use crate::synthesize;
use crate::topics;
use crate::txn::{self, Txn, TxnOpts};
use crate::util::{canonical_json, py_json_dumps};

/// claims.py:28 — TERMINAL statuses.
const TERMINAL: &[&str] = &["superseded", "rejected"];

/// doctor.py:26 — candidates unreviewed longer than this are semantic debt.
const CANDIDATE_AGE_DAYS: i64 = 14;

/// Deferred review items re-flag after this many days (doctor.py:372-377).
const DEFERRED_AGE_DAYS: i64 = 7;

/// synthesize.py:35 — near-duplicate Jaccard lint threshold.
const NEAR_DUP_LINT_THRESHOLD: f64 = 0.70;

/// doctor.py:23-24 — sections whose content pages form the nav fabric.
const PAGE_SECTIONS: &[&str] = &[
    "concepts",
    "entities",
    "procedures",
    "questions",
    "sources",
    "changes",
];

/// Required review-item keys (doctor.py:357-358).
const REVIEW_ITEM_FIELDS: &[&str] =
    &["id", "created_at", "kind", "status", "problem", "affected", "risk"];

/// Legacy derived files the Python engine maintained under `.state/`
/// (spec §10.3): ignored by Rust, deleted by `doctor --fix`.
const LEGACY_DERIVED: &[&str] = &[".state/claims-index.json", ".state/fts.sqlite"];

fn id_re() -> regex::Regex {
    regex::Regex::new(r"^[a-z]+_[0-9A-HJKMNP-TV-Z]{26}$").unwrap()
}

fn md_link_re() -> regex::Regex {
    // doctor.py:19 — markdown links to .md targets.
    regex::Regex::new(r"\[[^\]]*\]\(([^)#\s]+\.md)\)").unwrap()
}

fn wiki_link_re() -> regex::Regex {
    // doctor.py:20 — [[claim_id]] citations.
    regex::Regex::new(r"\[\[([a-z]+_[0-9A-Za-z]+)\]\]").unwrap()
}

fn page_link_re() -> regex::Regex {
    // synthesize.py PAGE_LINK_RE — [[wiki:<kind>/<slug>]].
    regex::Regex::new(r"\[\[wiki:([a-z]+)/([a-z0-9][a-z0-9\-]*)\]\]").unwrap()
}



fn token_split_re() -> regex::Regex {
    // text.py _SPLIT_RE.
    regex::Regex::new(r"[^0-9a-z]+").unwrap()
}

/// text.py _SUFFIXES — one conservative suffix stripped per token.
const TOKEN_SUFFIXES: &[&str] = &["tion", "ment", "ing", "ed", "ly", "s"];

/// One finding — `{"severity","code","detail","fix"}` (doctor.py:38).
#[derive(Debug, Clone)]
pub struct Finding {
    pub severity: &'static str,
    pub code: &'static str,
    pub detail: String,
    pub fix: String,
}

impl Finding {
    fn to_value(&self) -> Value {
        json!({
            "severity": self.severity,
            "code": self.code,
            "detail": self.detail,
            "fix": self.fix,
        })
    }
}

/// `plan_add` entries — `{code, path, action}` (doctor.py:400-401).
#[derive(Debug, Clone)]
struct PlanItem {
    code: &'static str,
    path: String,
    action: String,
}

impl PlanItem {
    fn to_value(&self) -> Value {
        json!({"code": self.code, "path": self.path, "action": self.action})
    }
}

/// Doctor report — `{ok, findings[], counts{error,warn,info}, fts,
/// extractors, repair_plan}` (doctor.py:462-466 key parity).
pub struct Report {
    pub ok: bool,
    pub findings: Vec<Finding>,
    pub counts: Map<String, Value>,
    pub fts: Value,
    pub extractors: Value,
    pub repair_plan: Vec<Value>,
}

impl Report {
    /// Payload `{ok, findings, counts, fts, extractors, repair_plan}`.
    pub fn to_value(&self) -> Value {
        json!({
            "ok": self.ok,
            "findings": self.findings.iter().map(Finding::to_value).collect::<Vec<_>>(),
            "counts": Value::Object(self.counts.clone()),
            "fts": self.fts.clone(),
            "extractors": self.extractors.clone(),
            "repair_plan": self.repair_plan.clone(),
        })
    }

    /// `--scope` substring filter (wiki.py:617-623): a finding survives
    /// when `scope in json.dumps(finding)` — Python's *default* dumps
    /// separators (`", "` / `": "`), so the serialized form is rebuilt
    /// here rather than using `to_value` + `serde_json::to_string`
    /// (compact). `ok` and `counts` are recounted over the survivors.
    pub fn scoped(&self, scope: &str) -> Report {
        let findings: Vec<Finding> = self
            .findings
            .iter()
            .filter(|f| py_json_dumps(&f.to_value()).contains(scope))
            .cloned()
            .collect();
        let (ok, counts) = count_findings(&findings);
        Report {
            ok,
            findings,
            counts,
            fts: self.fts.clone(),
            extractors: self.extractors.clone(),
            repair_plan: self.repair_plan.clone(),
        }
    }
}


fn count_findings(findings: &[Finding]) -> (bool, Map<String, Value>) {
    let mut counts = Map::new();
    for sev in ["error", "warn", "info"] {
        let n = findings.iter().filter(|f| f.severity == sev).count();
        counts.insert(sev.to_string(), json!(n));
    }
    let ok = !findings.iter().any(|f| f.severity == "error");
    (ok, counts)
}

struct Ctx {
    findings: Vec<Finding>,
    plan: Vec<PlanItem>,
}

impl Ctx {
    fn add(&mut self, severity: &'static str, code: &'static str, detail: String, fix: &str) {
        self.findings.push(Finding {
            severity,
            code,
            detail,
            fix: fix.to_string(),
        });
    }

    fn plan_add(&mut self, code: &'static str, path: &str, action: String) {
        self.plan.push(PlanItem {
            code,
            path: path.to_string(),
            action,
        });
    }
}

/// `wiki/<kind>/<slug>.md` -> `wiki:<kind>/<slug>` (doctor.py _wiki_ref).
fn wiki_ref(page_rel: &str) -> String {
    let stripped = page_rel
        .strip_prefix("wiki/")
        .unwrap_or(page_rel)
        .strip_suffix(".md")
        .unwrap_or(page_rel.strip_prefix("wiki/").unwrap_or(page_rel));
    format!("wiki:{stripped}")
}

fn read_text(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok()
}

/// Read + parse a JSON file, yielding the io/parse error string on
/// failure — the corrupt-JSON guard shared by the claims/decisions scans
/// (doctor.py: a corrupt file is a `schema_invalid` finding, never a crash).
fn read_json(path: &Path) -> Result<Value, String> {
    let text = fs::read_to_string(path).map_err(|e| e.to_string())?;
    serde_json::from_str(&text).map_err(|e| e.to_string())
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


/// synthesize.py `extract_page_links` — `[[wiki:kind/slug]]` -> `kind/slug`,
/// deduped in first-seen order.
fn extract_page_links(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in page_link_re().captures_iter(body) {
        let r = format!("{}/{}", &m[1], &m[2]);
        if !out.contains(&r) {
            out.push(r);
        }
    }
    out
}

/// text.py `norm_tokens` — lowercase alnum runs, len>=3, one suffix strip.
fn norm_tokens(s: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    for tok in token_split_re().split(&s.to_lowercase()) {
        if tok.chars().count() < 3 {
            continue;
        }
        let mut t = tok.to_string();
        for suf in TOKEN_SUFFIXES {
            let min_len = if *suf == "s" { 3 } else { 4 };
            if t.ends_with(suf) && t.len() - suf.len() >= min_len {
                t.truncate(t.len() - suf.len());
                break;
            }
        }
        out.insert(t);
    }
    out
}

fn normalize_tokens(v: &Value) -> HashSet<String> {
    norm_tokens(&py_str(v))
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = a.union(b).count() as f64;
    inter / union
}

fn build_token_index(entries: &HashMap<String, HashSet<String>>) -> BTreeMap<String, Vec<String>> {
    let mut index: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (key, toks) in entries {
        for tok in toks {
            index.entry(tok.clone()).or_default().push(key.clone());
        }
    }
    for keys in index.values_mut() {
        keys.sort();
    }
    index
}

fn candidate_pairs(index: &BTreeMap<String, Vec<String>>) -> Vec<(String, String)> {
    let mut pairs: Vec<(String, String)> = Vec::new();
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for keys in index.values() {
        for i in 0..keys.len() {
            for b in keys.iter().skip(i + 1) {
                let pair = (keys[i].clone(), b.clone());
                if seen.insert(pair.clone()) {
                    pairs.push(pair);
                }
            }
        }
    }
    pairs.sort();
    pairs
}

// ---------------------------------------------------------------------
// review.py `age_days` — whole days since a `%Y-%m-%dT%H:%M:%SZ` stamp;
// 0 when missing/unparseable.
// ---------------------------------------------------------------------

fn parse_iso_ts(ts: &str) -> Option<i64> {
    // "%Y-%m-%dT%H:%M:%SZ" strict shape.
    let re = regex::Regex::new(
        r"^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})Z$",
    )
    .unwrap();
    let m = re.captures(ts)?;
    let (y, mo, d) = (
        m[1].parse::<i64>().ok()?,
        m[2].parse::<i64>().ok()?,
        m[3].parse::<i64>().ok()?,
    );
    let (h, mi, s) = (
        m[4].parse::<i64>().ok()?,
        m[5].parse::<i64>().ok()?,
        m[6].parse::<i64>().ok()?,
    );
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) || h > 23 || mi > 59 || s > 60 {
        return None;
    }
    Some(days_from_civil(y, mo as u32, d as u32) * 86_400 + h * 3600 + mi * 60 + s)
}

/// Howard Hinnant's days-from-civil (inverse of util's civil_from_days).
fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(146_097);
    let yoe = y - era * 400;
    let mp = ((m + 9) % 12) as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn age_days(ts: Option<&Value>) -> i64 {
    let Some(s) = ts.and_then(Value::as_str) else {
        return 0;
    };
    let Some(then) = parse_iso_ts(s) else {
        return 0;
    };
    let now = crate::util::unix_secs() as i64;
    ((now - then) / 86_400).max(0)
}

/// `YYYY-MM-DD` (Python `date.fromisoformat` basic form) -> (y, m, d).
fn parse_date(s: &str) -> Option<(i64, u32, u32)> {
    let compact = regex::Regex::new(r"^(\d{4})(\d{2})(\d{2})$").unwrap();
    let dashed = regex::Regex::new(r"^(\d{4})-(\d{2})-(\d{2})$").unwrap();
    for re in [&dashed, &compact] {
        if let Some(m) = re.captures(s) {
            let (y, mo, d) = (
                m[1].parse::<i64>().ok()?,
                m[2].parse::<u32>().ok()?,
                m[3].parse::<u32>().ok()?,
            );
            if (1..=12).contains(&mo) && (1..=31).contains(&d) {
                return Some((y, mo, d));
            }
            return None;
        }
    }
    None
}

/// Today's UTC date — doctor.py uses `datetime.date.today()` (local); the
/// engine stamps UTC everywhere, so UTC is the consistent clock here.
fn today() -> (i64, u32, u32) {
    let days = (crate::util::unix_secs() / 86_400) as i64;
    civil_from_days(days)
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

// ---------------------------------------------------------------------
// Review queue (doctor.py `_queue_items`) — parsed items; unparseable
// lines skipped here and flagged by the hygiene pass.
// ---------------------------------------------------------------------

fn queue_items(wiki: &Wiki) -> Vec<Value> {
    let mut out = Vec::new();
    let Ok(text) = fs::read_to_string(wiki.p(deps::REVIEW_QUEUE)) else {
        return out;
    };
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(v) = serde_json::from_str::<Value>(line) {
            out.push(v);
        }
    }
    out
}


fn source_cited(claim_objs: &[Value], sid: &str) -> bool {
    claim_objs.iter().any(|c| {
        c.get("evidence")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .any(|ev| ev.get("source_id").and_then(Value::as_str) == Some(sid))
            })
            .unwrap_or(false)
    })
}

fn str_list(v: Option<&Value>) -> Vec<String> {
    v.and_then(Value::as_array)
        .map(|a| a.iter().map(py_str).collect())
        .unwrap_or_default()
}

fn dep_str(d: &Value) -> String {
    py_str(d)
}

// ---------------------------------------------------------------------
// The scan — doctor.py `run(wiki)` top-to-bottom.
// ---------------------------------------------------------------------

pub fn run(wiki: &Wiki) -> Result<Report, EngineError> {
    let mut cx = Ctx {
        findings: Vec::new(),
        plan: Vec::new(),
    };
    let claim_schema: Value = serde_json::from_str(schema::CLAIM_SCHEMA)?;
    let decision_schema: Value = serde_json::from_str(schema::DECISION_SCHEMA)?;

    // --- static: schema validity of every canonical object -------------
    let claims_dir = wiki.p("claims");
    let claim_files = sorted_dir_names(&claims_dir);
    let mut claim_objs: Vec<Value> = Vec::new();
    for name in &claim_files {
        if !(name.starts_with("claim_") && name.ends_with(".json")) {
            continue;
        }
        let path = claims_dir.join(name);
        let data: Value = match read_json(&path) {
            Ok(v) => v,
            Err(err) => {
                cx.add(
                    "error",
                    "schema_invalid",
                    format!("claims/{name}: {err}"),
                    "restore from operations history or delete (derived views rebuild)",
                );
                continue;
            }
        };
        claim_objs.push(data.clone());
        let errors = schema::validate(&data, &claim_schema);
        if !errors.is_empty() {
            cx.add(
                "error",
                "schema_invalid",
                format!("claims/{name}: {}", errors.join("; ")),
                "fix via wiki-review actions or correct the JSON to schema",
            );
        }
        if data.get("id") != Some(&Value::String(name[..name.len() - 5].to_string())) {
            cx.add(
                "error",
                "invalid_id",
                format!("claims/{name}: id mismatch"),
                "rename file or fix id field",
            );
        }
    }

    let decisions_dir = wiki.p("decisions");
    let decision_files = sorted_dir_names(&decisions_dir);
    let mut decision_objs: Vec<Value> = Vec::new();
    for name in &decision_files {
        if !(name.starts_with("decision_") && name.ends_with(".json")) {
            continue;
        }
        let path = decisions_dir.join(name);
        let data: Value = match read_json(&path) {
            Ok(v) => v,
            Err(err) => {
                cx.add(
                    "error",
                    "schema_invalid",
                    format!("decisions/{name}: {err}"),
                    "restore from operations history or correct the JSON",
                );
                continue;
            }
        };
        decision_objs.push(data.clone());
        let errors = schema::validate(&data, &decision_schema);
        if !errors.is_empty() {
            cx.add(
                "error",
                "schema_invalid",
                format!("decisions/{name}: {}", errors.join("; ")),
                "fix decision JSON to schema",
            );
        }
        if data.get("id") != Some(&Value::String(name[..name.len() - 5].to_string())) {
            cx.add(
                "error",
                "invalid_id",
                format!("decisions/{name}: id mismatch"),
                "rename file or fix id field",
            );
        }
    }

    let auto_stubs = decision_objs
        .iter()
        .filter(|d| {
            d.get("auto_materialized") == Some(&Value::Bool(true))
                && d.get("status") == Some(&Value::String("accepted".into()))
        })
        .count();
    if auto_stubs > 0 {
        cx.add(
            "info",
            "auto_decision_stub",
            format!("{auto_stubs} auto-materialized decision stub(s) awaiting human review"),
            "review decisions/ stubs and flesh out or retract via wiki-decide/wiki-review",
        );
    }

    // --- static: broken references --------------------------------------
    let sources_dir = wiki.p("sources");
    let known_sources: HashSet<String> = sorted_dir_names(&sources_dir).into_iter().collect();
    let claim_ids: HashSet<String> = claim_files
        .iter()
        .filter(|n| n.ends_with(".json"))
        .map(|n| n[..n.len() - 5].to_string())
        .collect();
    for c in &claim_objs {
        let cid = c.get("id").and_then(Value::as_str).unwrap_or("?");
        for ev in c.get("evidence").and_then(Value::as_array).cloned().unwrap_or_default() {
            let sid = ev.get("source_id").and_then(Value::as_str).unwrap_or("");
            if !known_sources.contains(sid.split('@').next().unwrap_or("")) {
                cx.add(
                    "error",
                    "broken_reference",
                    format!("claim {cid} cites missing source {sid}"),
                    "re-ingest the source or correct the citation",
                );
            } else if let Some(ver) = ev.get("source_version").and_then(Value::as_i64) {
                if let Ok(m) = wiki.load_json(&format!("sources/{sid}/manifest.json")) {
                    let n = m
                        .get("versions")
                        .and_then(Value::as_array)
                        .map(|a| a.len() as i64)
                        .unwrap_or(-1);
                    if n >= 0 && ver > n {
                        cx.add(
                            "error",
                            "broken_reference",
                            format!("claim {cid} cites version {ver} of {sid} (only {n} exist)"),
                            "correct the version or re-ingest",
                        );
                    }
                }
            }
        }
        for sup in str_list(c.get("supersedes")) {
            if !claim_ids.contains(&sup) {
                cx.add(
                    "error",
                    "broken_reference",
                    format!("claim {cid} supersedes missing claim {sup}"),
                    "correct or remove the supersedes entry",
                );
            }
        }
    }
    let terminal_claims: HashSet<String> = claim_objs
        .iter()
        .filter(|c| {
            c.get("id").is_some()
                && c.get("status")
                    .and_then(Value::as_str)
                    .map(|s| TERMINAL.contains(&s))
                    .unwrap_or(false)
        })
        .filter_map(|c| c.get("id").and_then(Value::as_str).map(String::from))
        .collect();
    for d in &decision_objs {
        let did = d.get("id").and_then(Value::as_str).unwrap_or("?");
        let dclaims = str_list(d.get("claims"));
        for cid in &dclaims {
            if !claim_ids.contains(cid) {
                cx.add(
                    "error",
                    "broken_reference",
                    format!("decision {did} references missing claim {cid}"),
                    "update the decision's claim list",
                );
            }
        }
        for ev in d.get("evidence").and_then(Value::as_array).cloned().unwrap_or_default() {
            let sid = ev.get("source_id").and_then(Value::as_str).unwrap_or("");
            if !known_sources.contains(sid.split('@').next().unwrap_or("")) {
                cx.add(
                    "error",
                    "broken_reference",
                    format!("decision {did} cites missing source {sid}"),
                    "re-ingest the source or correct the citation",
                );
            }
        }
        if !frontmatter::truthy(d.get("claims")) && !frontmatter::truthy(d.get("evidence")) {
            let title = d.get("title").and_then(Value::as_str).unwrap_or("?");
            cx.add(
                "warn",
                "orphan_decision",
                format!("decision {did} ({title}) references no claims and cites no evidence"),
                "link supporting claims or evidence, or retract the decision",
            );
        }
        let stale_refs: Vec<String> = dclaims
            .iter()
            .filter(|cid| terminal_claims.contains(*cid))
            .cloned()
            .collect();
        let status = d.get("status").and_then(Value::as_str).unwrap_or("");
        if (status == "proposed" || status == "accepted") && !stale_refs.is_empty() {
            let mut sorted = stale_refs.clone();
            sorted.sort();
            cx.add(
                "warn",
                "stale_decision",
                format!("decision {did} rests on superseded/rejected claim(s) {}", sorted.join(", ")),
                "revisit the decision in light of the newer claims",
            );
        }
    }

    // --- static: unextracted/orphan sources; staging leftovers -----------
    let dep_edges: Map<String, Value> = match deps::graph(wiki) {
        Ok(g) => g
            .get("edges")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
        Err(_) => Map::new(),
    };
    let claims_by_id: HashMap<String, &Value> = claim_objs
        .iter()
        .filter_map(|c| c.get("id").and_then(Value::as_str).map(|id| (id.to_string(), c)))
        .collect();
    let mut covered_sources: HashSet<String> = HashSet::new();
    for (page, page_deps) in &dep_edges {
        if !page.starts_with("wiki/") || !wiki.p(page).is_file() {
            continue;
        }
        for d in page_deps.as_array().cloned().unwrap_or_default() {
            let dep = dep_str(&d);
            let base = dep.split('@').next().unwrap_or("");
            if let Some(c) = claims_by_id.get(base) {
                for ev in c.get("evidence").and_then(Value::as_array).cloned().unwrap_or_default() {
                    if let Some(sid) = ev.get("source_id").and_then(Value::as_str) {
                        covered_sources.insert(sid.to_string());
                    }
                }
            }
        }
    }
    let id_re = id_re();
    for sid in &known_sources {
        if !id_re.is_match(sid) {
            cx.add(
                "warn",
                "invalid_id",
                format!("sources/{sid} is not a valid id"),
                "rename directory",
            );
        } else if !source_cited(&claim_objs, sid) {
            cx.add(
                "info",
                "unextracted_source",
                format!("source {sid} is not cited by any claim"),
                "run wiki-compile to extract claims, or leave as raw evidence",
            );
        } else if !covered_sources.contains(sid) {
            cx.add(
                "info",
                "orphan_source",
                format!("source {sid} is cited by claims but no page covers it"),
                "run wiki page-targets/write-page to synthesize its claims",
            );
        }
    }
    let staging = wiki.p(".state/staging");
    if staging.is_dir() {
        for entry in sorted_dir_names(&staging) {
            if entry.starts_with("run_") {
                cx.add(
                    "info",
                    "staging_leftover",
                    format!("staging run {entry} still present"),
                    &format!("inspect then delete .state/staging/{entry} (failed or abandoned run)"),
                );
            }
        }
    }

    // --- transaction atomicity leftovers (R10) ---------------------------
    let state_dir = wiki.p(".state");
    if state_dir.is_dir() {
        if state_dir.join("journal.json").is_file() {
            cx.add(
                "error",
                "transaction_journal_leftover",
                "interrupted transaction left .state/journal.json".to_string(),
                "restore affected files from the newest .state/.backup-<rev>/ dir, then delete the journal",
            );
        }
        for entry in sorted_dir_names(&state_dir) {
            if entry.starts_with(".backup-") && state_dir.join(&entry).is_dir() {
                cx.add(
                    "warn",
                    "stale_transaction_backup",
                    format!("transaction backup dir .state/{entry} still present"),
                    &format!("verify the corresponding commit landed, then delete .state/{entry}"),
                );
            }
        }
    }

    // --- commit-lock hygiene ----------------------------------------------
    // `commit.lock` and `index.lock` (§10.2) are engine-owned and never
    // deleted — any OTHER file under .state/locks/ is a stale_lock warn.
    let locks_dir = wiki.p(".state/locks");
    if locks_dir.is_dir() {
        for entry in sorted_dir_names(&locks_dir) {
            if entry != "commit.lock" && entry != "index.lock" {
                cx.add(
                    "warn",
                    "stale_lock",
                    format!("unexpected file .state/locks/{entry}"),
                    &format!("inspect then delete .state/locks/{entry}"),
                );
            }
        }
        let commit_lock = locks_dir.join("commit.lock");
        if commit_lock.is_file() {
            if let Ok(file) = fs::OpenOptions::new().read(true).open(&commit_lock) {
                match file.try_lock_exclusive() {
                    Ok(true) => {
                        let _ = file.unlock();
                    }
                    Ok(false) => {
                        cx.add(
                            "warn",
                            "stale_lock",
                            ".state/locks/commit.lock is still held".to_string(),
                            "if no wiki command is running, remove the lock file",
                        );
                    }
                    Err(e) if lock_would_block(&e) => {
                        cx.add(
                            "warn",
                            "stale_lock",
                            ".state/locks/commit.lock is still held".to_string(),
                            "if no wiki command is running, remove the lock file",
                        );
                    }
                    Err(_) => {}
                }
            }
        }
    }

    // --- static: stale derived artifacts ---------------------------------
    for artifact in deps::stale_artifacts(wiki).unwrap_or_default() {
        cx.add(
            "warn",
            "stale_dependency",
            format!("derived artifact {artifact} is stale"),
            "rebuild via wiki-compile / build-pages",
        );
    }

    // --- unregistered derived pages (R10) ---------------------------------
    let edges = dep_edges.clone();
    for sub in [
        "sources",
        "concepts",
        "entities",
        "procedures",
        "questions",
        "decisions",
        "changes",
    ] {
        let subdir = wiki.p(&format!("wiki/{sub}"));
        if !subdir.is_dir() {
            continue;
        }
        for name in sorted_dir_names(&subdir) {
            if !name.ends_with(".md") {
                continue;
            }
            let rel = format!("wiki/{sub}/{name}");
            if !edges.contains_key(&rel) {
                cx.add(
                    "warn",
                    "unregistered_page",
                    format!("derived page {rel} has no dependency registration"),
                    "rebuild via build-pages (script-owned) or re-write via write-page (agent-authored)",
                );
            }
        }
    }

    // --- dependency-edge integrity (read-only mirror of pages.verify) -----
    let items = queue_items(wiki);
    let scan = dep_scan(wiki, &claim_objs, &decision_objs, &items);
    for (artifact, dep_list) in &scan.edges {
        if !wiki.p(artifact).is_file() {
            cx.add(
                "error",
                "missing_artifact",
                format!("registered artifact {artifact} does not exist"),
                "rebuild it, or remove its edge from .state/dependencies.json",
            );
        }
        let Some(deps_arr) = dep_list.as_array() else {
            continue;
        };
        for dep in deps_arr {
            match dep_verdict(wiki, &scan, dep) {
                DepVerdict::Ok => {}
                DepVerdict::VersionStale { dep_id, ver, cur } => {
                    cx.add(
                        "warn",
                        "unresolved_dependency",
                        format!(
                            "dependency {dep_id}@{ver} in {artifact} does not match current version {}",
                            cur.map(|v| v.to_string()).unwrap_or_else(|| "None".to_string())
                        ),
                        "rebuild the artifact so its deps pin real versions",
                    );
                }
                DepVerdict::Dead => {
                    cx.add(
                        "warn",
                        "unresolved_dependency",
                        format!("unresolved dependency {} in {artifact}", py_repr(dep)),
                        "rebuild the artifact or fix .state/dependencies.json",
                    );
                }
            }
        }
    }

    // --- semantic debt (PRD §24) ------------------------------------------
    let under_review: HashSet<String> = items
        .iter()
        .filter(|i| i.get("status").and_then(Value::as_str) == Some("open"))
        .flat_map(|i| str_list(i.get("affected")))
        .map(|a| a.split('@').next().unwrap_or("").to_string())
        .collect();
    for c in &claim_objs {
        let cid = c.get("id").and_then(Value::as_str).unwrap_or("?");
        let status = c.get("status").and_then(Value::as_str).unwrap_or("");
        if status == "accepted" {
            let indep = claims::independence(wiki, c)?;
            if indep == 0
                && c.get("authority")
                    .and_then(|a| a.get("type"))
                    .and_then(Value::as_str)
                    == Some("agent_inference")
            {
                cx.add(
                    "warn",
                    "unsupported_accepted",
                    format!("accepted claim {cid} has no independent evidence"),
                    "corroborate, demote to candidate, or reject via wiki-review",
                );
            }
            if !frontmatter::truthy(c.get("scope")) {
                cx.add(
                    "info",
                    "missing_scope",
                    format!("accepted claim {cid} has empty scope"),
                    "set scope via wiki-review set-scope",
                );
            }
            if !frontmatter::truthy(c.get("valid_from")) {
                cx.add(
                    "info",
                    "missing_effective_date",
                    format!("accepted claim {cid} has no valid_from"),
                    "set validity via wiki-review set-validity",
                );
            }
        }
        if status == "disputed" {
            cx.add(
                "warn",
                "unresolved_contradiction",
                format!("claim {cid} is disputed"),
                "resolve via wiki-review",
            );
        }
        if !frontmatter::truthy(c.get("evidence")) {
            let subject = c.get("subject").map(py_str).unwrap_or_else(|| "-".to_string());
            cx.add(
                "warn",
                "orphan_claim",
                format!("claim {cid} ({subject}) has no evidence"),
                "add evidence via wiki-ingest/compile, or reject via wiki-review",
            );
        }
        if status == "candidate"
            && !under_review.contains(c.get("id").and_then(Value::as_str).unwrap_or(""))
            && age_days(c.get("recorded_at")) > CANDIDATE_AGE_DAYS
        {
            cx.add(
                "warn",
                "stale_candidate",
                format!(
                    "candidate claim {cid} has been unreviewed for {} days",
                    age_days(c.get("recorded_at"))
                ),
                "accept, reject, or defer via wiki-review",
            );
        }
    }

    // near-identical live claims — probable duplicates the reconciler missed.
    let mut dup_groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for c in &claim_objs {
        let status = c.get("status").and_then(Value::as_str).unwrap_or("");
        if TERMINAL.contains(&status) || !frontmatter::truthy(c.get("id")) {
            continue;
        }
        // Group key mirrors the Python tuple (subject, predicate,
        // canonical(scope), canonical(value)) — scope/value are serialized
        // via canonical_json inside the tuple exactly like hashing.py.
        let gkey = canonical_json(&json!([
            c.get("subject").cloned().unwrap_or(Value::Null),
            c.get("predicate").cloned().unwrap_or(Value::Null),
            canonical_json(&c.get("scope").cloned().unwrap_or(json!({}))),
            canonical_json(&c.get("value").cloned().unwrap_or(Value::Null)),
        ]));
        if let Some(id) = c.get("id").and_then(Value::as_str) {
            dup_groups.entry(gkey).or_default().push(id.to_string());
        }
    }
    for ids_in_group in dup_groups.values() {
        if ids_in_group.len() > 1 {
            let mut sorted = ids_in_group.clone();
            sorted.sort();
            cx.add(
                "info",
                "possible_duplicate",
                format!("claims {} share subject/predicate/scope/value", sorted.join(", ")),
                "merge or mark_duplicate via wiki-review",
            );
        }
    }

    for item in &items {
        if item.get("status").and_then(Value::as_str) == Some("open")
            && item.get("kind").and_then(Value::as_str) == Some("possible_contradiction")
        {
            let problem = item.get("problem").map(py_str).unwrap_or_default();
            cx.add(
                "warn",
                "unresolved_contradiction",
                format!("open review item: {problem}"),
                "resolve via wiki-review",
            );
        }
    }

    // --- review queue hygiene (R10) ----------------------------------------
    let mut known_review_ids: HashSet<String> = claim_ids.clone();
    known_review_ids.extend(
        decision_objs
            .iter()
            .filter_map(|d| d.get("id").and_then(Value::as_str).map(String::from)),
    );
    let queue_path = wiki.p(deps::REVIEW_QUEUE);
    if queue_path.is_file() {
        if let Ok(text) = fs::read_to_string(&queue_path) {
            for (lineno, line) in text.lines().enumerate() {
                let line = line.trim();
                if line.is_empty() {
                    continue;
                }
                let lineno = lineno + 1;
                let item: Value = match serde_json::from_str(line) {
                    Ok(v) => v,
                    Err(_) => {
                        cx.add(
                            "error",
                            "malformed_review_item",
                            format!("review-queue line {lineno} is not valid JSON"),
                            &format!("repair or remove .state/review-queue.jsonl line {lineno} (queue is transaction-owned; restore from backup)"),
                        );
                        continue;
                    }
                };
                let missing: Vec<&str> = REVIEW_ITEM_FIELDS
                    .iter()
                    .filter(|k| item.get(**k).is_none())
                    .copied()
                    .collect();
                if !missing.is_empty() {
                    let iid = item.get("id").and_then(Value::as_str).unwrap_or("?");
                    cx.add(
                        "error",
                        "malformed_review_item",
                        format!("review item {iid} missing fields: {}", missing.join(", ")),
                        "restore .state/review-queue.jsonl from backup",
                    );
                    continue;
                }
                for aid in str_list(item.get("affected")) {
                    if !known_review_ids.contains(&aid) {
                        let iid = item.get("id").and_then(Value::as_str).unwrap_or("?");
                        cx.add(
                            "warn",
                            "broken_reference",
                            format!("review item {iid} affected id {aid} no longer exists"),
                            "update or resolve the review item via wiki-review",
                        );
                    }
                }
                if item.get("status").and_then(Value::as_str) == Some("deferred")
                    && age_days(item.get("deferred_at")) > DEFERRED_AGE_DAYS
                {
                    let iid = item.get("id").and_then(Value::as_str).unwrap_or("?");
                    cx.add(
                        "warn",
                        "deferred_aging",
                        format!("review item {iid} deferred for {} days", age_days(item.get("deferred_at"))),
                        "reopen or resolve via wiki-review (defer/reopen)",
                    );
                }
            }
        }
    }

    // low extraction coverage on text-like sources
    if sources_dir.is_dir() {
        let mut sids: Vec<&String> = known_sources.iter().collect();
        sids.sort();
        for sid in sids {
            let xpath = wiki.p(&format!("sources/{sid}/extraction.json"));
            if !xpath.is_file() {
                continue;
            }
            let Ok(ext) = fs::read_to_string(&xpath)
                .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.into()))
            else {
                continue;
            };
            let cov = ext
                .get("extraction_report")
                .and_then(|r| r.get("coverage"))
                .cloned()
                .unwrap_or(Value::Null);
            if cov.get("text").and_then(Value::as_str) == Some("not_extracted") {
                cx.add(
                    "info",
                    "low_extraction_coverage",
                    format!("source {sid} text was not extracted"),
                    "ingest with a document parser for semantic extraction",
                );
            }
        }
    }

    // --- two-tier lint (spec §2) -------------------------------------------
    index_topology_checks(wiki, &mut cx);
    page_graph_checks(wiki, &mut cx);

    // --- topic registry debt ------------------------------------------------
    let treg: Map<String, Value> = match topics::load(wiki) {
        Ok(t) => t.topics,
        Err(e) => {
            cx.add(
                "error",
                "topics_registry_corrupt",
                format!("cannot read topic registry topics.json: {e}"),
                "fix or remove .llm-wiki/topics.json",
            );
            Map::new()
        }
    };
    let conflicts_n = treg
        .values()
        .filter(|e| e.get("status").and_then(Value::as_str) == Some("conflict"))
        .count();
    if conflicts_n > 0 {
        cx.add(
            "warn",
            "topic_kind_conflict",
            format!("{conflicts_n} topic(s) have unresolved kind conflicts"),
            "resolve via review act set_topic_kind",
        );
    }
    let unclaimed = if treg.is_empty() {
        Vec::new()
    } else {
        topics::unclaimed(wiki).unwrap_or_default()
    };
    if !unclaimed.is_empty() {
        cx.add(
            "info",
            "topic_unclaimed",
            format!(
                "{} declared topic(s) have no claims: {}",
                unclaimed.len(),
                unclaimed.iter().take(5).cloned().collect::<Vec<_>>().join(", ")
            ),
            "extract claims for them or accept thin coverage",
        );
    }
    let auto_n = treg
        .values()
        .filter(|e| e.get("auto_registered").and_then(Value::as_bool).unwrap_or(false))
        .count();
    if auto_n > 0 {
        cx.add(
            "info",
            "topics_auto_registered",
            format!("{auto_n} topic(s) auto-registered (kind inferred)"),
            "declare kinds explicitly in topics.jsonl",
        );
    }
    for (subj, e) in &treg {
        let want = e
            .get("kind")
            .and_then(Value::as_str)
            .and_then(|k| topics::PAGE_DIR.iter().find(|(kind, _)| *kind == k))
            .map(|(_, dir)| *dir);
        let Some(want) = want else { continue };
        if e.get("status").and_then(Value::as_str) != Some("resolved") {
            continue;
        }
        let slug = topics::slugify(subj);
        let wikip = wiki.p("wiki");
        if !wikip.is_dir() {
            continue;
        }
        for other in sorted_dir_names(&wikip) {
            let other_p = wikip.join(&other).join(format!("{slug}.md"));
            if other != want
                && other_p.is_file()
                && frontmatter::PAGE_KINDS.contains(&other.as_str())
            {
                let kind = e.get("kind").and_then(Value::as_str).unwrap_or("?");
                cx.add(
                    "error",
                    "topic_page_kind_mismatch",
                    format!(
                        "topic {} is {kind} but page exists at wiki/{other}/{slug}.md",
                        py_repr_str(subj)
                    ),
                    &format!("re-write page at wiki/{want}/{slug}.md or fix the topic kind"),
                );
            }
        }
    }

    // --- FTS retrieval cache health → index_stale ---------------------------
    // Rust analog of doctor.py ftx.health + §9.1 emission rule: a MISSING
    // index.sqlite is normal degradation (no finding); corrupt/behind emits
    // `index_stale`.
    let expected_rev = wiki.revision().unwrap_or(0);
    let probe = index::staleness_probe(wiki).unwrap_or(None);
    let fts_h = match probe {
        None => json!({
            "usable": false, "rows": 0, "revision": Value::Null,
            "expected_revision": expected_rev, "reason": "missing",
        }),
        Some(built) => {
            let rows: i64 = index_rows_count(wiki);
            if built == expected_rev {
                json!({
                    "usable": true, "rows": rows, "revision": built,
                    "expected_revision": expected_rev, "reason": "ok",
                })
            } else if built == -1 {
                json!({
                    "usable": false, "rows": 0, "revision": Value::Null,
                    "expected_revision": expected_rev, "reason": "corrupt",
                })
            } else {
                json!({
                    "usable": false, "rows": rows, "revision": built,
                    "expected_revision": expected_rev, "reason": "stale_revision",
                })
            }
        }
    };
    if probe.is_some() && fts_h["usable"] == Value::Bool(false) {
        cx.add(
            "warn",
            "index_stale",
            format!(
                "index {} (rows={}, built_at_revision={}, manifest={}); readers fall back to the canonical scan",
                fts_h["reason"].as_str().unwrap_or("stale"),
                fts_h["rows"],
                fts_h["revision"],
                fts_h["expected_revision"],
            ),
            "rebuild index.sqlite via doctor --fix --code index_stale or llm-wiki doctor --rebuild-index",
        );
    }

    // --- legacy derived files (spec §10.3) ----------------------------------
    for rel in LEGACY_DERIVED {
        if wiki.p(rel).is_file() {
            cx.add(
                "info",
                "legacy_state",
                format!("legacy derived file {rel} is ignored by the Rust engine"),
                "delete it — `doctor --fix --code legacy_state`; content is rebuilt from canonical",
            );
        }
    }

    // --- sort + report --------------------------------------------------------
    let rank = |s: &str| match s {
        "error" => 0,
        "warn" => 1,
        _ => 2,
    };
    cx.findings.sort_by(|a, b| {
        (rank(a.severity), a.code).cmp(&(rank(b.severity), b.code))
    });
    let (ok, counts) = count_findings(&cx.findings);
    let mut plan: Vec<PlanItem> = cx.plan;
    plan.sort_by(|a, b| (&a.code, &a.path).cmp(&(&b.code, &b.path)));
    Ok(Report {
        ok,
        findings: cx.findings,
        counts,
        fts: fts_h,
        extractors: extractor_capability(),
        repair_plan: plan.iter().map(PlanItem::to_value).collect(),
    })
}

/// `.state/index.sqlite` claims_fts row count — best-effort for the `fts`
/// health tuple; 0 on any failure (health detail, never authoritative).
fn index_rows_count(wiki: &Wiki) -> i64 {
    let path = wiki.p(".state/index.sqlite");
    let Ok(conn) = rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) else {
        return 0;
    };
    conn.query_row("SELECT COUNT(*) FROM claims_fts", [], |r| r.get(0))
        .unwrap_or(0)
}

fn collect_wiki_files(root: &Path, dir: &Path, live: &mut HashSet<String>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for entry in rd.flatten() {
        let path = entry.path();
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_dir() {
            collect_wiki_files(root, &path, live);
        } else if ft.is_file() {
            if let Ok(rel) = path.strip_prefix(root) {
                live.insert(format!("wiki/{}", rel.to_string_lossy().replace('\\', "/")));
            }
        }
    }
}

fn lock_would_block(e: &std::io::Error) -> bool {
    if e.kind() == std::io::ErrorKind::WouldBlock {
        return true;
    }
    #[cfg(unix)]
    {
        matches!(e.raw_os_error(), Some(11) | Some(13))
    }
    #[cfg(not(unix))]
    {
        false
    }
}

/// doctor.py `_extractor_capability` — markitdown availability probe.
fn extractor_capability() -> Value {
    let via = find_on_path("markitdown");
    json!({
        "markitdown": {
            "available": via,
            "via": if via { json!("cli") } else { Value::Null },
            "formats": {
                "application/pdf": if via { "extracted" } else { "agent-handoff" },
                ".docx": if via { "extracted" } else { "agent-handoff" },
                ".xlsx": if via { "extracted" } else { "agent-handoff" },
            },
            "remediation": if via { Value::Null } else { json!("pip install markitdown (optional)") },
        }
    })
}

fn find_on_path(exe: &str) -> bool {
    let Ok(path_var) = std::env::var("PATH") else { return false };
    std::env::split_paths(&path_var)
        .any(|dir| dir.join(exe).is_file())
}

// ---------------------------------------------------------------------
// Tier 1 — index-fabric lint (doctor.py `_index_topology_checks`).
// ---------------------------------------------------------------------

fn index_topology_checks(wiki: &Wiki, cx: &mut Ctx) {
    let overview = read_text(&wiki.p("wiki/overview.md")).unwrap_or_default();
    let md_links = md_link_re();
    for section in PAGE_SECTIONS {
        let sec_dir = wiki.p(&format!("wiki/{section}"));
        if !sec_dir.is_dir() {
            continue;
        }
        let index_rel = format!("wiki/{section}/index.md");
        let index_body = read_text(&wiki.p(&index_rel)).unwrap_or_default();
        let index_links: HashSet<String> = md_links
            .captures_iter(&index_body)
            .map(|m| m[1].to_string())
            .collect();
        let linked_from_overview = overview.contains(&format!("{section}/index.md"));
        if wiki.p(&index_rel).is_file() && !linked_from_overview {
            cx.add(
                "warn",
                "section_index_unlinked",
                format!("wiki/{section}/index.md exists but overview.md does not link it"),
                "rebuild pages (build-pages regenerates overview) or add the section link to overview.md",
            );
            cx.plan_add(
                "section_index_unlinked",
                &index_rel,
                format!(
                    "run `wiki.py doctor --fix-indexes` to regenerate overview.md and restore the [[{}]] section link",
                    wiki_ref(&index_rel)
                ),
            );
        }
        for name in sorted_dir_names(&sec_dir) {
            if !name.ends_with(".md") || name == "index.md" {
                continue;
            }
            if !index_links.contains(&name) {
                let rel = format!("wiki/{section}/{name}");
                cx.add(
                    "warn",
                    "unlisted_page",
                    format!("wiki/{section}/{name} is not listed in its section index"),
                    "rebuild pages (build-pages regenerates section indexes) or list the page in its section index",
                );
                cx.plan_add(
                    "unlisted_page",
                    &rel,
                    format!(
                        "run `wiki.py doctor --fix-indexes` to regenerate wiki/{section}/index.md so it lists [[{}]]",
                        wiki_ref(&rel)
                    ),
                );
            }
        }
    }
}

// ---------------------------------------------------------------------
// Tier 2 — page-graph lint (doctor.py `_page_graph_checks`), all warn.
// ---------------------------------------------------------------------

/// Page universe: agent-authored section pages — kind in PAGE_KINDS,
/// index.md excluded (write_page domain). Shared by page_graph_checks and
/// the §9.1 malformed_frontmatter fix scan.
fn page_universe(wiki: &Wiki) -> Vec<String> {
    let edges: Map<String, Value> = match deps::graph(wiki) {
        Ok(g) => g
            .get("edges")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
        Err(_) => Map::new(),
    };
    let mut arts: Vec<String> = edges
        .keys()
        .filter(|a| {
            a.starts_with("wiki/")
                && a.matches('/').count() == 2
                && a.ends_with(".md")
                && a.split('/').nth(1).map(|k| frontmatter::PAGE_KINDS.contains(&k)).unwrap_or(false)
                && a.rsplit('/').next() != Some("index.md")
        })
        .cloned()
        .collect();
    for kind in frontmatter::PAGE_KINDS {
        let d = wiki.p(&format!("wiki/{kind}"));
        if d.is_dir() {
            for n in sorted_dir_names(&d) {
                let rel = format!("wiki/{kind}/{n}");
                if n.ends_with(".md") && n != "index.md" && !arts.contains(&rel) {
                    arts.push(rel);
                }
            }
        }
    }
    arts.sort();
    arts.dedup();
    arts
}

fn page_graph_checks(wiki: &Wiki, cx: &mut Ctx) {
    let edges: Map<String, Value> = match deps::graph(wiki) {
        Ok(g) => g
            .get("edges")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
        Err(_) => Map::new(),
    };
    let arts = page_universe(wiki);


    // Parse every page once. Malformed frontmatter is an error finding and
    // removes the page from the graph — diagnostics never crash on it.
    let mut fronts: HashMap<String, Map<String, Value>> = HashMap::new();
    let mut bodies: HashMap<String, String> = HashMap::new();
    let mut valid: Vec<String> = Vec::new();
    for a in &arts {
        let Some(text) = read_text(&wiki.p(a)) else { continue };
        let parsed = frontmatter::split_front(&text);
        // Unterminated "---" opener (no closing fence) is malformed too —
        // synthesize._split_front just finds no frontmatter on it (the
        // whole text is returned as body).
        let opener_unclosed = text.starts_with("---")
            && parsed.as_ref().map(|(_, body)| body.len() == text.len()).unwrap_or(true);
        match parsed {
            Ok((front, body)) if !opener_unclosed => {
                fronts.insert(a.clone(), front);
                bodies.insert(a.clone(), body.to_string());
                valid.push(a.clone());
            }
            _ => {
                cx.add(
                    "error",
                    "malformed_frontmatter",
                    format!("{a} has malformed frontmatter"),
                    "fix the --- frontmatter block or rewrite via write-page",
                );
                cx.plan_add(
                    "malformed_frontmatter",
                    a,
                    format!(
                        "fix the --- frontmatter block of [[{}]] or rewrite the page via write-page",
                        wiki_ref(a)
                    ),
                );
            }
        }
    }
    let page_set: HashSet<String> = valid.iter().cloned().collect();

    let front_deps = |front: &Map<String, Value>| -> Vec<String> {
        match front.get("deps") {
            Some(Value::String(s)) => vec![s.clone()],
            Some(Value::Array(a)) => a.iter().map(py_str).collect(),
            _ => Vec::new(),
        }
    };

    let wiki_link = wiki_link_re();
    let mut out_e: HashMap<String, Vec<String>> = HashMap::new();
    for a in &valid {
        let mut targets: HashSet<String> = HashSet::new();
        let body = &bodies[a];
        for r in extract_page_links(body) {
            targets.insert(format!("wiki/{r}.md"));
        }
        for d in front_deps(&fronts[a]) {
            if let Some(rest) = d.strip_prefix("wiki:") {
                targets.insert(format!("wiki/{rest}.md"));
            } else if d.starts_with("wiki/") {
                targets.insert(d.split('@').next().unwrap_or("").to_string());
            }
        }
        for d in edges.get(a).and_then(Value::as_array).cloned().unwrap_or_default() {
            if let Value::String(ds) = &d {
                let mut dd = ds.split('@').next().unwrap_or("").to_string();
                if let Some(rest) = dd.strip_prefix("wiki:") {
                    dd = format!("wiki/{rest}.md");
                }
                if dd.starts_with("wiki/") {
                    targets.insert(dd);
                }
            }
        }
        let mut sorted: Vec<String> = targets
            .into_iter()
            .filter(|d| page_set.contains(d) && d != a)
            .collect();
        sorted.sort();
        out_e.insert(a.clone(), sorted);
    }
    let mut in_e: HashMap<String, Vec<String>> = HashMap::new();
    for a in &valid {
        in_e.insert(a.clone(), Vec::new());
    }
    for (a, ds) in &out_e {
        for d in ds {
            in_e.entry(d.clone()).or_default().push(a.clone());
        }
    }
    for ds in in_e.values_mut() {
        ds.sort();
    }

    for a in &valid {
        // An orphan needs a graph to be orphaned from: never flag a
        // single-page wiki.
        if valid.len() > 1 && in_e[a].is_empty() {
            cx.add(
                "warn",
                "orphan_page",
                format!("{a} has no inbound page links"),
                "link it from a related page or a question page",
            );
            cx.plan_add(
                "orphan_page",
                a,
                format!(
                    "add a [[{}]] link to a related page's See-also section and deps",
                    wiki_ref(a)
                ),
            );
        }
        if out_e[a].is_empty() {
            let cites = wiki_link.is_match(&bodies[a])
                || front_deps(&fronts[a])
                    .iter()
                    .any(|d| d.split('@').next().unwrap_or("").starts_with("claim_"));
            let hint = if cites {
                "cites claims but has no outbound [[wiki:]] page links"
            } else {
                "links to no other page"
            };
            cx.add(
                "warn",
                "dead_end_page",
                format!("{a} {hint}"),
                "add a See also section with [[wiki:kind/slug]] links",
            );
            cx.plan_add(
                "dead_end_page",
                a,
                format!(
                    "add a See-also section to [[{}]] with [[wiki:kind/slug]] links to related pages and register them in deps",
                    wiki_ref(a)
                ),
            );
        }
    }

    // walled gardens: undirected components of >=3 pages; the single
    // largest component is the wiki's main body and is never a garden.
    let mut undir: HashMap<String, HashSet<String>> = HashMap::new();
    for (a, ds) in &out_e {
        undir.insert(a.clone(), ds.iter().cloned().collect());
    }
    for (a, ds) in &in_e {
        undir.entry(a.clone()).or_default().extend(ds.iter().cloned());
    }
    let mut comps: Vec<Vec<String>> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for a in valid.iter().map(|s| s.clone()).collect::<Vec<_>>() {
        if seen.contains(&a) {
            continue;
        }
        let mut comp: HashSet<String> = HashSet::new();
        let mut stack = vec![a.clone()];
        while let Some(x) = stack.pop() {
            if !comp.contains(&x) {
                comp.insert(x.clone());
                for n in undir.get(&x).cloned().unwrap_or_default() {
                    if !comp.contains(&n) {
                        stack.push(n);
                    }
                }
            }
        }
        let mut members: Vec<String> = comp.into_iter().collect();
        members.sort();
        for m in &members {
            seen.insert(m.clone());
        }
        comps.push(members);
    }
    if comps.len() > 1 {
        // Deterministic largest: size first, member list as tiebreak.
        let main = comps
            .iter()
            .max_by(|x, y| (x.len(), x).cmp(&(y.len(), y)))
            .cloned()
            .unwrap_or_default();
        for members in &comps {
            if members.len() >= 3 && *members != main {
                cx.add(
                    "warn",
                    "walled_garden",
                    format!("{} pages only link to each other: {}", members.len(), members.join(", ")),
                    "cross-link the cluster to the rest of the wiki",
                );
                cx.plan_add(
                    "walled_garden",
                    &members[0],
                    format!(
                        "link [[{}]] into the main component by adding a [[wiki:kind/slug]] edge to a related page outside the cluster",
                        wiki_ref(&members[0])
                    ),
                );
            }
        }
    }

    // near-duplicate subjects: title+subject token Jaccard, pairs not
    // already linked in either direction.
    let tokens: HashMap<String, HashSet<String>> = valid
        .iter()
        .map(|a| {
            let front = &fronts[a];
            let mut t = normalize_tokens(front.get("title").unwrap_or(&Value::Null));
            t.extend(normalize_tokens(front.get("subject").unwrap_or(&Value::Null)));
            (a.clone(), t)
        })
        .collect();
    for (a, b) in candidate_pairs(&build_token_index(&tokens)) {
        let jac = jaccard(&tokens[&a], &tokens[&b]);
        if jac >= NEAR_DUP_LINT_THRESHOLD
            && !out_e[&a].contains(&b)
            && !out_e[&b].contains(&a)
        {
            cx.add(
                "warn",
                "near_duplicate_subject",
                format!("{a} and {b} have near-identical subjects (Jaccard {jac:.2})"),
                "merge the pages or link them explicitly",
            );
            cx.plan_add(
                "near_duplicate_subject",
                &a,
                format!(
                    "extend [[{}]] to cover the topic or differentiate [[{}]]'s subject",
                    wiki_ref(&b),
                    wiki_ref(&a)
                ),
            );
        }
    }

    let today = today();
    for a in &valid {
        let raw = fronts[a].get("stale_after");
        let Some(raw_s) = raw.and_then(Value::as_str) else { continue };
        let Some(cutoff) = parse_date(raw_s.trim()) else { continue };
        if cutoff < today {
            cx.add(
                "warn",
                "stale_page_ttl",
                format!("{a} passed its stale_after date {raw_s}"),
                "refresh the page or bump stale_after",
            );
            cx.plan_add(
                "stale_page_ttl",
                a,
                format!(
                    "refresh [[{}]] against current claims or bump its stale_after date",
                    wiki_ref(a)
                ),
            );
        }
    }
}

// =====================================================================
// doctor --fix (spec §9) — FIXABLE table, one-transaction execution,
// post-commit index rebuild. Deferred codes (§9.4) never appear here.
// =====================================================================

/// Shared dep-integrity scan — computed once for the read path (run) and
/// once for the fix path so both classify deps identically (§9.1 split).
struct DepScan {
    edges: Map<String, Value>,
    known_versions: HashMap<String, Option<i64>>,
    queue_ids: HashSet<String>,
    live: HashSet<String>,
}

fn dep_scan(wiki: &Wiki, claim_objs: &[Value], decision_objs: &[Value], items: &[Value]) -> DepScan {
    let mut known_versions: HashMap<String, Option<i64>> = HashMap::new();
    for c in claim_objs {
        if let Some(id) = c.get("id").and_then(Value::as_str) {
            known_versions.insert(id.to_string(), c.get("version").and_then(Value::as_i64));
        }
    }
    for d in decision_objs {
        if let Some(id) = d.get("id").and_then(Value::as_str) {
            known_versions.insert(id.to_string(), d.get("version").and_then(Value::as_i64));
        }
    }
    let queue_ids: HashSet<String> = items
        .iter()
        .filter_map(|i| i.get("id").and_then(Value::as_str).map(String::from))
        .collect();
    let edges: Map<String, Value> = match deps::graph(wiki) {
        Ok(g) => g
            .get("edges")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default(),
        Err(_) => Map::new(),
    };
    // dep-liveness set (dead-vs-live, §9.1): canonical ids + wiki/* +
    // review_* namespaces; falls back to the locally resolved ids when the
    // shared probe trips on a corrupt claim file (doctor never crashes).
    let live = deps::live_ids(wiki).unwrap_or_else(|_| {
        let mut live: HashSet<String> = known_versions.keys().cloned().collect();
        live.extend(queue_ids.iter().cloned());
        live.extend(edges.keys().filter(|k| k.starts_with("wiki/")).cloned());
        collect_wiki_files(&wiki.p("wiki"), &wiki.p("wiki"), &mut live);
        live
    });
    DepScan {
        edges,
        known_versions,
        queue_ids,
        live,
    }
}

/// Per-dep verdict mirroring the read-path detector exactly.
enum DepVerdict {
    Ok,
    /// Live canonical id pinned at a non-current version — §9.1: rebuild
    /// (repin), NEVER drop.
    VersionStale {
        dep_id: String,
        ver: String,
        cur: Option<i64>,
    },
    /// Dead dep — drop the edge (dream edge_drops rule).
    Dead,
}

fn dep_verdict(wiki: &Wiki, scan: &DepScan, dep: &Value) -> DepVerdict {
    let dep_s = dep_str(dep);
    let dep_s = dep_s.trim();
    let (dep_id, ver) = match dep_s.split_once('@') {
        Some((id, v)) => (id.to_string(), v.to_string()),
        None => (dep_s.to_string(), String::new()),
    };
    if dep_id.starts_with("wiki/") {
        if !(scan.edges.contains_key(&dep_id) || wiki.p(&dep_id).is_file()) {
            DepVerdict::Dead
        } else {
            DepVerdict::Ok
        }
    } else if dep_id.starts_with("review_") {
        if !scan.queue_ids.contains(&dep_id) {
            DepVerdict::Dead
        } else {
            DepVerdict::Ok
        }
    } else if scan.known_versions.contains_key(&dep_id) {
        let cur = scan.known_versions[&dep_id];
        if !ver.is_empty()
            && (!ver.chars().all(|c| c.is_ascii_digit()) || ver.parse::<i64>().ok() != cur)
        {
            DepVerdict::VersionStale { dep_id, ver, cur }
        } else {
            DepVerdict::Ok
        }
    } else {
        // dead-vs-live: an id outside the live set is a dead dep.
        // doctor.py flags even an empty dep id as unresolved — the live
        // set never contains it (live_ids is additive: an
        // unexpected-but-live wiki/review id stays resolved).
        if dep_id.is_empty() || !deps::is_live_dep(&dep_id, &scan.live) {
            DepVerdict::Dead
        } else {
            DepVerdict::Ok
        }
    }
}

/// `true` for codes with an applicable fix in v1 — the §9.1 table.
/// Everything else (§9.4) is detect-only: it can appear in `remaining[]`
/// but never in `fixed[]`.
fn is_fixable(code: &str) -> bool {
    matches!(
        code,
        "unlisted_page"
            | "section_index_unlinked"
            | "unresolved_dependency"
            | "missing_artifact"
            | "staging_leftover"
            | "transaction_journal_leftover"
            | "stale_transaction_backup"
            | "malformed_frontmatter"
            | "fts_stale"
            | "index_stale"
            | "legacy_state"
            | "stale_lock"
    )
}

/// Artifact rel path extracted from a finding's detail text — the detector
/// embeds the path verbatim at a known position per code, so this is a
/// co-located inverse, not a heuristic.
fn detail_artifact(f: &Finding) -> Option<String> {
    let d = &f.detail;
    match f.code {
        "unlisted_page" => d.split(' ').next().map(str::to_string),
        "section_index_unlinked" => d.split(' ').next().map(str::to_string),
        "malformed_frontmatter" => d.split(' ').next().map(str::to_string),
        "staging_leftover" => d
            .strip_prefix("staging run ")
            .map(|s| format!(".state/staging/{}", s.split(' ').next().unwrap_or(""))),
        "stale_transaction_backup" => d
            .strip_prefix("transaction backup dir ")
            .map(|s| s.split(' ').next().unwrap_or("").to_string()),
        "stale_lock" => d
            .strip_prefix("unexpected file ")
            .map(str::to_string),
        "legacy_state" => d
            .strip_prefix("legacy derived file ")
            .map(|s| s.split(' ').next().unwrap_or("").to_string()),
        "index_stale" | "fts_stale" => Some(".state/index.sqlite".to_string()),
        "transaction_journal_leftover" => Some(".state/journal.json".to_string()),
        "missing_artifact" => d
            .strip_prefix("registered artifact ")
            .and_then(|s| s.strip_suffix(" does not exist"))
            .map(str::to_string),
        "unresolved_dependency" => {
            if let Some(rest) = d.strip_prefix("dependency ") {
                rest.split_once(" in ")
                    .map(|(dep, rest)| (dep, rest.split_once(" does not match").map(|x| x.0)))
                    .and_then(|(_dep, art)| art.map(str::to_string))
            } else {
                d.strip_prefix("unresolved dependency ")
                    .and_then(|rest| rest.rsplit_once(" in ").map(|(_, art)| art.to_string()))
            }
        }
        _ => None,
    }
}

/// A planned fix — rel paths only, so the identical plan applies to the
/// real wiki or a --dry-run scratch copy.
#[derive(Debug)]
enum FixOp {
    /// §5.4 repair rule over journal/backup leftovers (txn::recovery::repair).
    RepairLeftovers,
    /// rebuild every section index + overview (build-pages renderer).
    RebuildSectionIndexes,
    /// regenerate a script-owned artifact (missing_artifact / dep repin).
    RebuildArtifact { rel: String },
    /// `edges[artifact].remove(dep)` — dream edge_drops semantics.
    DropDep { artifact: String, dep: String },
    /// `edges[artifact]` dep `old` → `new` for agent-authored pages (the
    /// registry repin that stands in for "rebuild the artifact").
    RepinDep { artifact: String, old: String, new: String },
    /// `edges.remove(artifact)` — non-regenerable missing artifact.
    DropArtifactEdge { artifact: String },
    /// default title/type/tags rewrite, body preserved.
    RewriteFrontmatter { rel: String },
    /// `stage_delete(rel)` in the fix txn.
    DeleteFile { rel: String },
    /// dead staging run dir — post-commit `remove_dir_all` (§5.5).
    RemoveStagingDir { rel: String },
    /// index.sqlite rebuild, post-commit under index.lock (§9.2).
    RebuildIndex,
}

/// One `fixed[]` record: `{code, artifact, action}`.
#[derive(Debug)]
struct FixRecord {
    code: &'static str,
    artifact: String,
    action: String,
}

impl FixRecord {
    fn to_value(&self) -> Value {
        json!({
            "code": self.code,
            "artifact": self.artifact,
            "action": self.action,
        })
    }
}

#[derive(Default)]
struct FixPlan {
    ops: Vec<FixOp>,
    records: Vec<FixRecord>,
}

/// The §9.1 selection pass: findings are already scope-filtered; `codes`
/// (empty = all) restricts which finding codes get fixed. Fix targets are
/// re-derived from live state rather than trusting detail strings for
/// dep-level precision; detail strings supply only the artifact path for
/// codes whose emitter wrote it verbatim.
fn collect_fixes(wiki: &Wiki, findings: &[Finding], codes: &[String]) -> Result<FixPlan, EngineError> {
    let want = |code: &str| codes.is_empty() || codes.iter().any(|c| c == code);
    let mut plan = FixPlan::default();
    let mut dead_arts: HashSet<String> = HashSet::new();
    let mut stale_arts: HashSet<String> = HashSet::new();
    let mut missing_arts: HashSet<String> = HashSet::new();
    let mut malformed: HashSet<String> = HashSet::new();
    let mut repair_selected = false;

    for f in findings {
        if !is_fixable(f.code) || !want(f.code) {
            continue;
        }
        match f.code {
            "unlisted_page" | "section_index_unlinked" => {
                plan.records.push(FixRecord {
                    code: f.code,
                    artifact: detail_artifact(f).unwrap_or_default(),
                    action: "rebuild_section_indexes".to_string(),
                });
            }
            "unresolved_dependency" => {
                let Some(art) = detail_artifact(f) else { continue };
                if f.detail.contains("does not match current version") {
                    stale_arts.insert(art);
                } else {
                    dead_arts.insert(art);
                }
            }
            "missing_artifact" => {
                if let Some(art) = detail_artifact(f) {
                    missing_arts.insert(art);
                }
            }
            "staging_leftover" => {
                let Some(rel) = detail_artifact(f) else { continue };
                let dir = wiki.p(&rel);
                // §5.5: delete iff the run is dead (live-phase + 24h floor).
                if dir.is_dir() && !dream::staging_run_live(&dir) {
                    plan.ops.push(FixOp::RemoveStagingDir { rel: rel.clone() });
                    plan.records.push(FixRecord {
                        code: f.code,
                        artifact: rel,
                        action: "delete_staging_dir".to_string(),
                    });
                }
            }
            "transaction_journal_leftover" | "stale_transaction_backup" => {
                repair_selected = true;
                plan.records.push(FixRecord {
                    code: f.code,
                    artifact: detail_artifact(f).unwrap_or_default(),
                    action: "repair".to_string(),
                });
            }
            "malformed_frontmatter" => {
                if let Some(rel) = detail_artifact(f) {
                    malformed.insert(rel);
                }
            }
            "index_stale" | "fts_stale" => {
                plan.ops.push(FixOp::RebuildIndex);
                plan.records.push(FixRecord {
                    code: f.code,
                    artifact: detail_artifact(f).unwrap_or_default(),
                    action: "rebuild_index".to_string(),
                });
            }
            "legacy_state" | "stale_lock" => {
                let Some(rel) = detail_artifact(f) else { continue };
                if rel == ".state/locks/commit.lock" || rel == ".state/locks/index.lock" {
                    continue; // §5.6: engine-owned locks are never fix targets
                }
                if wiki.p(&rel).is_file() {
                    plan.ops.push(FixOp::DeleteFile { rel: rel.clone() });
                    plan.records.push(FixRecord {
                        code: f.code,
                        artifact: rel,
                        action: "delete".to_string(),
                    });
                }
            }
            _ => {}
        }
    }

    // Journal/backup leftovers the detector doesn't enumerate (run-scoped
    // `journal-<run>.json` files) still trip the §5.4 repair gate — repair
    // covers them whenever the repair codes are selected.
    if repair_selected && txn::recovery::repair_needed(wiki) {
        for l in txn::recovery::scan(wiki) {
            match l {
                txn::recovery::Leftover::Journal(p) => {
                    let rel = format!(".state/{}", p.file_name().unwrap().to_string_lossy());
                    if !plan.records.iter().any(|r| r.artifact == rel) {
                        plan.records.push(FixRecord {
                            code: "transaction_journal_leftover",
                            artifact: rel,
                            action: "repair".to_string(),
                        });
                    }
                }
                txn::recovery::Leftover::BackupDir(p) => {
                    let rel = format!(".state/{}", p.file_name().unwrap().to_string_lossy());
                    if !plan.records.iter().any(|r| r.artifact == rel) {
                        plan.records.push(FixRecord {
                            code: "stale_transaction_backup",
                            artifact: rel,
                            action: "repair".to_string(),
                        });
                    }
                }
                txn::recovery::Leftover::Staging(_) => {}
            }
        }
    }
    if repair_selected {
        plan.ops.push(FixOp::RepairLeftovers);
    }

    // --- dep-edge fixes -----------------------------------------------------
    if !dead_arts.is_empty() || !stale_arts.is_empty() || !missing_arts.is_empty() {
        let claim_objs = claims::list(wiki)?;
        let decision_objs = claims::list_decisions(wiki)?;
        let items = queue_items(wiki);
        let scan = dep_scan(wiki, &claim_objs, &decision_objs, &items);
        let mut rebuilt: HashSet<String> = HashSet::new();
        let mut dropped_edges: HashSet<String> = HashSet::new();
        // Regenerable = script-owned AND renderable right now: a decision
        // page whose canonical decisions/<did>.json is gone cannot be
        // re-rendered, so it takes the drop_edge branch like agent content.
        let regenerable = |art: &str| -> bool {
            if !pages::is_script_owned(art) {
                return false;
            }
            if art.starts_with("wiki/decisions/") && art.ends_with(".md") {
                let did = &art[15..art.len() - 3];
                return wiki
                    .p(&format!("decisions/{did}.json"))
                    .is_file();
            }
            true
        };
        // missing_artifact split: regenerable → rebuild, else drop edge.
        let mut missing_sorted: Vec<String> = missing_arts.iter().cloned().collect();
        missing_sorted.sort();
        for art in &missing_sorted {
            if !scan.edges.contains_key(art) {
                continue;
            }
            if regenerable(art) {
                rebuilt.insert(art.clone());
                plan.ops.push(FixOp::RebuildArtifact { rel: art.clone() });
                plan.records.push(FixRecord {
                    code: "missing_artifact",
                    artifact: art.clone(),
                    action: "rebuild_artifact".to_string(),
                });
            } else {
                dropped_edges.insert(art.clone());
                plan.ops.push(FixOp::DropArtifactEdge {
                    artifact: art.clone(),
                });
                plan.records.push(FixRecord {
                    code: "missing_artifact",
                    artifact: art.clone(),
                    action: "drop_edge".to_string(),
                });
            }
        }

        // unresolved_dependency split — dead → drop dep edge; version-stale
        // → rebuild (repin). A missing script-owned artifact being rebuilt
        // absorbs its dep findings; a dropped artifact's deps go with it.
        let mut dep_arts: Vec<String> = dead_arts
            .iter()
            .chain(stale_arts.iter())
            .filter(|a| !dropped_edges.contains(*a))
            .cloned()
            .collect();
        dep_arts.sort();
        dep_arts.dedup();
        for art in &dep_arts {
            let regen = rebuilt.contains(art)
                || (!missing_arts.contains(art) && regenerable(art));
            let Some(deps_arr) = scan.edges.get(art).and_then(Value::as_array) else {
                continue;
            };
            for dep in deps_arr {
                match dep_verdict(wiki, &scan, dep) {
                    DepVerdict::Ok => {}
                    DepVerdict::Dead => {
                        if !dead_arts.contains(art) {
                            continue; // finding wasn't emitted for this dep
                        }
                        let dep_s = dep_str(dep).trim().to_string();
                        let action = if regen {
                            // §8 ghost order: rebuild regenerates the dep
                            // list anyway; still stage the drop so the edge
                            // is gone even when the render keeps it.
                            rebuilt.insert(art.clone());
                            plan.ops.push(FixOp::RebuildArtifact { rel: art.clone() });
                            "rebuild_artifact"
                        } else {
                            plan.ops.push(FixOp::DropDep {
                                artifact: art.clone(),
                                dep: dep_s,
                            });
                            "drop_edge"
                        };
                        plan.records.push(FixRecord {
                            code: "unresolved_dependency",
                            artifact: art.clone(),
                            action: action.to_string(),
                        });
                    }
                    DepVerdict::VersionStale { dep_id, cur, .. } => {
                        if !stale_arts.contains(art) {
                            continue;
                        }
                        let action = if regen {
                            rebuilt.insert(art.clone());
                            plan.ops.push(FixOp::RebuildArtifact { rel: art.clone() });
                            "rebuild_artifact"
                        } else {
                            let dep_s = dep_str(dep).trim().to_string();
                            let new = match cur {
                                Some(v) => format!("{dep_id}@{v}"),
                                None => dep_id.clone(),
                            };
                            plan.ops.push(FixOp::RepinDep {
                                artifact: art.clone(),
                                old: dep_s,
                                new,
                            });
                            "repin_dep_edge"
                        };
                        plan.records.push(FixRecord {
                            code: "unresolved_dependency",
                            artifact: art.clone(),
                            action: action.to_string(),
                        });
                    }
                }
            }
        }
    }

    // RebuildSectionIndexes once when any index-fabric finding was selected.
    if plan.records.iter().any(|r| r.action == "rebuild_section_indexes") {
        plan.ops.push(FixOp::RebuildSectionIndexes);
    }

    // malformed_frontmatter — re-scan the page universe (verbatim detector
    // rule) so only truly-malformed pages get the default rewrite.
    if !malformed.is_empty() {
        for a in page_universe(wiki) {
            if !malformed.contains(&a) {
                continue;
            }
            let Some(text) = read_text(&wiki.p(&a)) else { continue };
            let parsed = frontmatter::split_front(&text);
            let opener_unclosed = text.starts_with("---")
                && parsed.as_ref().map(|(_, body)| body.len() == text.len()).unwrap_or(true);
            if !(parsed.is_err() || opener_unclosed) {
                continue; // page was repaired between detect and fix
            }
            plan.ops.push(FixOp::RewriteFrontmatter { rel: a.clone() });
            plan.records.push(FixRecord {
                code: "malformed_frontmatter",
                artifact: a,
                action: "rewrite_frontmatter_defaults".to_string(),
            });
        }
    }

    Ok(plan)
}

/// Execute `plan` against `wiki` — the §9.2 model: §5.4 leftovers first
/// (they gate every commit), then ONE transaction for file fixes, then
/// post-commit staging-dir removal + the index rebuild. Returns the
/// `fixed[]` records (planned ops that ran) — callers re-lint for
/// `remaining[]`.
fn apply_fixes(wiki: &Wiki, plan: FixPlan) -> Result<Vec<Value>, EngineError> {
    let needs_repair = plan.ops.iter().any(|op| matches!(op, FixOp::RepairLeftovers));
    // §9.2: index rebuild is post-commit for the §9.1 index codes only —
    // the section-index rebuild rewrites wiki/*.md, not claim rows.
    let needs_index = plan.ops.iter().any(|op| matches!(op, FixOp::RebuildIndex));
    let staging_dirs: Vec<String> = plan
        .ops
        .iter()
        .filter_map(|op| match op {
            FixOp::RemoveStagingDir { rel } => Some(rel.clone()),
            _ => None,
        })
        .collect();
    let in_txn: Vec<&FixOp> = plan
        .ops
        .iter()
        .filter(|op| {
            matches!(
                op,
                FixOp::RebuildSectionIndexes
                    | FixOp::RebuildArtifact { .. }
                    | FixOp::DropDep { .. }
                    | FixOp::RepinDep { .. }
                    | FixOp::DropArtifactEdge { .. }
                    | FixOp::RewriteFrontmatter { .. }
                    | FixOp::DeleteFile { .. }
            )
        })
        .collect();

    // §5.4 repair first: a leftover journal/backup gates Txn::commit, and
    // doctor --fix is the documented way out (spec §5.4). restore-then-
    // delete for backups; journal-only → delete journals + staging.
    if needs_repair {
        txn::recovery::repair(wiki)?;
    }

    // malformed_frontmatter rewrites land BEFORE the txn: the build-pages
    // renderers title-probe every section file at STAGE time and a
    // yamlite-unclean page would abort staging (the fix exists precisely
    // to make it clean). Defaults are title/type/tags only — body verbatim.
    // The same bytes are staged in-txn below so the write is journaled.
    for op in &in_txn {
        if let FixOp::RewriteFrontmatter { rel } = op {
            let content = frontmatter_default_rewrite(wiki, rel)?;
            crate::util::atomic_write_checked(&wiki.p(rel), &wiki.root, content.as_bytes())?;
        }
    }

    let mut committed_rev: Option<i64> = None;
    if !in_txn.is_empty() {
        let mut t = Txn::begin_with(
            wiki,
            &txn::mint_run_id(),
            TxnOpts {
                command: Some("wiki-doctor-fix".to_string()),
                // spec §5.4: doctor --fix is always exempt from the repair
                // gate (repair runs above when its codes are selected).
                allow_repair: true,
            },
        )?;

        // index-fabric rebuild — once, ahead of artifact rebuilds so a
        // re-rendered section index isn't clobbered by a later write.
        if in_txn
            .iter()
            .any(|op| matches!(op, FixOp::RebuildSectionIndexes))
        {
            pages::render_index_fabric(wiki, &mut t)?;
        }

        // single-artifact rebuilds (missing_artifact / dep repin for
        // script-owned pages).
        let mut rebuilt: HashSet<String> = HashSet::new();
        for op in &in_txn {
            if let FixOp::RebuildArtifact { rel } = op {
                if rebuilt.insert(rel.clone()) {
                    pages::stage_script_owned_artifact(wiki, &mut t, rel)?;
                }
            }
        }

        // dep-edge edits in ONE stage_state write — §8 pinned order: drop
        // dead deps from survivors first, whole edge removals last.
        let drops: Vec<(String, String)> = in_txn
            .iter()
            .filter_map(|op| match op {
                FixOp::DropDep { artifact, dep } => Some((artifact.clone(), dep.clone())),
                _ => None,
            })
            .collect();
        let repins: Vec<(String, String, String)> = in_txn
            .iter()
            .filter_map(|op| match op {
                FixOp::RepinDep { artifact, old, new } => {
                    Some((artifact.clone(), old.clone(), new.clone()))
                }
                _ => None,
            })
            .collect();
        let edge_removals: Vec<String> = in_txn
            .iter()
            .filter_map(|op| match op {
                FixOp::DropArtifactEdge { artifact } => Some(artifact.clone()),
                _ => None,
            })
            .collect();
        if !drops.is_empty() || !repins.is_empty() || !edge_removals.is_empty() {
            t.stage_state_with(deps::DEPS_FILE, |data| {
                if !data.is_object() {
                    *data = json!({});
                }
                let mut edges = data
                    .get("edges")
                    .and_then(Value::as_object)
                    .cloned()
                    .unwrap_or_default();
                for (art, dead) in &drops {
                    let kept: Vec<Value> = edges
                        .get(art)
                        .and_then(Value::as_array)
                        .map(|arr| {
                            arr.iter()
                                .filter(|x| py_str(x).trim() != dead.as_str())
                                .cloned()
                                .collect()
                        })
                        .unwrap_or_default();
                    edges.insert(art.clone(), Value::Array(kept));
                }
                for (art, old, new) in &repins {
                    if let Some(arr) = edges.get_mut(art).and_then(Value::as_array_mut) {
                        for d in arr.iter_mut() {
                            if py_str(d).trim() == old.as_str() {
                                *d = Value::String(new.clone());
                            }
                        }
                    }
                }
                for art in &edge_removals {
                    edges.remove(art);
                }
                data["edges"] = Value::Object(edges);
            })?;
        }

        // malformed_frontmatter: already repaired pre-txn above; stage the
        // identical bytes so the write is journaled in the one commit.
        for op in &in_txn {
            if let FixOp::RewriteFrontmatter { rel } = op {
                let content = frontmatter_default_rewrite(wiki, rel)?;
                t.stage_write(rel, content.as_bytes())?;
            }
        }

        for op in &in_txn {
            if let FixOp::DeleteFile { rel } = op {
                t.stage_delete(rel)?;
            }
        }

        t.changes
            .insert("fixes_applied".to_string(), json!(plan.records.len() as i64));
        let receipt = t.commit()?;
        committed_rev = receipt["revision"]["after"].as_i64();
    }

    // dead staging dirs — post-commit: dir deletes aren't file ops (§5.5).
    for rel in &staging_dirs {
        let dir = wiki.p(rel);
        if dir.is_dir() {
            let _ = fs::remove_dir_all(&dir);
        }
    }

    // §9.2: index rebuild is post-commit — it must observe the committed
    // state and takes index.lock.
    if needs_index {
        let rev = committed_rev.unwrap_or(wiki.revision()?);
        Index::rebuild_under_index_lock(wiki, rev)?;
    }

    Ok(plan.records.iter().map(FixRecord::to_value).collect())
}

/// `doctor --fix` entry — plan against `wiki`'s findings, apply to
/// `target` (the same wiki for a real run; a scratch copy for --dry-run's
/// assumeFixed simulation). Returns `(fixed[], post_fix_report)`.
pub(crate) fn fix_wiki(
    wiki: &Wiki,
    target: &Wiki,
    findings: &[Finding],
    codes: &[String],
) -> Result<(Vec<Value>, Report), EngineError> {
    let plan = collect_fixes(wiki, findings, codes)?;
    let fixed = apply_fixes(target, plan)?;
    let after = run(target)?;
    Ok((fixed, after))
}


/// The `malformed_frontmatter` repair content: default title/type/tags
/// ONLY — the body is kept verbatim (§9.1). `rel` is always
/// `wiki/<kind>/<slug>.md` (the page_universe shape).
fn frontmatter_default_rewrite(wiki: &Wiki, rel: &str) -> Result<String, EngineError> {
    let text = fs::read_to_string(wiki.p(rel)).unwrap_or_default();
    let body = frontmatter::body_after_fence(&text);
    let slug = rel
        .rsplit('/')
        .next()
        .unwrap_or("page")
        .trim_end_matches(".md");
    let kind = rel.split('/').nth(1).unwrap_or("concepts");
    let mut fr: Map<String, Value> = Map::new();
    fr.insert(
        "title".to_string(),
        json!(synthesize::py_title(&slug.replace('-', " "))),
    );
    fr.insert("type".to_string(), json!(frontmatter::kind_type(kind)));
    fr.insert("tags".to_string(), json!([]));
    frontmatter::page_text(&fr, body)
}
