//! Canonical topic registry (`topics.json`, topics.py TOPICS_FILE).
//!
//! Declared by the agent during compile extraction (topics.jsonl rows),
//! merged deterministically at apply time inside the claim transaction.
//! Port of wikicore/topics.py: merge_rows / auto_register / backfill /
//! ensure_source_topic plus the synthesize kind-routing helpers backfill
//! needs (synthesize.py:_load_kind_rules/_rule_matches/_heuristic_kind).

use std::collections::BTreeMap;

use serde_json::{json, Map, Value};

use crate::claims;
use crate::err::EngineError;
use crate::ids;
use crate::store::Wiki;
use crate::txn::{Txn, TxnOpts};

pub const TOPICS_FILE: &str = "topics.json";
const KIND_RULES_FILE: &str = "page-kinds.json";

/// Declarable kinds (topics.py KINDS); `source`/`decision` are engine-owned.
pub const KINDS: &[&str] = &[
    "concept",
    "entity",
    "procedure",
    "question",
    "change",
    "decision",
    "source",
];
const VALID_DECLARED: &[&str] = &[
    "concept",
    "entity",
    "procedure",
    "question",
    "change",
    "decision",
];
/// topics.py _PLURAL — plural page dir → singular kind.
fn plural_to_kind(plural: &str) -> Option<&'static str> {
    Some(match plural {
        "concepts" => "concept",
        "entities" => "entity",
        "procedures" => "procedure",
        "questions" => "question",
        "changes" => "change",
        "decisions" => "decision",
        "sources" => "source",
        _ => return None,
    })
}
/// topics.py PAGE_DIR — kinds with a wiki page section (decision excluded).
pub const PAGE_DIR: &[(&str, &str)] = &[
    ("concept", "concepts"),
    ("entity", "entities"),
    ("procedure", "procedures"),
    ("question", "questions"),
    ("change", "changes"),
    ("source", "sources"),
];
/// synthesize.py PAGE_KINDS — valid `kind` values for page-kinds.json rules.
const PAGE_KINDS: &[&str] = &[
    "concepts",
    "entities",
    "procedures",
    "questions",
    "sources",
    "changes",
];

/// Canonical registry shape `{version:int, topics:{subject:entry}}`.
/// Top-level extras are not part of the contract; entry-level extras are
/// kept verbatim inside each entry object (spec §4.1 — `links` tolerated).
#[derive(Debug, Clone, Default)]
pub struct Topics {
    pub version: i64,
    pub topics: Map<String, Value>,
}

impl Topics {
    pub fn to_value(&self) -> Value {
        json!({"version": self.version, "topics": Value::Object(self.topics.clone())})
    }

    fn from_value(v: &Value) -> Result<Topics, EngineError> {
        if !v.is_object() {
            return Err(EngineError::Txn(format!(
                "corrupt {TOPICS_FILE}: expected an object"
            )));
        }
        let version = v.get("version").and_then(Value::as_i64).unwrap_or(0);
        let topics = match v.get("topics") {
            Some(Value::Object(m)) => m.clone(),
            Some(_) => {
                return Err(EngineError::Txn(format!(
                    "corrupt {TOPICS_FILE}: 'topics' is not an object"
                )))
            }
            None => Map::new(),
        };
        Ok(Topics { version, topics })
    }
}

/// `topics.load` (topics.py:39-46) — `{version:0, topics:{}}` when absent;
/// corrupt JSON → TxnError naming the file.
pub fn load(wiki: &Wiki) -> Result<Topics, EngineError> {
    let path = wiki.p(TOPICS_FILE);
    if !path.is_file() {
        return Ok(Topics::default());
    }
    let text = std::fs::read_to_string(&path)?;
    let v: Value = serde_json::from_str(&text).map_err(|e| {
        EngineError::Txn(format!(
            "corrupt {} ({TOPICS_FILE}): {e}",
            path.display()
        ))
    })?;
    Topics::from_value(&v).map_err(|_| {
        EngineError::Txn(format!("corrupt {} ({TOPICS_FILE})", path.display()))
    })
}

/// Registry lookup — `topics.get` (topics.py:48-49).
pub fn get(wiki: &Wiki, subject: &str) -> Result<Option<Value>, EngineError> {
    Ok(load(wiki)?.topics.get(subject).cloned())
}

/// Distinct claim subjects, sorted — rollup input for `subjects` (P2) and
/// status rollups (`query.subjects` groups by claims.scan).
pub fn subjects(wiki: &Wiki) -> Result<Vec<String>, EngineError> {
    let mut out: Vec<String> = claims::list(wiki)?
        .iter()
        .map(|c| c["subject"].as_str().unwrap_or("").to_string())
        .collect();
    out.sort();
    out.dedup();
    Ok(out)
}

/// topics.py `_norm_kind` — lowercase + plural→singular.
pub fn norm_kind(k: &str) -> String {
    let k = k.trim().to_lowercase();
    match plural_to_kind(&k) {
        Some(s) => s.to_string(),
        None => k,
    }
}

/// topics.py `_title_for` — last dotted segment, `-`→` `, title-cased.
fn title_for(subject: &str) -> String {
    let last = subject.rsplit('.').next().unwrap_or(subject);
    let spaced: String = last.replace('-', " ");
    // Python str.title() uppercases each word's first char.
    spaced
        .split(' ')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + &c.as_str().to_lowercase(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// text.py:slugify — non-alnum runs → `-`, ≤60 chars cut at `-` boundary.
pub fn slugify(subject: &str) -> String {
    let mut slug = String::with_capacity(subject.len());
    let mut last_dash = false;
    for c in subject.chars().flat_map(char::to_lowercase) {
        if c.is_ascii_alphanumeric() {
            slug.push(c);
            last_dash = false;
        } else if !last_dash && !slug.is_empty() {
            slug.push('-');
            last_dash = true;
        }
    }
    let slug = slug.trim_matches('-').to_string();
    let mut slug = if slug.len() > 60 {
        let cut = &slug[..60];
        let cut = match cut.rfind('-') {
            Some(i) => &cut[..i],
            None => cut,
        };
        cut.trim_matches('-').to_string()
    } else {
        slug
    };
    if slug.is_empty() {
        slug = "untitled".to_string();
    }
    slug
}

/// topics.py `_entry`.
fn entry(kind: &str, title: &str, declared_by: &Value) -> Value {
    json!({
        "kind": kind,
        "title": title,
        "status": "resolved",
        "aliases": [],
        "alternate_kinds": [],
        "declared_by": [declared_by.clone()],
    })
}

/// Row keys consumed by the merge — everything else is agent-authored
/// extras kept verbatim on the entry (spec §4.1; e.g. `links`).
const CONSUMED: &[&str] = &["subject", "kind", "title", "aliases"];

/// Validate one topics.jsonl row (parse_topics_file field checks,
/// topics.py:190-201): subject/kind/title required non-blank; kind must
/// normalize to a declared kind (never `source`).
fn validate_row(row: &Value, i: usize) -> Result<String, EngineError> {
    if !row.is_object() {
        return Err(EngineError::Txn(format!(
            "topics row {i}: row must be an object"
        )));
    }
    for field in ["subject", "kind", "title"] {
        let ok = row
            .get(field)
            .map(|v| match v {
                Value::String(s) => !s.trim().is_empty(),
                Value::Null => false,
                other => !other.to_string().trim().is_empty(),
            })
            .unwrap_or(false);
        if !ok {
            return Err(EngineError::Txn(format!(
                "topics row {i}: missing {field:?}"
            )));
        }
    }
    let kind = norm_kind(row["kind"].as_str().unwrap_or(""));
    if !VALID_DECLARED.contains(&kind.as_str()) {
        return Err(EngineError::Txn(format!(
            "topics row {i}: bad kind {:?} (valid: {})",
            row["kind"],
            VALID_DECLARED.join(", ")
        )));
    }
    Ok(kind)
}

/// Python `str(v).strip()` for row fields — strings verbatim, scalars
/// stringified, anything else empty.
fn field_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.trim().to_string(),
        Value::Number(_) | Value::Bool(_) => v.to_string(),
        _ => String::new(),
    }
}

/// topics.py `_merge_one` — returns `true` when a kind conflict was added.
fn merge_one(reg: &mut Topics, row: &Value, kind: &str, declared_by: &Value, out: &mut Map<String, Value>) -> bool {
    let subj = field_str(&row["subject"]);
    let entry_map = reg.topics.entry(subj.clone());
    match entry_map {
        serde_json::map::Entry::Vacant(v) => {
            let mut ent = entry(kind, &field_str(&row["title"]), declared_by);
            // Extras kept verbatim on the entry.
            if let (Some(src), Some(dst)) = (row.as_object(), ent.as_object_mut()) {
                for (k, val) in src {
                    if !CONSUMED.contains(&k.as_str()) {
                        dst.insert(k.clone(), val.clone());
                    }
                }
            }
            v.insert(ent);
            let registered = out
                .entry("registered".to_string())
                .or_insert_with(|| json!(0));
            *registered = json!(registered.as_i64().unwrap_or(0) + 1);
        }
        serde_json::map::Entry::Occupied(mut o) => {
            let ent = o.get_mut();
            let ent_obj = ent.as_object_mut().unwrap();
            let db = ent_obj
                .entry("declared_by".to_string())
                .or_insert_with(|| json!([]));
            if let Some(list) = db.as_array_mut() {
                if !list.iter().any(|e| e == declared_by) {
                    list.push(declared_by.clone());
                }
            }
            if ent_obj.get("kind").and_then(Value::as_str) != Some(kind) {
                let alt = ent_obj
                    .entry("alternate_kinds".to_string())
                    .or_insert_with(|| json!([]));
                if let Some(list) = alt.as_array_mut() {
                    if !list.iter().any(|e| e.as_str() == Some(kind)) {
                        list.push(json!(kind));
                    }
                }
                ent_obj.insert("status".to_string(), json!("conflict"));
                let conflicts = out
                    .entry("kind_conflicts".to_string())
                    .or_insert_with(|| json!([]));
                if let Some(list) = conflicts.as_array_mut() {
                    if !list.iter().any(|e| e.as_str() == Some(subj.as_str())) {
                        list.push(json!(subj));
                    }
                }
                return true;
            }
            // Merge missing extras only — a later declaration never
            // overwrites an extra already recorded on the entry.
            if let Some(src) = row.as_object() {
                for (k, val) in src {
                    if !CONSUMED.contains(&k.as_str()) {
                        ent_obj.entry(k.clone()).or_insert_with(|| val.clone());
                    }
                }
            }
        }
    }
    // Aliases + title warning apply to new and same-kind merges alike.
    let ent = reg.topics.get_mut(&subj).unwrap();
    let ent_obj = ent.as_object_mut().unwrap();
    if let Some(aliases) = row.get("aliases").and_then(Value::as_array) {
        let list = ent_obj
            .entry("aliases".to_string())
            .or_insert_with(|| json!([]));
        if let Some(list) = list.as_array_mut() {
            for a in aliases {
                let a = a.as_str().unwrap_or("").trim().to_string();
                if !a.is_empty() && !list.iter().any(|e| e.as_str() == Some(a.as_str())) {
                    list.push(json!(a));
                }
            }
        }
    }
    let new_title = field_str(&row["title"]);
    let cur_title = ent_obj.get("title").and_then(Value::as_str).unwrap_or("").to_string();
    if !new_title.is_empty() && new_title != cur_title {
        let warnings = out
            .entry("warnings".to_string())
            .or_insert_with(|| json!([]));
        if let Some(list) = warnings.as_array_mut() {
            list.push(json!(format!(
                "topic {subj:?}: title {cur_title:?} kept over {new_title:?}"
            )));
        }
    }
    false
}

/// review.py `add_item` — delegated to `crate::review::add_item`
/// (schema-validated item shape; staged queue wins over disk so repeated
/// calls compose).
fn review_add_item(
    txn: &mut Txn,
    kind: &str,
    problem: &str,
    evidence: &[String],
    affected: &[String],
    risk: &str,
    proposed: &str,
) -> Result<(), EngineError> {
    crate::review::add_item(
        txn,
        kind,
        problem,
        evidence.iter().map(|s| json!(s)).collect(),
        affected.iter().map(|s| json!(s)).collect(),
        risk,
        proposed,
    )?;
    Ok(())
}

/// `topics.merge_rows` (topics.py:106-126) — merge declared topics.jsonl
/// rows into the registry, staged via `txn`. Rows are validated first
/// (subject/kind/title required — the parse_topics_file field checks);
/// entry-level extras are kept verbatim. Kind conflicts mark the entry
/// `conflict`, list the subject in `kind_conflicts`, and enqueue a
/// `topic_kind_conflict` review item.
pub fn merge_rows(
    txn: &mut Txn,
    rows: &[Value],
    declared_by: &Value,
) -> Result<Value, EngineError> {
    // Whole-batch validation up front — no partial merge on bad input.
    let kinds: Vec<String> = rows
        .iter()
        .enumerate()
        .map(|(i, r)| validate_row(r, i))
        .collect::<Result<_, _>>()?;

    let mut reg = match txn.staged_value(TOPICS_FILE) {
        Some(v) => Topics::from_value(v)?,
        None => load(txn.wiki)?,
    };
    let mut out = Map::new();
    out.insert("declared".to_string(), json!(rows.len()));
    out.insert("registered".to_string(), json!(0));
    out.insert("kind_conflicts".to_string(), json!([]));
    out.insert("warnings".to_string(), json!([]));

    for (row, kind) in rows.iter().zip(kinds.iter()) {
        let subj = field_str(&row["subject"]);
        if merge_one(&mut reg, row, kind, declared_by, &mut out) {
            review_add_item(
                txn,
                "topic_kind_conflict",
                &format!("subject {subj:?} declared with conflicting kinds"),
                &[format!(
                    "declared_by: {}",
                    serde_json::to_string(declared_by)?
                )],
                &[subj.clone()],
                "medium",
                reg.topics
                    .get(&subj)
                    .and_then(|e| e.get("kind"))
                    .and_then(Value::as_str)
                    .unwrap_or(""),
            )?;
        }
    }
    if !rows.is_empty() {
        reg.version += 1;
        txn.stage_state(TOPICS_FILE, &reg.to_value())?;
    }
    Ok(Value::Object(out))
}

/// topics.py `ensure_source_topic` — register a `source`-kind entry when
/// absent, else append declared_by (each bumps version when it stages).
pub fn ensure_source_topic(
    txn: &mut Txn,
    subject: &str,
    title: &str,
    declared_by: &Value,
) -> Result<(), EngineError> {
    let mut reg = match txn.staged_value(TOPICS_FILE) {
        Some(v) => Topics::from_value(v)?,
        None => load(txn.wiki)?,
    };
    match reg.topics.get_mut(subject) {
        None => {
            let mut ent = entry("source", title, declared_by);
            ent.as_object_mut()
                .unwrap()
                .insert("auto_registered".to_string(), json!(true));
            reg.topics.insert(subject.to_string(), ent);
            reg.version += 1;
            txn.stage_state(TOPICS_FILE, &reg.to_value())
        }
        Some(ent) => {
            let ent_obj = ent.as_object_mut().unwrap();
            let db = ent_obj
                .entry("declared_by".to_string())
                .or_insert_with(|| json!([]));
            let mut staged = false;
            if let Some(list) = db.as_array_mut() {
                if !list.iter().any(|e| e == declared_by) {
                    list.push(declared_by.clone());
                    staged = true;
                }
            }
            if staged {
                reg.version += 1;
                txn.stage_state(TOPICS_FILE, &reg.to_value())?;
            }
            Ok(())
        }
    }
}

/// topics.py `auto_register` — undeclared subjects become kind=concept
/// entries titled from the subject slug; `auto_registered: true`.
pub fn auto_register(
    txn: &mut Txn,
    subjects: &[String],
    declared_by: &Value,
) -> Result<Value, EngineError> {
    let mut reg = match txn.staged_value(TOPICS_FILE) {
        Some(v) => Topics::from_value(v)?,
        None => load(txn.wiki)?,
    };
    let mut added: Vec<String> = Vec::new();
    for s in subjects {
        if reg.topics.contains_key(s) {
            continue;
        }
        let mut ent = entry("concept", &title_for(s), declared_by);
        ent.as_object_mut()
            .unwrap()
            .insert("auto_registered".to_string(), json!(true));
        reg.topics.insert(s.clone(), ent);
        added.push(s.clone());
    }
    if !added.is_empty() {
        reg.version += 1;
        txn.stage_state(TOPICS_FILE, &reg.to_value())?;
    }
    added.sort();
    Ok(json!({"auto_registered": added}))
}

/// topics.py `unclaimed` — declared non-source/decision subjects with no
/// claims.
pub fn unclaimed(wiki: &Wiki) -> Result<Vec<String>, EngineError> {
    let have: std::collections::BTreeSet<String> =
        subjects(wiki)?.into_iter().collect();
    let reg = load(wiki)?;
    Ok(reg
        .topics
        .iter()
        .filter(|(s, e)| {
            e.get("kind").and_then(Value::as_str) != Some("source")
                && e.get("kind").and_then(Value::as_str) != Some("decision")
                && !have.contains(*s)
        })
        .map(|(s, _)| s.clone())
        .collect())
}

// ---- kind routing (synthesize.py ports needed by backfill) -------------

/// synthesize.py `_load_kind_rules` — ordered rules + plural default from
/// page-kinds.json; malformed inputs degrade to `([], "concepts")` with a
/// warning string appended.
fn load_kind_rules(wiki: &Wiki, warnings: &mut Vec<String>) -> (Vec<Value>, String) {
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
    match data.get("rules") {
        Some(v) if !v.is_array() => {
            warnings.push(format!("{KIND_RULES_FILE} ignored: 'rules' must be a list"));
        }
        _ => {}
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
        if !kind_ok {
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
        rules.push(r.clone());
    }
    (rules, default)
}

/// synthesize.py `_rule_matches` — every supplied matcher key must match.
fn rule_matches(rule: &Value, subject: &str, clist: &[Value]) -> bool {
    for m in ["subject", "prefix", "suffix", "regex", "predicate"] {
        let Some(v) = rule.get(m).and_then(Value::as_str) else {
            continue;
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
                        re.is_match(c.get("predicate").and_then(Value::as_str).unwrap_or(""))
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

/// synthesize.py `_heuristic_kind` — built-in rules 1-4, first hit wins.
fn heuristic_kind(subject: &str, clist: &[Value]) -> Option<&'static str> {
    // 1. any claim predicate looks procedural
    let procedural = regex::Regex::new(r"^(step|procedure|runbook|how[ _-]?to)").unwrap();
    if clist.iter().any(|c| {
        procedural.is_match(c.get("predicate").and_then(Value::as_str).unwrap_or(""))
    }) {
        return Some("procedures");
    }
    // 2. ticket id (stc-97) or dotted component (x.api/.service/.ui/.db/.cli)
    let ticketish = regex::Regex::new(r"^[a-z]+-\d+$|\.(api|service|ui|db|cli)$").unwrap();
    if ticketish.is_match(subject) {
        return Some("entities");
    }
    // 3. single-token code identifier with >=2 claims
    let code_ident = regex::Regex::new(
        r"^[a-z0-9_]+(?:[A-Z][a-z0-9]*)*$|^[a-z0-9][a-z0-9_\-]*$",
    )
    .unwrap();
    if clist.len() >= 2 && code_ident.is_match(subject) {
        return Some("entities");
    }
    // 4. open-question marker on subject or any predicate
    let q = regex::Regex::new(r"open[ _-]?question|unresolved|tbd").unwrap();
    if q.is_match(subject)
        || clist.iter().any(|c| {
            q.is_match(c.get("predicate").and_then(Value::as_str).unwrap_or(""))
        })
    {
        return Some("questions");
    }
    None
}

/// synthesize.py `_route_kind` — file rules first, then heuristics, then
/// the default (plural) kind.
fn route_kind(subject: &str, clist: &[Value], rules: &[Value], default: &str) -> String {
    for rule in rules {
        if rule_matches(rule, subject, clist) {
            return rule["kind"].as_str().unwrap_or(default).to_string();
        }
    }
    if let Some(k) = heuristic_kind(subject, clist) {
        return k.to_string();
    }
    default.to_string()
}

/// topics.py `backfill` — register every existing claim subject
/// (idempotent; never overwrites). Kind comes from an existing wiki page,
/// then kind rules, then built-in heuristics, then `default`.
/// Commits `{topics_registered}` inside its own `wiki-topics-backfill`
/// transaction when anything was added.
pub fn backfill(wiki: &Wiki) -> Result<Value, EngineError> {
    let mut reg = load(wiki)?;
    let mut warnings: Vec<String> = Vec::new();
    let (rules, default_kind) = load_kind_rules(wiki, &mut warnings);

    let mut groups: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for c in claims::list(wiki)? {
        groups
            .entry(c["subject"].as_str().unwrap_or("").to_string())
            .or_default()
            .push(c);
    }

    let declared_by = json!({"source_id": "backfill", "version": 0});
    let mut added = 0i64;
    for (subj, clist) in &groups {
        if reg.topics.contains_key(subj) {
            continue;
        }
        let slug = slugify(subj);
        let mut kind: Option<&str> = None;
        for (k, dir) in PAGE_DIR {
            if wiki.p(&format!("wiki/{dir}/{slug}.md")).is_file() {
                kind = Some(k);
                break;
            }
        }
        let k = match kind {
            Some(k) => k.to_string(),
            None => {
                let plural = route_kind(subj, clist, &rules, &default_kind);
                plural_to_kind(&plural).unwrap_or("concept").to_string()
            }
        };
        let mut ent = entry(&k, &title_for(subj), &declared_by);
        ent.as_object_mut()
            .unwrap()
            .insert("auto_registered".to_string(), json!(true));
        reg.topics.insert(subj.clone(), ent);
        added += 1;
    }
    if added > 0 {
        reg.version += 1;
        let run_id = ids::new_run();
        let mut txn = Txn::begin_with(
            wiki,
            &run_id,
            TxnOpts {
                command: Some("wiki-topics-backfill".to_string()),
                ..Default::default()
            },
        )?;
        txn.stage_state(TOPICS_FILE, &reg.to_value())?;
        txn.changes
            .insert("topics_registered".to_string(), json!(added));
        txn.commit()?;
    }
    Ok(json!({
        "topics_registered": added,
        "warnings": warnings,
    }))
}
