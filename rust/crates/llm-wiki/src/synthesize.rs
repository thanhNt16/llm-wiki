//! Claims -> prose synthesis: target selection and gated page writes.
//! Port of wikicore/synthesize.py — the agent authors prose; this module
//! selects eligible subjects, validates the proposed page against the
//! dependency graph, and commits atomically. No prose is invented here.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::LazyLock;

use serde_json::{json, Map, Value};

use crate::claims;
use crate::deps;
use crate::err::EngineError;
use crate::frontmatter::{self, PAGE_KINDS, kind_type};
use crate::ids;
use crate::store::Wiki;
use crate::topics;
use crate::txn::{Txn, TxnOpts};
use crate::util;

/// synthesize.py:29 — statuses that can seed a page.
const ELIGIBLE_STATUSES: &[&str] = &["accepted", "provisional"];
/// synthesize.py:30 — authority that bypasses corroboration thresholds.
const AUTHORITATIVE: &str = "explicit_project_decision";
/// synthesize.py:34 — write_page refuses a NEW page at/above this
/// title/subject token-Jaccard (unless --allow-new).
const NEAR_DUP_WRITE_THRESHOLD: f64 = 0.60;

/// synthesize.py:37 `LINK_RE` — `[[claim_...]]` body citations.
static LINK_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\[\[([a-z]+_[0-9A-Za-z]+)\]\]").unwrap());
/// synthesize.py:38 `PAGE_LINK_RE` — `[[wiki:<kind>/<slug>]]` page refs.
static PAGE_LINK_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\[\[wiki:([a-z]+)/([a-z0-9][a-z0-9\-]*)\]\]").unwrap()
});
/// synthesize.py:40 `_MD_LINK_RE` — markdown links into the wiki/ bundle.
static MD_LINK_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"\[[^\]]*\]\(([^)\s]+\.md)\)").unwrap());

const KIND_RULES_FILE: &str = "page-kinds.json";

fn now() -> String {
    util::now_iso()
}

/// `format!("{:?}", s)` renders the Python `%r`-style repr used in the
/// original error strings (quoted with escapes).
fn repr(s: &str) -> String {
    format!("{s:?}")
}

// ---- kind routing (synthesize.py:46-182) -------------------------------

/// A usable file rule: its original index (emitted as `matched_rule`) and
/// the raw rule object.
pub type KindRule = (usize, Value);

/// `_load_kind_rules` — ordered kind-routing rules from
/// `.llm-wiki/page-kinds.json`. First match wins; --kind overrides all.
/// `heuristics_active` is always true in Python — kept as a literal.
pub fn load_kind_rules(wiki: &Wiki, warnings: &mut Vec<String>) -> (Vec<KindRule>, String) {
    let path = wiki.p(KIND_RULES_FILE);
    if !path.is_file() {
        return (Vec::new(), "concepts".to_string());
    }
    let data: Value = match std::fs::read_to_string(&path)
        .map_err(|e| e.to_string())
        .and_then(|t| serde_json::from_str(&t).map_err(|e| e.to_string()))
    {
        Ok(v) => v,
        Err(e) => {
            warnings.push(format!("{KIND_RULES_FILE} ignored: {e}"));
            return (Vec::new(), "concepts".to_string());
        }
    };
    if !data.is_object() {
        warnings.push(format!("{KIND_RULES_FILE} ignored: expected an object"));
        return (Vec::new(), "concepts".to_string());
    }
    let mut default = data
        .get("default")
        .and_then(Value::as_str)
        .unwrap_or("concepts")
        .to_string();
    if !PAGE_KINDS.contains(&default.as_str()) {
        warnings.push(format!(
            "{KIND_RULES_FILE}: default {default:?} not a page kind — using 'concepts'"
        ));
        default = "concepts".to_string();
    }
    let raw_rules = data.get("rules");
    if raw_rules.is_some_and(|v| !v.is_array()) {
        warnings.push(format!("{KIND_RULES_FILE} ignored: 'rules' must be a list"));
    }
    let mut rules = Vec::new();
    for (i, r) in data
        .get("rules")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
        .iter()
        .enumerate()
    {
        let kind_ok = r
            .get("kind")
            .and_then(Value::as_str)
            .map(|k| PAGE_KINDS.contains(&k))
            .unwrap_or(false);
        if !r.is_object() || !kind_ok {
            warnings.push(format!(
                "{KIND_RULES_FILE} rule {i} ignored: missing/invalid 'kind'"
            ));
            continue;
        }
        let has_matcher = ["subject", "prefix", "suffix", "regex", "predicate"]
            .iter()
            .any(|m| r.get(*m).is_some_and(|v| !v.is_null()));
        if !has_matcher {
            warnings.push(format!(
                "{KIND_RULES_FILE} rule {i} ignored: no matcher key"
            ));
            continue;
        }
        rules.push((i, r.clone()));
    }
    (rules, default)
}

/// `_rule_matches` — every supplied matcher key must match (AND).
fn rule_matches(rule: &Value, subject: &str, clist: &[Value]) -> bool {
    for m in ["subject", "prefix", "suffix", "regex", "predicate"] {
        let Some(raw) = rule.get(m) else { continue };
        if raw.is_null() {
            continue;
        }
        let Some(v) = raw.as_str() else {
            return false; // non-string matcher value never matches
        };
        let matched = match m {
            "subject" => subject == v,
            "prefix" => subject.starts_with(v),
            "suffix" => subject.ends_with(v),
            "regex" => regex::Regex::new(v)
                .map(|re| re.is_match(subject))
                .unwrap_or(false),
            "predicate" => regex::Regex::new(v)
                .map(|re| {
                    clist.iter().any(|c| {
                        let p = c
                            .get("predicate")
                            .map(util::py_str)
                            .unwrap_or_default();
                        re.is_match(&p)
                    })
                })
                .unwrap_or(false),
            _ => true,
        };
        if !matched {
            return false;
        }
    }
    if let Some(min) = rule
        .get("when")
        .and_then(|w| w.get("min_claims"))
        .and_then(Value::as_i64)
    {
        if (clist.len() as i64) < min {
            return false;
        }
    }
    true
}

static H_PROCEDURAL: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^(step|procedure|runbook|how[ _-]?to)").unwrap()
});
static H_TICKETISH: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^[a-z]+-\d+$|\.(api|service|ui|db|cli)$").unwrap()
});
static H_CODE_IDENT: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"^[a-z0-9_]+(?:[A-Z][a-z0-9]*)*$|^[a-z0-9][a-z0-9_\-]*$")
        .unwrap()
});
static H_QUESTION: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"open[ _-]?question|unresolved|tbd").unwrap());

fn pred_str(c: &Value) -> String {
    c.get("predicate").map(util::py_str).unwrap_or_default()
}

/// `_heuristic_kind` — built-in rules 1-4, first hit wins; returns
/// (kind, "heuristic:N") or None.
fn heuristic_kind(subject: &str, clist: &[Value]) -> Option<(&'static str, &'static str)> {
    // 1. any claim predicate looks procedural
    if clist.iter().any(|c| H_PROCEDURAL.is_match(&pred_str(c))) {
        return Some(("procedures", "heuristic:1"));
    }
    // 2. ticket id (stc-97) or dotted component (x.api/.service/.ui/.db/.cli)
    if H_TICKETISH.is_match(subject) {
        return Some(("entities", "heuristic:2"));
    }
    // 3. single-token code identifier with >=2 claims (Python fullmatch)
    if clist.len() >= 2 && H_CODE_IDENT.is_match(subject) {
        return Some(("entities", "heuristic:3"));
    }
    // 4. open-question marker on subject or any predicate
    if H_QUESTION.is_match(subject)
        || clist.iter().any(|c| H_QUESTION.is_match(&pred_str(c)))
    {
        return Some(("questions", "heuristic:4"));
    }
    None
}

/// `matched_rule` JSON value: rule index int, "heuristic:N", or null.
#[derive(Debug, Clone, PartialEq)]
pub enum MatchedRule {
    Index(usize),
    Heuristic(&'static str),
    Default,
}

impl MatchedRule {
    fn to_value(&self) -> Value {
        match self {
            MatchedRule::Index(i) => json!(i),
            MatchedRule::Heuristic(h) => json!(h),
            MatchedRule::Default => Value::Null,
        }
    }
}

/// `_route_kind` — file rules first, then built-in heuristics (always
/// active), then the default. Returns (kind, matched_rule).
pub fn route_kind(
    subject: &str,
    clist: &[Value],
    rules: &[KindRule],
    default: &str,
) -> (String, MatchedRule) {
    for (i, rule) in rules {
        if rule_matches(rule, subject, clist) {
            return (
                rule["kind"].as_str().unwrap_or(default).to_string(),
                MatchedRule::Index(*i),
            );
        }
    }
    if let Some((kind, tag)) = heuristic_kind(subject, clist) {
        return (kind.to_string(), MatchedRule::Heuristic(tag));
    }
    (default.to_string(), MatchedRule::Default)
}

// ---- links --------------------------------------------------------------

/// `extract_page_links` — `[[wiki:<kind>/<slug>]]` -> `kind/slug`, deduped
/// in first-seen order.
pub fn extract_page_links(body: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for m in PAGE_LINK_RE.captures_iter(body) {
        let r = format!("{}/{}", &m[1], &m[2]);
        if !out.contains(&r) {
            out.push(r);
        }
    }
    out
}

/// `page_links` — `[[wiki:<kind>/<slug>]]` -> `wiki/<kind>/<slug>.md`
/// dep paths.
pub fn page_links(body: &str) -> BTreeSet<String> {
    extract_page_links(body)
        .into_iter()
        .map(|r| format!("wiki/{r}.md"))
        .collect()
}

// ---- near-duplicate detection (synthesize.py:208-311) -------------------

static TITLE_STOPWORDS: &[&str] = &[
    "the", "a", "an", "of", "and", "or", "to", "for", "in", "on", "is", "are",
];

static NON_ALNUM: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"[^0-9a-z]+").unwrap());
static TOKEN_SUFFIXES: &[&str] = &["tion", "ment", "ing", "ed", "ly", "s"];

/// `_normalize_tokens` — text.py `norm_tokens`: lowercase, split on
/// non-alnum, drop tokens <3 chars, strip one conservative suffix.
pub fn norm_tokens(s: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    for tok in NON_ALNUM.split(&s.to_lowercase()) {
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

/// `_norm_title_tokens` — title/subject tokens: lowercase alnum runs
/// minus stopwords (no length or suffix stripping).
fn norm_title_tokens(text: &str) -> HashSet<String> {
    NON_ALNUM
        .split(&text.to_lowercase())
        .filter(|t| !t.is_empty() && !TITLE_STOPWORDS.contains(t))
        .map(|t| t.to_string())
        .collect()
}

fn jaccard(a: &HashSet<String>, b: &HashSet<String>) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    a.intersection(b).count() as f64 / a.union(b).count() as f64
}

/// `build_token_index` — inverted index token -> sorted list of keys.
pub fn build_token_index(
    entries: &HashMap<String, HashSet<String>>,
) -> BTreeMap<String, Vec<String>> {
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

/// `candidate_pairs` — every unordered key-pair sharing >=1 token, sorted
/// (lossless superset of pairs whose Jaccard could be nonzero).
pub fn candidate_pairs(
    index: &BTreeMap<String, Vec<String>>,
) -> BTreeSet<(String, String)> {
    let mut pairs = BTreeSet::new();
    for keys in index.values() {
        for i in 0..keys.len() {
            for b in keys.iter().skip(i + 1) {
                pairs.insert((keys[i].clone(), b.clone()));
            }
        }
    }
    pairs
}

/// Python `round(x, 3)` — three-decimal ties-to-even.
fn round3(x: f64) -> f64 {
    format!("{x:.3}").parse().unwrap_or(x)
}

/// `similar_pages` — near-duplicate pages for a proposed new page:
/// token-Jaccard >= NEAR_DUP_WRITE_THRESHOLD on title/subject vs every
/// existing page in ALL sections (cross-section twins are the worst kind).
pub fn similar_pages(
    wiki: &Wiki,
    kind: &str,
    subject: &str,
    title: &str,
    exclude_slug: Option<&str>,
) -> Vec<Value> {
    let want: HashSet<String> = norm_title_tokens(title)
        .union(&norm_title_tokens(subject))
        .cloned()
        .collect();
    let norm_title = title.trim().to_lowercase();
    let mut order: Vec<String> = Vec::new();
    let mut fronts: HashMap<String, Map<String, Value>> = HashMap::new();
    let mut tokens: HashMap<String, HashSet<String>> = HashMap::new();
    let mut title_map: HashMap<String, Vec<String>> = HashMap::new();
    for sec in PAGE_KINDS {
        let sec_dir = wiki.p(&format!("wiki/{sec}"));
        if !sec_dir.is_dir() {
            continue;
        }
        let mut names: Vec<String> = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&sec_dir) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name.ends_with(".md") && name != "index.md" {
                    names.push(name);
                }
            }
        }
        names.sort();
        for name in names {
            let slug = &name[..name.len() - 3];
            if exclude_slug == Some(slug) && *sec == kind {
                continue;
            }
            let path = wiki.p(&format!("wiki/{sec}/{name}"));
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let Ok((front, _)) = frontmatter::split_front(&text) else {
                continue; // unparseable frontmatter can't produce a title/subject
            };
            let key = format!("{sec}/{slug}");
            order.push(key.clone());
            let ftitle = front.get("title").map(util::py_str).unwrap_or_default();
            let fsubject = front.get("subject").map(util::py_str).unwrap_or_default();
            tokens.insert(
                key.clone(),
                norm_title_tokens(&ftitle)
                    .union(&norm_title_tokens(&fsubject))
                    .cloned()
                    .collect(),
            );
            title_map
                .entry(ftitle.trim().to_lowercase())
                .or_default()
                .push(key.clone());
            fronts.insert(key, front);
        }
    }
    let index = build_token_index(&tokens);
    let mut cands: HashSet<String> = HashSet::new();
    for tok in &want {
        if let Some(keys) = index.get(tok) {
            cands.extend(keys.iter().cloned());
        }
    }
    if let Some(keys) = title_map.get(&norm_title) {
        cands.extend(keys.iter().cloned());
    }
    let mut out = Vec::new();
    for key in order {
        if !cands.contains(&key) {
            continue;
        }
        let (sec, slug) = key.split_once('/').unwrap();
        let front = &fronts[&key];
        let sim = jaccard(&want, &tokens[&key]);
        let ftitle = front.get("title").map(util::py_str).unwrap_or_default();
        if sim >= NEAR_DUP_WRITE_THRESHOLD || ftitle.trim().to_lowercase() == norm_title {
            out.push(json!({
                "section": sec,
                "slug": slug,
                "title": front.get("title").cloned().unwrap_or(Value::Null),
                "similarity": round3(sim),
            }));
        }
    }
    out
}

fn is_reserved(artifact: &str) -> bool {
    let a = artifact.trim_matches('/');
    a == "wiki/index.md"
        || a == "wiki/overview.md"
        || a == "wiki/review.md"
        || a == "wiki/log.md"
        || a.starts_with("wiki/decisions/")
        || a.ends_with("/index.md")
}

fn detail(c: &Value) -> Value {
    let authority_type = match c.get("authority") {
        Some(Value::Object(o)) => o.get("type").cloned().unwrap_or(Value::Null),
        _ => Value::Null,
    };
    json!({
        "id": c.get("id").cloned().unwrap_or(Value::Null),
        "version": c.get("version").cloned().unwrap_or(Value::Null),
        "predicate": c.get("predicate").cloned().unwrap_or(Value::Null),
        "value": c.get("value").cloned().unwrap_or(Value::Null),
        "scope": match c.get("scope") { Some(Value::Object(_)) => c["scope"].clone(), _ => json!({}) },
        "status": c.get("status").cloned().unwrap_or(Value::Null),
        "authority.type": authority_type,
        "valid_from": c.get("valid_from").cloned().unwrap_or(Value::Null),
        "valid_to": c.get("valid_to").cloned().unwrap_or(Value::Null),
    })
}

/// `_claim_origins` — root origins behind one claim's evidence (the set,
/// so subjects can union across claims).
fn claim_origins(wiki: &Wiki, c: &Value) -> Result<HashSet<String>, EngineError> {
    let mut origins = HashSet::new();
    for ev in c.get("evidence").and_then(Value::as_array).cloned().unwrap_or_default() {
        let mut ro = ev.get("root_origin").cloned().unwrap_or(Value::Null);
        if !frontmatter::truthy(Some(&ro)) {
            let sid = ev.get("source_id").and_then(Value::as_str).ok_or_else(|| {
                EngineError::Txn("claim evidence missing source_id".into())
            })?;
            ro = match wiki.load_json_opt(&format!("sources/{sid}/manifest.json")) {
                Ok(Some(m)) => m
                    .get("root_origin")
                    .cloned()
                    .unwrap_or_else(|| json!(sid)),
                _ => json!(sid),
            };
        }
        origins.insert(serde_json::to_string(&ro)?);
    }
    Ok(origins)
}

fn claim_exists(wiki: &Wiki, cid: &str) -> bool {
    wiki.p(&format!("claims/{cid}.json")).is_file()
}

/// `_load_named` — load claims/<id>.json / decisions/<id>.json without
/// ULID validation (tolerates pre-ULID and fixture ids) while refusing
/// traversal.
fn load_named(wiki: &Wiki, subdir: &str, oid: &str) -> Result<Value, EngineError> {
    if oid.is_empty() || oid.contains('/') || oid.contains('\\') || oid.contains("..") {
        return Err(EngineError::Txn(format!("dep id invalid: {}", repr(oid))));
    }
    let path = wiki.p(&format!("{subdir}/{oid}.json"));
    if !path.is_file() {
        return Err(EngineError::Txn(format!("dep {} does not exist", repr(oid))));
    }
    let text = std::fs::read_to_string(&path).map_err(|e| {
        EngineError::Txn(format!("dep {} unreadable: {e}", repr(oid)))
    })?;
    serde_json::from_str(&text).map_err(|e| {
        EngineError::Txn(format!("dep {} unreadable: {e}", repr(oid)))
    })
}

/// POSIX `os.path.normpath` for the forward-slash bundle paths used here.
fn normpath(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    for comp in path.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                if !out.is_empty() {
                    out.pop();
                }
            }
            c => out.push(c),
        }
    }
    if out.is_empty() {
        ".".to_string()
    } else {
        out.join("/")
    }
}

/// `_page_links` — resolve OKF-style markdown links to wiki/ rel paths.
/// External URLs and non-.md links are ignored; bundle-internal .md links
/// that don't resolve are rejected.
fn page_md_links(wiki: &Wiki, artifact: &str, body: &str) -> Result<Vec<String>, EngineError> {
    let mut out: Vec<String> = Vec::new();
    for m in MD_LINK_RE.captures_iter(body) {
        let target = &m[1];
        if target.contains("://") {
            continue;
        }
        let rel = if target.starts_with("wiki/") {
            target.to_string()
        } else {
            let dir = match artifact.rfind('/') {
                Some(i) => &artifact[..i],
                None => "",
            };
            if dir.is_empty() {
                normpath(target)
            } else {
                normpath(&format!("{dir}/{target}"))
            }
        };
        if !rel.starts_with("wiki/") {
            continue; // links outside the bundle are the author's business
        }
        if rel == artifact {
            continue;
        }
        if !wiki.p(&rel).is_file() {
            return Err(EngineError::Txn(format!(
                "unresolved page link {} -> {rel} in {artifact}",
                repr(target)
            )));
        }
        if !out.contains(&rel) {
            out.push(rel);
        }
    }
    Ok(out)
}

/// `_normalize_deps` — typed deps to canonical `claim_X@N` /
/// `decision_Y@N` strings (version must match current; rejected/superseded
/// claims refused) and `wiki/<dir>/<slug>.md` artifact paths (must exist).
fn normalize_deps(wiki: &Wiki, dep_list: &[Value]) -> Result<Vec<String>, EngineError> {
    let mut out: Vec<String> = Vec::new();
    for depv in dep_list {
        let dep = match depv {
            Value::String(s) => s.trim().to_string(),
            other => other.to_string(),
        };
        let d = if dep.starts_with("wiki/") {
            if !dep.ends_with(".md") || dep.split('/').any(|p| p == "..") {
                return Err(EngineError::Txn(format!(
                    "dep {} is not a wiki/<kind>/<slug>.md path",
                    repr(&dep)
                )));
            }
            if !wiki.p(&dep).is_file() {
                return Err(EngineError::Txn(format!(
                    "dep {} does not exist on disk",
                    repr(&dep)
                )));
            }
            dep
        } else if dep.starts_with("decision_") {
            let (did, ver) = match dep.split_once('@') {
                Some((d, v)) => (d, v),
                None => (dep.as_str(), ""),
            };
            let dec = load_named(wiki, "decisions", did)?;
            let cur = dec["version"].as_i64().unwrap_or(0);
            if !ver.is_empty() && ver.parse::<i64>().ok() != Some(cur) {
                return Err(EngineError::Txn(format!(
                    "dep {did}@{ver} does not match current version {cur}"
                )));
            }
            format!("{did}@{cur}")
        } else {
            let (cid, ver) = match dep.split_once('@') {
                Some((c, v)) => (c, v),
                None => (dep.as_str(), ""),
            };
            let c = load_named(wiki, "claims", cid)?;
            let cur = c["version"].as_i64().unwrap_or(0);
            if !ver.is_empty() && ver.parse::<i64>().ok() != Some(cur) {
                return Err(EngineError::Txn(format!(
                    "dep {cid}@{ver} does not match current version {cur}"
                )));
            }
            let status = c["status"].as_str().unwrap_or("");
            if claims::TERMINAL.contains(&status) {
                return Err(EngineError::Txn(format!(
                    "dep {cid} is {status}; cite a current claim"
                )));
            }
            format!("{cid}@{cur}")
        };
        if !out.contains(&d) {
            out.push(d);
        }
    }
    Ok(out)
}

/// `page_targets` — group eligible claims by subject; report synthesis
/// targets, subjects still waiting, and stale derived artifacts under wiki/.
pub fn page_targets(
    wiki: &Wiki,
    kind: Option<&str>,
    stale_only: bool,
    min_claims: i64,
    min_sources: i64,
) -> Result<Value, EngineError> {
    if let Some(k) = kind {
        if !PAGE_KINDS.contains(&k) {
            return Err(EngineError::Txn(format!(
                "unknown page kind: {k} (expected one of {})",
                PAGE_KINDS.join(", ")
            )));
        }
    }
    let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for c in claims::list(wiki)? {
        let status = c.get("status").and_then(Value::as_str).unwrap_or("");
        if ELIGIBLE_STATUSES.contains(&status) {
            groups
                .entry(c.get("subject").map(util::py_str).unwrap_or_default())
                .or_default()
                .push(c);
        }
    }
    let mut warnings: Vec<String> = Vec::new();
    let (rules, default_kind) = load_kind_rules(wiki, &mut warnings);
    let mut targets: Vec<Value> = Vec::new();
    let mut waiting: Vec<Value> = Vec::new();
    let topic_map = topics::load(wiki)?.topics;
    for (subject, cs) in &groups {
        let n = cs.len() as i64;
        // distinct origins across the WHOLE claim set (set-union): two
        // claims sharing one origin corroborate once, not twice.
        let mut origins: HashSet<String> = HashSet::new();
        for c in cs {
            origins.extend(claim_origins(wiki, c)?);
        }
        let origins = origins.len() as i64;
        let tentry = topic_map.get(subject.as_str());
        let resolved = tentry
            .and_then(|e| e.get("status"))
            .and_then(Value::as_str)
            == Some("resolved");
        let declared_kind = if resolved {
            tentry
                .and_then(|e| e.get("kind"))
                .and_then(Value::as_str)
                .and_then(|k| topics::PAGE_DIR.iter().find(|(dk, _)| *dk == k))
                .map(|(_, dir)| *dir)
        } else {
            None
        };
        if resolved
            && tentry.and_then(|e| e.get("kind")).and_then(Value::as_str) == Some("decision")
        {
            continue; // decision topics never emit pages
        }
        let reason = if declared_kind.is_some() {
            "declared"
        } else if n >= min_claims && origins >= min_sources {
            "corroborated"
        } else if cs.iter().any(|c| {
            c.get("authority")
                .and_then(|a| a.get("type"))
                .and_then(Value::as_str)
                == Some(AUTHORITATIVE)
        }) {
            "authoritative"
        } else if cs.iter().any(|c| {
            c.get("predicate").and_then(Value::as_str) == Some("open_question")
        }) {
            // open questions are definitionally awaiting humans — exempt
            // from BOTH corroboration thresholds or they starve in waiting[]
            "open_question"
        } else {
            waiting.push(json!({
                "subject": subject,
                "claim_count": n,
                "distinct_origins": origins,
                "reason": "below_threshold",
            }));
            continue;
        };
        let (k, matched) = if let Some(k) = kind {
            (k.to_string(), MatchedRule::Default)
        } else if let Some(dk) = declared_kind {
            (dk.to_string(), MatchedRule::Default)
        } else {
            route_kind(subject, cs, &rules, &default_kind)
        };
        let slug = topics::slugify(subject);
        let rel = format!("wiki/{k}/{slug}.md");
        let prefix = match subject.rsplit_once('.') {
            Some((p, _)) => p,
            None => subject.as_str(),
        };
        let mut related: Vec<String> = Vec::new();
        for (sib, e) in &topic_map {
            if sib == subject
                || e.get("status").and_then(Value::as_str) != Some("resolved")
            {
                continue;
            }
            let sib_prefix = match sib.rsplit_once('.') {
                Some((p, _)) => p,
                None => sib.as_str(),
            };
            if sib_prefix == prefix {
                related.push(sib.clone());
            }
        }
        related.sort();
        related.truncate(8);
        let mut claim_ids: Vec<String> = cs
            .iter()
            .map(|c| {
                format!(
                    "{}@{}",
                    c.get("id").and_then(Value::as_str).unwrap_or(""),
                    c.get("version").and_then(Value::as_i64).unwrap_or(0)
                )
            })
            .collect();
        claim_ids.sort();
        targets.push(json!({
            "subject": subject,
            "matched_rule": matched.to_value(),
            "kind": k,
            "slug": slug,
            "artifact": rel.clone(),
            "claim_ids": claim_ids,
            "claim_count": n,
            "distinct_origins": origins,
            "reason": reason,
            "related": related,
            "exists": wiki.p(&rel).is_file(),
            "stale": deps::is_stale(wiki, &rel).unwrap_or(false),
            "claim_details": cs.iter().map(detail).collect::<Vec<_>>(),
        }));
    }

    // resolved declared topics with no eligible claims still surface as
    // waiting rows so the agent can see them pending
    let topic_keys: Vec<String> = topic_map.keys().cloned().collect();
    for subject in topic_keys {
        let e = &topic_map[&subject];
        let ek = e.get("kind").and_then(Value::as_str);
        if !groups.contains_key(&subject)
            && e.get("status").and_then(Value::as_str) == Some("resolved")
            && ek != Some("source")
            && ek
                .map(|k| topics::PAGE_DIR.iter().any(|(dk, _)| *dk == k))
                .unwrap_or(false)
        {
            waiting.push(json!({
                "subject": subject,
                "claim_count": 0,
                "distinct_origins": 0,
                "reason": "declared_no_claims",
            }));
        }
    }

    // slug collisions: same (kind, slug) from distinct subjects — the first
    // subject (sorted order) keeps the base slug; the rest get a
    // deterministic "-<md5(subject)[:6]>" suffix (mirrored by write_page's
    // slug check)
    let mut seen: HashMap<(String, String), ()> = HashMap::new();
    for t in targets.iter_mut() {
        let tobj = t.as_object_mut().unwrap();
        let key = (
            tobj["kind"].as_str().unwrap_or("").to_string(),
            tobj["slug"].as_str().unwrap_or("").to_string(),
        );
        if seen.insert(key.clone(), ()).is_some() {
            let suffix = &util::md5_hex(
                tobj["subject"].as_str().unwrap_or("").as_bytes(),
            )[..6];
            let new_slug = format!("{}-{}", key.1, suffix);
            warnings.push(format!(
                "slug collision wiki/{}/{}.md: subject {} -> {}",
                key.0,
                key.1,
                repr(tobj["subject"].as_str().unwrap_or("")),
                new_slug
            ));
            tobj.insert("slug".to_string(), json!(new_slug));
            let artifact = format!("wiki/{}/{}.md", key.0, tobj["slug"].as_str().unwrap());
            tobj.insert("artifact".to_string(), json!(artifact.clone()));
            tobj.insert(
                "stale".to_string(),
                json!(deps::is_stale(wiki, &artifact).unwrap_or(false)),
            );
        }
    }

    // extend-don't-create: NEW targets that near-duplicate an existing page
    // are routed extend-first
    for t in targets.iter_mut() {
        let tobj = t.as_object_mut().unwrap();
        if tobj["exists"].as_bool().unwrap_or(false) {
            continue;
        }
        let subject = tobj["subject"].as_str().unwrap_or("").to_string();
        let sim = similar_pages(
            wiki,
            tobj["kind"].as_str().unwrap_or(""),
            &subject,
            &py_title(&subject),
            None,
        );
        if !sim.is_empty() {
            tobj.insert("similar_existing".to_string(), json!(sim));
            tobj.insert("routing".to_string(), json!("extend-first"));
        }
    }

    if stale_only {
        targets.retain(|t| t["stale"].as_bool().unwrap_or(false));
    }
    targets.sort_by(|a, b| {
        let ka = (
            !a["stale"].as_bool().unwrap_or(false),
            -a["claim_count"].as_i64().unwrap_or(0),
            a["subject"].as_str().unwrap_or("").to_string(),
        );
        let kb = (
            !b["stale"].as_bool().unwrap_or(false),
            -b["claim_count"].as_i64().unwrap_or(0),
            b["subject"].as_str().unwrap_or("").to_string(),
        );
        ka.cmp(&kb)
    });
    let stale_artifacts: Vec<String> = deps::stale_artifacts(wiki)?
        .into_iter()
        .filter(|a| a.starts_with("wiki/"))
        .collect();
    Ok(json!({
        "targets": targets,
        "waiting": waiting,
        "stale_artifacts": stale_artifacts,
        "warnings": warnings,
    }))
}

/// Python `str.title()` — uppercase the first letter after each non-letter.
/// `pub(crate)`: doctor --fix reuses it for the malformed_frontmatter title
/// default (§9.1).
pub(crate) fn py_title(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_alpha = false;
    for ch in s.chars() {
        let lc = ch.to_lowercase().next().unwrap_or(ch);
        let uc = ch.to_uppercase().next().unwrap_or(ch);
        if prev_alpha {
            out.push(lc);
        } else {
            out.push(uc);
        }
        prev_alpha = uc != lc || ch.is_alphabetic();
    }
    out
}

/// `write_page` — validate an agent-authored page and commit it atomically
/// with its dependency registration and stale-flag clear.
///
/// `dep_list=None` auto-derives deps from the body's `[[claim_*]]` links at
/// their current versions.
pub fn write_page(
    wiki: &Wiki,
    artifact: &str,
    markdown_text: &str,
    dep_list: Option<Vec<Value>>,
    base_revision: i64,
    allow_new: bool,
) -> Result<Value, EngineError> {
    let rel = artifact.trim_matches('/').to_string();
    if is_reserved(&rel) {
        return Err(EngineError::Txn(format!(
            "reserved artifact cannot be written by write_page: {rel}"
        )));
    }
    let parts: Vec<&str> = rel.split('/').collect();
    if parts.len() != 3
        || parts[0] != "wiki"
        || !PAGE_KINDS.contains(&parts[1])
        || !parts[2].ends_with(".md")
    {
        return Err(EngineError::Txn(format!(
            "artifact must be wiki/<kind>/<slug>.md: {artifact}"
        )));
    }

    let (mut front, body) = frontmatter::split_front(markdown_text)?;
    let ftype = match front.get("type") {
        Some(Value::String(s)) => s.clone(),
        _ => {
            return Err(EngineError::Txn("frontmatter type missing/empty".into()));
        }
    };
    if ftype != kind_type(parts[1]) {
        return Err(EngineError::Txn(format!(
            "frontmatter type {} does not match artifact kind {}",
            repr(&ftype),
            repr(kind_type(parts[1]))
        )));
    }
    let subject = front.get("subject").map(util::py_str).unwrap_or_default();
    if subject.trim().is_empty() {
        return Err(EngineError::Txn(
            "frontmatter subject missing/empty".into(),
        ));
    }
    if !frontmatter::truthy(front.get("title")) {
        return Err(EngineError::Txn("frontmatter title missing/empty".into()));
    }
    let base_slug = topics::slugify(&subject);
    let want = &parts[2][..parts[2].len() - 3];
    let md5_suffix = format!("{}-{}", base_slug, &util::md5_hex(subject.as_bytes())[..6]);
    if want != base_slug && want != md5_suffix {
        return Err(EngineError::Txn(format!(
            "frontmatter subject {} does not match artifact slug {}",
            repr(&subject),
            repr(want)
        )));
    }

    let linked: BTreeSet<String> = LINK_RE
        .captures_iter(body)
        .map(|m| m[1].to_string())
        .collect();
    let unresolved: Vec<String> = linked
        .iter()
        .filter(|c| !claim_exists(wiki, c))
        .cloned()
        .collect();
    if !unresolved.is_empty() {
        return Err(EngineError::Txn(format!(
            "unresolved [[claim]] links: {}",
            unresolved.join(", ")
        )));
    }
    // [[wiki:]] page links: self-links never allowed (the target of a
    // self-link cannot exist yet, so check before dep normalization), and
    // every link target must exist as a page on disk.
    let pl_paths: Vec<String> = page_links(body).into_iter().collect();
    for pl_path in &pl_paths {
        if *pl_path == rel {
            return Err(EngineError::Txn(format!(
                "self-link not allowed: page cannot link to itself: {rel}"
            )));
        }
        if !wiki.p(pl_path).is_file() {
            return Err(EngineError::Txn(format!(
                "unresolved [[wiki:{}]] link: {pl_path}",
                &pl_path[5..pl_path.len() - 3]
            )));
        }
    }
    let page_deps = page_md_links(wiki, &rel, body)?; // OKF markdown links, validated
    let mut dep_list = match dep_list {
        Some(dl) => dl,
        None => {
            // auto-derive: claim links pin to current versions; [[wiki:]]
            // page links join as wiki/<kind>/<slug>.md artifact deps
            linked
                .iter()
                .cloned()
                .chain(pl_paths.iter().cloned())
                .collect::<BTreeSet<_>>()
                .into_iter()
                .map(Value::String)
                .collect::<Vec<Value>>()
        }
    };
    for p in &page_deps {
        let pv = json!(p);
        if !dep_list.contains(&pv) {
            dep_list.push(pv);
        }
    }
    let dep_set = normalize_deps(wiki, &dep_list)?;
    if dep_set.is_empty() && ftype != "question" {
        return Err(EngineError::Txn(
            "deps required: cite at least one dep (only type: question pages may have empty deps)"
                .into(),
        ));
    }
    let claim_dep_ids: HashSet<String> = dep_set
        .iter()
        .filter(|d| d.starts_with("claim_"))
        .map(|d| d.split('@').next().unwrap_or("").to_string())
        .collect();
    let missing: Vec<&String> = linked
        .iter()
        .filter(|c| !claim_dep_ids.contains(*c))
        .collect();
    if !missing.is_empty() {
        return Err(EngineError::Txn(format!(
            "linked claims missing from deps: {}",
            missing
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )));
    }
    for pl_path in &pl_paths {
        if !dep_set.contains(pl_path) {
            return Err(EngineError::Txn(format!(
                "linked page missing from deps: {pl_path}"
            )));
        }
    }
    for d in &dep_set {
        let did = d.split('@').next().unwrap_or("");
        if did.starts_with("claim_")
            && load_named(wiki, "claims", did)?
                .get("status")
                .and_then(Value::as_str)
                == Some("disputed")
            && front.get("disputed") != Some(&Value::Bool(true))
        {
            return Err(EngineError::Txn(format!(
                "dep {did} is disputed; set frontmatter disputed: true to cite it"
            )));
        }
    }

    let rel_path = wiki.p(&rel);
    let is_new = !rel_path.is_file();
    let mut twins: Vec<Value> = Vec::new();
    let mut old_body: Option<String> = None;
    if !is_new {
        let text = std::fs::read_to_string(&rel_path)?;
        let (old_front, ob) = frontmatter::split_front(&text)?;
        let old_subject = match old_front.get("subject") {
            Some(v) if frontmatter::truthy(Some(v)) => util::py_str(v),
            _ => String::new(),
        };
        if old_subject != subject {
            return Err(EngineError::Txn(format!(
                "subject is immutable on refresh: existing {} != {}",
                repr(&old_subject),
                repr(&subject)
            )));
        }
        if !frontmatter::truthy(front.get("created")) {
            let created = match old_front.get("created") {
                Some(v) if frontmatter::truthy(Some(v)) => v.clone(),
                _ => json!(now()),
            };
            front.insert("created".to_string(), created);
        }
        old_body = Some(ob.to_string());
    } else {
        let created = match front.get("created") {
            Some(v) if frontmatter::truthy(Some(v)) => util::py_str(v),
            _ => String::new(),
        };
        if !regex::Regex::new(r"^\d{4}-\d{2}-\d{2}T")
            .unwrap()
            .is_match(&created)
        {
            return Err(EngineError::Txn(
                "frontmatter created must be an ISO datetime (YYYY-MM-DDT...) on new pages"
                    .into(),
            ));
        }
        let title = match front.get("title") {
            Some(v) => util::py_str(v),
            None => String::new(),
        };
        twins = similar_pages(wiki, parts[1], &subject, &title, None);
        if !twins.is_empty() && !allow_new {
            let tw = &twins[0];
            let names: Vec<String> = twins
                .iter()
                .take(3)
                .map(|t| {
                    format!(
                        "wiki/{}/{}.md",
                        t["section"].as_str().unwrap_or(""),
                        t["slug"].as_str().unwrap_or("")
                    )
                })
                .collect();
            return Err(EngineError::Txn(format!(
                "near-duplicate page: wiki/{}/{}.md (sim={}, title='{}') \
                 already covers this — extend it, or pass --allow-new; twins: {}",
                tw["section"].as_str().unwrap_or(""),
                tw["slug"].as_str().unwrap_or(""),
                util::py_str(&tw["similarity"]),
                tw["title"].as_str().unwrap_or(""),
                names.join(", ")
            )));
        }
    }

    if is_new && allow_new && !twins.is_empty() {
        // forced past the near-duplicate guard
        front.insert("forced_new".to_string(), json!(true));
    }

    // OKF niceties: `owner`/`tags` pass through untouched when authored;
    // `owner` is backfilled from the first dep claim's independent origin.
    if !frontmatter::truthy(front.get("owner")) {
        for d in &dep_set {
            let did = d.split('@').next().unwrap_or("");
            if did.starts_with("claim_") {
                // _load_named (not load_claim): dep_set was already
                // validated, and test fixtures may use non-ULID ids
                let c = load_named(wiki, "claims", did)?;
                let mut origins: Vec<Value> =
                    vec![c.get("root_origin").cloned().unwrap_or(Value::Null)];
                for ev in c.get("evidence").and_then(Value::as_array).cloned().unwrap_or_default()
                {
                    origins.push(ev.get("root_origin").cloned().unwrap_or(Value::Null));
                }
                if let Some(origin) = origins.iter().find(|o| frontmatter::truthy(Some(o))) {
                    front.insert("owner".to_string(), origin.clone());
                    break;
                }
            }
        }
    }
    front.insert("deps".to_string(), json!(dep_set));
    front.insert("updated".to_string(), json!(now()));
    front.insert("stale".to_string(), json!(false));
    let text = frontmatter::page_text(&front, body)
        .map_err(|e| EngineError::Txn(format!("frontmatter invalid: {e}")))?;

    let run_id = ids::new_run();
    let mut txn = Txn::begin_with(
        wiki,
        &run_id,
        TxnOpts {
            command: Some("wiki-write-page".to_string()),
            ..Default::default()
        },
    )?;
    txn.stage_write(&rel, text.as_bytes())?;
    deps::replace(
        &mut txn,
        &rel,
        &dep_set.iter().map(|s| s.as_str()).collect::<Vec<_>>(),
    )?;
    deps::clear_stale(&mut txn, &[rel.as_str()])?;
    // A new page or a changed body invalidates every artifact that deps on
    // this page (transitively, via deps.invalidate's artifact-name cascade).
    let body_changed = match &old_body {
        None => true,
        Some(ob) => ob.trim() != body.trim(),
    };
    if body_changed {
        deps::invalidate(wiki, &mut txn, &[rel.as_str()])?;
    }
    txn.changes.insert("page".to_string(), json!(rel));
    txn.base_revision = base_revision;
    txn.commit()?;
    Ok(json!({
        "written": rel,
        "deps": dep_set,
        "claims_cited": linked.len(),
    }))
}

/// `page_show` — frontmatter, body, fresh claim details, staleness and
/// registered deps for one derived page.
pub fn page_show(wiki: &Wiki, artifact: &str) -> Result<Value, EngineError> {
    let rel = artifact.trim_matches('/');
    let parts: Vec<&str> = rel.split('/').collect();
    if !rel.starts_with("wiki/")
        || !rel.ends_with(".md")
        || parts.iter().any(|p| p.is_empty() || *p == "." || *p == "..")
    {
        return Err(EngineError::Txn(format!(
            "artifact must be a path under wiki/: {artifact}"
        )));
    }
    let edges: Map<String, Value> = match deps::graph(wiki) {
        Ok(g) => g
            .get("edges")
            .and_then(|e| e.as_object())
            .cloned()
            .unwrap_or_default(),
        Err(_) => Map::new(),
    };
    if !edges.contains_key(rel) {
        return Err(EngineError::Txn(format!(
            "artifact is not a registered wiki page: {rel}"
        )));
    }
    let path = wiki.p(rel);
    if !path.is_file() {
        return Err(EngineError::Txn(format!("no such artifact: {rel}")));
    }
    let text = std::fs::read_to_string(&path)?;
    let (front, body) = frontmatter::split_front(&text)?;
    let mut claim_details: Vec<Value> = Vec::new();
    for dep in front.get("deps").and_then(Value::as_array).cloned().unwrap_or_default() {
        let dep = dep.as_str().unwrap_or("");
        let did = dep.split('@').next().unwrap_or("");
        if did.starts_with("claim_") {
            // claims.load_claim semantics: claim_ prefix + ULID validation
            if did.starts_with("claim_") && ids::validate(did) {
                if let Ok(Some(c)) = wiki.load_json_opt(&claims::claim_rel(did)) {
                    claim_details.push(detail(&c));
                }
            }
        }
    }
    Ok(json!({
        "artifact": rel,
        "front": front,
        "body": body,
        "claim_details": claim_details,
        "stale": deps::is_stale(wiki, rel).unwrap_or(false),
        "deps_registered": deps::graph(wiki)?
            .get("edges")
            .and_then(|e| e.get(rel))
            .cloned()
            .unwrap_or_else(|| json!([])),
    }))
}
