//! Read-only knowledge-graph payload — port of `wikicore/graphdata.py`.
//!
//! `llm-wiki graph-data` prints one JSON object: entity/source/decision/page
//! nodes and typed links derived from `.llm-wiki/claims`, `decisions/`,
//! `sources/`. Deterministic: identical wiki state → byte-identical output
//! except `generated_at`.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::sync::LazyLock;

use regex::Regex;
use serde_json::{json, Value};

use crate::claims;
use crate::deps;
use crate::err::EngineError;
use crate::frontmatter::{self, PAGE_KINDS};
use crate::store::Wiki;
use crate::util::{now_iso, py_str};

/// graphdata.py:19 — stc/pm ticket-id subjects get wtype "ticket".
static TICKET_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?:stc|pm)-?\d+$").unwrap());

/// graphdata.py:21 — alias words too generic to imply a "mentions" edge.
/// `MENTION_STOP` env var (comma-separated) replaces this set for a run.
const MENTION_STOP: &[&str] = &[
    "stock", "workflow", "part", "build", "order", "item", "items", "detail",
    "list", "modal", "api", "ui", "feature", "status", "field", "source",
    "data", "page", "tab", "table", "column", "po", "bo", "service",
];

/// graphdata.py:30 — predicates that lead an entity node's summary.
const SUMMARY_PREDICATES: &[&str] =
    &["status", "summary", "verdict", "feature", "title"];

/// claims.py:28 — statuses that mark a node fully terminal.
const TERMINAL: &[&str] = &["superseded", "rejected"];

/// review.py:15 — queue scanned for open items affecting claims.
const REVIEW_QUEUE: &str = ".state/review-queue.jsonl";

static TOKEN_SPLIT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[^0-9a-z]+").unwrap());
static BLOB_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[a-z0-9_]+").unwrap());
const TOKEN_SUFFIXES: &[&str] = &["tion", "ment", "ing", "ed", "ly", "s"];

/// `text.norm_tokens` — lowercase, split on non-alnum, drop tokens < 3 chars,
/// strip one conservative trailing suffix ('s' leaves ≥3, others ≥4).
fn norm_tokens(s: &str) -> HashSet<String> {
    let mut out = HashSet::new();
    for tok in TOKEN_SPLIT_RE.split(&s.to_lowercase()) {
        if tok.len() < 3 {
            continue;
        }
        let mut t = tok.to_string();
        for suf in TOKEN_SUFFIXES {
            if t.ends_with(suf) && t.len() - suf.len() >= (if *suf == "s" { 3 } else { 4 }) {
                t.truncate(t.len() - suf.len());
                break;
            }
        }
        out.insert(t);
    }
    out
}

fn mention_stop() -> HashSet<String> {
    match std::env::var("MENTION_STOP") {
        Err(_) => MENTION_STOP.iter().map(|s| s.to_string()).collect(),
        Ok(env) => env
            .split(',')
            .map(|w| w.trim().to_lowercase())
            .filter(|w| !w.is_empty())
            .collect(),
    }
}

/// `claims.py:_scan_decisions` — canonical `decisions/decision_*.json`
/// scan, sorted by filename.
fn scan_decisions(wiki: &Wiki) -> Result<Vec<Value>, EngineError> {
    let mut out = Vec::new();
    let ddir = wiki.p("decisions");
    if !ddir.is_dir() {
        return Ok(out);
    }
    let mut names: Vec<String> = Vec::new();
    for e in std::fs::read_dir(&ddir)? {
        let e = e?;
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with("decision_") && name.ends_with(".json") {
            names.push(name);
        }
    }
    names.sort();
    for name in names {
        let text = std::fs::read_to_string(ddir.join(&name))?;
        out.push(serde_json::from_str(&text)?);
    }
    Ok(out)
}

/// `wiki.read_jsonl` — see `Wiki::read_jsonl` (store.py parity).
fn read_jsonl(wiki: &Wiki, rel: &str) -> Result<Vec<Value>, EngineError> {
    wiki.read_jsonl(rel)
}

/// `review.summary` — queue counts consumed by meta.review.
fn review_summary(wiki: &Wiki) -> Result<Value, EngineError> {
    let items = read_jsonl(wiki, REVIEW_QUEUE)?;
    let open = items
        .iter()
        .filter(|i| i.get("status").and_then(Value::as_str) == Some("open"))
        .count() as i64;
    let deferred = items
        .iter()
        .filter(|i| i.get("status").and_then(Value::as_str) == Some("deferred"))
        .count() as i64;
    Ok(json!({"open": open, "deferred": deferred, "total": items.len() as i64}))
}

/// `_entity_key` — first two dotted subject segments.
fn entity_key(subject: &str) -> String {
    subject.split('.').take(2).collect::<Vec<_>>().join(".")
}

/// `_summarize` — `(summary-predicate claims ++ rest)[:6]` candidates,
/// latest write per predicate wins, first 4 emitted. `c not in pri` is
/// value-equality (Python list membership), not predicate exclusion.
fn summarize(clist: &[&Value]) -> Vec<Value> {
    let pri: Vec<Value> = clist
        .iter()
        .filter(|c| SUMMARY_PREDICATES.contains(&c["predicate"].as_str().unwrap_or("")))
        .map(|c| (*c).clone())
        .collect();
    let mut pick: Vec<&Value> = clist
        .iter()
        .filter(|c| pri.contains(*c))
        .copied()
        .collect();
    pick.extend(clist.iter().filter(|c| !pri.contains(*c)).copied());
    pick.truncate(6);
    let mut seen: Vec<(String, Value)> = Vec::new();
    for c in pick {
        let p = c["predicate"].as_str().unwrap_or("").to_string();
        let v = c.get("value").cloned().unwrap_or(Value::Null);
        if let Some(slot) = seen.iter_mut().find(|(k, _)| *k == p) {
            slot.1 = v;
        } else {
            seen.push((p, v));
        }
    }
    seen.into_iter()
        .take(4)
        .map(|(p, v)| json!({"p": p, "v": v}))
        .collect()
}

/// `_load_type_map` — `graph-types.json` object of string→string; any
/// malformed content warns and yields the empty map.
fn load_type_map(wiki: &Wiki, warnings: &mut Vec<Value>) -> BTreeMap<String, String> {
    let path = wiki.p("graph-types.json");
    if !path.is_file() {
        return BTreeMap::new();
    }
    let parsed = (|| -> Result<BTreeMap<String, String>, String> {
        let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
        let v: Value = serde_json::from_str(&text).map_err(|e| e.to_string())?;
        let obj = v
            .as_object()
            .ok_or_else(|| "expected object of string->string".to_string())?;
        let mut m = BTreeMap::new();
        for (k, v) in obj {
            match v.as_str() {
                Some(s) => {
                    m.insert(k.clone(), s.to_string());
                }
                None => return Err("expected object of string->string".to_string()),
            }
        }
        Ok(m)
    })();
    match parsed {
        Ok(m) => m,
        Err(e) => {
            warnings.push(json!(format!("graph-types.json ignored: {e}")));
            BTreeMap::new()
        }
    }
}

/// `_wtype` — second segment → type_map | ticket | domain.
fn wtype(key: &str, type_map: &BTreeMap<String, String>) -> String {
    let seg = if key.contains('.') {
        key.split('.').nth(1).unwrap_or(key)
    } else {
        key
    };
    if let Some(t) = type_map.get(seg) {
        return t.clone();
    }
    if TICKET_RE.is_match(seg) {
        return "ticket".to_string();
    }
    "domain".to_string()
}

/// `_size` — display radius per node kind.
fn size(kind: &str, claim_count: usize) -> f64 {
    if kind == "source" {
        return 5.0;
    }
    if kind == "decision" {
        return 7.0;
    }
    (6.0 * (claim_count.max(1) as f64).sqrt()).max(4.0)
}

/// `_list_sources` — `sources/*/manifest.json` values, dir-name sorted.
fn list_sources(wiki: &Wiki) -> Result<Vec<Value>, EngineError> {
    let sdir = wiki.p("sources");
    let mut out = Vec::new();
    if !sdir.is_dir() {
        return Ok(out);
    }
    let mut names: Vec<String> = Vec::new();
    for e in std::fs::read_dir(&sdir)? {
        let e = e?;
        names.push(e.file_name().to_string_lossy().into_owned());
    }
    names.sort();
    for name in names {
        if !sdir.join(&name).join("manifest.json").is_file() {
            continue;
        }
        if let Ok(m) = wiki.load_json(&format!("sources/{name}/manifest.json")) {
            out.push(m);
        }
    }
    Ok(out)
}

/// `graphdata.graph_payload(wiki, full)` — nodes[]/links[] graph payload.
pub fn graph_payload(wiki: &Wiki, full: bool) -> Result<Value, EngineError> {
    if !wiki.exists() {
        return Err(EngineError::Txn(
            "wiki not initialized: run wiki init first".to_string(),
        ));
    }
    let mut warnings: Vec<Value> = Vec::new();
    let cfg = wiki.config()?;
    let project = cfg.get("project").and_then(Value::as_str).unwrap_or("");

    let mut all_claims = claims::list(wiki)?;
    all_claims.sort_by(|a, b| {
        a.get("id")
            .map(|v| v.to_string())
            .cmp(&b.get("id").map(|v| v.to_string()))
    });
    let mut decisions = scan_decisions(wiki)?;
    decisions.sort_by(|a, b| {
        a.get("id")
            .map(|v| v.to_string())
            .cmp(&b.get("id").map(|v| v.to_string()))
    });
    let manifests = list_sources(wiki)?;
    let type_map = load_type_map(wiki, &mut warnings);

    // Group claims by entity key; insertion order preserved for link
    // emission order (Python dict parity).
    let mut by_ent: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut ent_order: Vec<String> = Vec::new();
    for c in &all_claims {
        let key = entity_key(c.get("subject").and_then(Value::as_str).unwrap_or(""));
        if !by_ent.contains_key(&key) {
            ent_order.push(key.clone());
        }
        by_ent.entry(key).or_default().push(c.clone());
    }
    let live_claims: Vec<&Value> = all_claims
        .iter()
        .filter(|c| c.get("status").and_then(Value::as_str) != Some("superseded"))
        .collect();
    let mut live_by_ent: BTreeMap<String, Vec<&Value>> = BTreeMap::new();
    let mut live_ent_order: Vec<String> = Vec::new();
    for c in &live_claims {
        let key = entity_key(c.get("subject").and_then(Value::as_str).unwrap_or(""));
        if !live_by_ent.contains_key(&key) {
            live_ent_order.push(key.clone());
        }
        live_by_ent.entry(key).or_default().push(c);
    }
    let claim_to_ent: HashMap<String, String> = all_claims
        .iter()
        .map(|c| {
            (
                c["id"].as_str().unwrap_or("").to_string(),
                entity_key(c.get("subject").and_then(Value::as_str).unwrap_or("")),
            )
        })
        .collect();

    // Labels: strip "<project>." prefix; on collision keep the full key.
    let mut raw_labels: BTreeMap<String, String> = BTreeMap::new();
    for key in by_ent.keys() {
        let stripped = if !project.is_empty() && key.starts_with(&format!("{project}.")) {
            key[project.len() + 1..].to_string()
        } else {
            key.clone()
        };
        raw_labels.insert(key.clone(), stripped);
    }
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for lbl in raw_labels.values() {
        *counts.entry(lbl.as_str()).or_insert(0) += 1;
    }
    let labels: HashMap<String, String> = raw_labels
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                if counts.get(v.as_str()).copied().unwrap_or(0) > 1 {
                    k.clone()
                } else {
                    v.clone()
                },
            )
        })
        .collect();

    let mut nodes: Vec<Value> = Vec::new();
    let mut ent_ids: HashMap<String, usize> = HashMap::new();
    for (i, key) in ent_order_sorted(by_ent.keys()).iter().enumerate() {
        let key = key.clone();
        ent_ids.insert(key.clone(), i);
        let clist = by_ent.get(&key).unwrap();
        let refs: Vec<&Value> = clist.iter().collect();
        let mut cited: BTreeSet<String> = BTreeSet::new();
        for c in clist {
            for ev in c
                .get("evidence")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(sid) = ev.get("source_id").and_then(Value::as_str) {
                    if !sid.is_empty() {
                        cited.insert(sid.to_string());
                    }
                }
            }
        }
        let mut auth: BTreeSet<String> = BTreeSet::new();
        for c in clist {
            if let Some(s) = c
                .get("authority")
                .and_then(|a| a.get("source"))
                .and_then(Value::as_str)
            {
                if !s.is_empty() {
                    auth.insert(s.to_string());
                }
            }
        }
        let mut sources: Vec<Value> = cited.iter().map(|s| json!(s)).collect();
        for a in &auth {
            if !cited.contains(a) {
                sources.push(json!(a));
            }
        }
        let mut node = json!({
            "id": i,
            "kind": "entity",
            "key": key,
            "label": labels.get(&key).cloned().unwrap_or_else(|| key.clone()),
            "wtype": wtype(&key, &type_map),
            "claim_count": clist.len() as i64,
            "size": size("entity", clist.len()),
            "summary": summarize(&refs),
            "claim_ids": clist
                .iter()
                .map(|c| c["id"].as_str().unwrap_or("").to_string())
                .collect::<Vec<_>>(),
            "sources": sources,
        });
        if full {
            node["claims"] = json!(clist);
        }
        nodes.push(node);
    }

    let mut src_ids: HashMap<String, usize> = HashMap::new();
    let mut src_nodes: Vec<Value> = Vec::new();
    for m in &manifests {
        let sid = m.get("id").and_then(Value::as_str).unwrap_or("");
        if sid.is_empty() {
            continue;
        }
        let idx = nodes.len() + src_nodes.len();
        src_ids.insert(sid.to_string(), idx);
        // `m.get("title") or basename(str(m.get("origin", sid)))` — the
        // basename falls back to sid when origin is absent, but the
        // summary/origin fields use m.get("origin", "") below.
        let title = m
            .get("title")
            .filter(|t| crate::frontmatter::truthy(Some(t)))
            .map(|t| t.as_str().map(|s| s.to_string()).unwrap_or_else(|| py_str(t)))
            .unwrap_or_else(|| {
                let o = m
                    .get("origin")
                    .map(py_str)
                    .unwrap_or_else(|| sid.to_string());
                o.rsplit('/').next().unwrap_or(&o).to_string()
            });
        src_nodes.push(json!({
            "id": idx,
            "kind": "source",
            "key": sid,
            "label": title,
            "wtype": "source",
            "claim_count": 0,
            "size": size("source", 0),
            "summary": [{"p": "document", "v": m.get("origin").cloned().unwrap_or(json!(""))}],
            "claims": [],
            "sources": [sid],
            "title": title,
            "origin": m.get("origin").cloned().unwrap_or(Value::Null),
        }));
    }

    let mut dec_nodes: Vec<Value> = Vec::new();
    let mut dec_ids: HashMap<String, usize> = HashMap::new();
    for d in &decisions {
        let idx = nodes.len() + src_nodes.len() + dec_nodes.len();
        let did = d.get("id").and_then(Value::as_str).unwrap_or("").to_string();
        dec_ids.insert(did.clone(), idx);
        let mut dec_sources = BTreeSet::new();
        for ev in d
            .get("evidence")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(sid) = ev.get("source_id").and_then(Value::as_str) {
                if !sid.is_empty() {
                    dec_sources.insert(sid.to_string());
                }
            }
        }
        dec_nodes.push(json!({
            "id": idx,
            "kind": "decision",
            "key": did,
            "label": d.get("title").cloned().unwrap_or_else(|| json!(did)),
            "wtype": "decision",
            "claim_count": 0,
            "size": size("decision", 0),
            "summary": [
                {"p": "status", "v": d.get("status").and_then(Value::as_str).unwrap_or("")},
                {"p": "decided_on", "v": d.get("decided_on").cloned().unwrap_or(Value::Null)},
            ],
            "claims": d.get("claims").cloned().unwrap_or(json!([])),
            "sources": dec_sources.into_iter().collect::<Vec<_>>(),
        }));
    }

    // Agent-authored wiki/<kind>/*.md pages; deterministic: sorted by path.
    let mut pages: Vec<Value> = Vec::new();
    let mut page_ids: HashMap<String, usize> = HashMap::new();
    let mut page_rels: Vec<String> = Vec::new();
    for kind in PAGE_KINDS {
        let pdir = wiki.p("wiki").join(kind);
        if !pdir.is_dir() {
            continue;
        }
        for e in std::fs::read_dir(&pdir)? {
            let e = e?;
            let name = e.file_name().to_string_lossy().into_owned();
            if name.ends_with(".md") && name != "index.md" {
                page_rels.push(format!("wiki/{kind}/{name}"));
            }
        }
    }
    page_rels.sort();
    for rel in &page_rels {
        let text = std::fs::read_to_string(wiki.p(rel))?;
        let (front, body) = frontmatter::split_front(&text)?;
        let singular = frontmatter::kind_type(rel.split('/').nth(1).unwrap_or(""));
        let idx = nodes.len() + src_nodes.len() + dec_nodes.len() + pages.len();
        page_ids.insert(rel.clone(), idx);
        let mut summary = vec![
            json!({"p": "type", "v": singular}),
            json!({"p": "stale", "v": frontmatter::truthy(front.get("stale"))}),
        ];
        if frontmatter::truthy(front.get("title")) {
            summary.push(json!({"p": "title", "v": front.get("title").cloned().unwrap()}));
        }
        // `front.get("title") or basename(rel)[:-3]`
        let label = front
            .get("title")
            .filter(|t| frontmatter::truthy(Some(t)))
            .map(|t| t.as_str().map(|s| s.to_string()).unwrap_or_else(|| py_str(t)))
            .unwrap_or_else(|| {
                rel.rsplit('/')
                    .next()
                    .unwrap_or(rel)
                    .strip_suffix(".md")
                    .unwrap_or(rel)
                    .to_string()
            });
        pages.push(json!({
            "id": idx,
            "kind": "page",
            "key": rel,
            "label": label,
            "wtype": format!("page_{singular}"),
            "claim_count": 0,
            "size": 5.0,
            "summary": summary,
            "claims": [],
            "sources": [],
            "path": rel,
            // T9: prose payload for the viz reader pane, capped.
            "body": body.chars().take(20000).collect::<String>(),
        }));
    }

    let mut links: Vec<Value> = Vec::new();
    let mut eset: HashSet<(usize, usize, String)> = HashSet::new();
    fn add(
        eset: &mut HashSet<(usize, usize, String)>,
        links: &mut Vec<Value>,
        a: usize,
        b: usize,
        t: &str,
        w: i64,
    ) {
        if a == b || !eset.insert((a, b, t.to_string())) {
            return;
        }
        links.push(json!({"source": a, "target": b, "type": t, "w": w}));
    }

    // evidence edges: live entity claims → source nodes
    for key in live_ent_order.iter() {
        if let Some(lst) = live_by_ent.get(key) {
            let mut sids = BTreeSet::new();
            for c in lst {
                for ev in c
                    .get("evidence")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(sid) = ev.get("source_id").and_then(Value::as_str) {
                        if !sid.is_empty() {
                            sids.insert(sid.to_string());
                        }
                    }
                }
            }
            for sid in sids {
                if let (Some(&a), Some(&b)) = (ent_ids.get(key), src_ids.get(&sid)) {
                    add(&mut eset, &mut links, a, b, "evidence", 1);
                }
            }
        }
    }

    // contains edges: parent dotted prefix → child key
    for key in ent_order.iter() {
        let parent = key
            .rsplit_once('.')
            .map(|(p, _)| p.to_string())
            .unwrap_or_default();
        if by_ent.contains_key(&parent) && parent != *key {
            if let (Some(&a), Some(&b)) = (ent_ids.get(&parent), ent_ids.get(key)) {
                add(&mut eset, &mut links, a, b, "contains", 1);
            }
        }
    }

    // Alias matching (Q1/Q8): normalized token overlap, not a raw prefix
    // regex. Alias = second key segment minus project-qualifying prefix,
    // minimum 4 chars, stop-list filtered.
    let stop = mention_stop();
    let mut alias_stems: Vec<(String, HashSet<String>)> = Vec::new();
    for key in ent_order.iter() {
        let lst = by_ent.get(key).unwrap();
        let seg = if key.contains('.') {
            key.split('.').nth(1).unwrap_or(key)
        } else {
            key.as_str()
        };
        let mut alias = seg.to_lowercase();
        if !project.is_empty() {
            let low = project.to_lowercase();
            for sep in ["_", "-", "."] {
                if alias.starts_with(&format!("{low}{sep}")) {
                    alias = alias[low.len() + 1..].to_string();
                    break;
                }
            }
            if alias == low {
                alias = String::new();
            }
        }
        let stems = norm_tokens(&alias);
        if alias.len() >= 4 && !stems.is_empty() && stems.is_disjoint(&stop) && lst.len() >= 2 {
            alias_stems.push((key.clone(), stems));
        }
    }
    for key in ent_order.iter() {
        let lst = by_ent.get(key).unwrap();
        let blob = lst
            .iter()
            .map(|c| {
                format!(
                    "{} {}",
                    py_str(&c.get("predicate").cloned().unwrap_or(json!(""))),
                    py_str(&c.get("value").cloned().unwrap_or(json!("")))
                )
            })
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase();
        let tokens: Vec<&str> = BLOB_TOKEN_RE.find_iter(&blob).map(|m| m.as_str()).collect();
        for (other, stem) in &alias_stems {
            if other == key {
                continue;
            }
            let hits = tokens
                .iter()
                .filter(|t| !norm_tokens(t).is_disjoint(stem))
                .count() as i64;
            if hits >= 2 {
                if let (Some(&a), Some(&b)) = (ent_ids.get(other), ent_ids.get(key)) {
                    add(&mut eset, &mut links, a, b, "mentions", hits);
                }
            }
        }
    }

    // decision edges: decision → resolved-claim entity / evidence source
    for d in &decisions {
        let did = d.get("id").and_then(Value::as_str).unwrap_or("").to_string();
        for cid in d
            .get("claims")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            if let Some(ent) = claim_to_ent.get(cid) {
                if let (Some(&a), Some(&b)) = (dec_ids.get(&did), ent_ids.get(ent)) {
                    add(&mut eset, &mut links, a, b, "decision", 1);
                }
            }
        }
        for ev in d
            .get("evidence")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(sid) = ev.get("source_id").and_then(Value::as_str) {
                if let (Some(&a), Some(&b)) = (dec_ids.get(&did), src_ids.get(sid)) {
                    add(&mut eset, &mut links, a, b, "evidence", 1);
                }
            }
        }
    }

    // dep edges → documents / depends_on links
    let dep_edges = deps::graph(wiki)?
        .get("edges")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    for (artifact, dep_list) in dep_edges.iter() {
        let page_id = match page_ids.get(artifact) {
            Some(&p) => p,
            None => continue,
        };
        for dep in dep_list.as_array().into_iter().flatten().filter_map(Value::as_str) {
            let dep_id = dep.split('@').next().unwrap_or("");
            if dep_id.starts_with("claim_") {
                if let Some(ent) = claim_to_ent.get(dep_id) {
                    if let Some(&a) = ent_ids.get(ent) {
                        add(&mut eset, &mut links, a, page_id, "documents", 1);
                    }
                }
            } else if dep_id.starts_with("decision_") {
                if let Some(&a) = dec_ids.get(dep_id) {
                    add(&mut eset, &mut links, a, page_id, "documents", 1);
                }
            } else if let Some(&a) = page_ids.get(dep_id) {
                add(&mut eset, &mut links, a, page_id, "depends_on", 1);
            }
        }
    }

    let mut all_nodes = nodes;
    all_nodes.extend(src_nodes.iter().cloned());
    all_nodes.extend(dec_nodes.iter().cloned());
    all_nodes.extend(pages.iter().cloned());
    let src_count = src_nodes.len() as i64;
    let dec_count = dec_nodes.len() as i64;
    let page_count = pages.len() as i64;

    // Review-surface annotations (R3): per-node flags plus the queue
    // summary under meta.
    let mut open_affected: HashSet<String> = HashSet::new();
    for item in read_jsonl(wiki, REVIEW_QUEUE)? {
        if item.get("status").and_then(Value::as_str) == Some("open") {
            for a in item
                .get("affected")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
            {
                open_affected.insert(a.split('@').next().unwrap_or("").to_string());
            }
        }
    }
    let claim_status: HashMap<String, String> = all_claims
        .iter()
        .map(|c| {
            (
                c["id"].as_str().unwrap_or("").to_string(),
                c.get("status").and_then(Value::as_str).unwrap_or("").to_string(),
            )
        })
        .collect();
    let dec_status: HashMap<String, String> = decisions
        .iter()
        .map(|d| {
            (
                d.get("id").and_then(Value::as_str).unwrap_or("").to_string(),
                d.get("status").and_then(Value::as_str).unwrap_or("").to_string(),
            )
        })
        .collect();
    let mut degree: HashMap<usize, usize> = HashMap::new();
    for l in &links {
        *degree.entry(l["source"].as_u64().unwrap_or(0) as usize).or_insert(0) += 1;
        *degree.entry(l["target"].as_u64().unwrap_or(0) as usize).or_insert(0) += 1;
    }
    for n in all_nodes.iter_mut() {
        // `n.get("claim_ids") or n.get("claims")` — empty list falls through.
        let raw: Vec<String> = n
            .get("claim_ids")
            .and_then(Value::as_array)
            .filter(|a| !a.is_empty())
            .or_else(|| n.get("claims").and_then(Value::as_array))
            .map(|a| {
                // cid lists are strings; non-strings skip (dicts would be
                // unhashable in the Python `set(raw)` anyway)
                a.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let cids: HashSet<String> = raw.into_iter().collect();
        let mut statuses: HashSet<String> = cids
            .iter()
            .map(|c| claim_status.get(c).cloned().unwrap_or_default())
            .collect();
        if n["kind"].as_str() == Some("decision") {
            statuses.insert(
                dec_status
                    .get(n["key"].as_str().unwrap_or(""))
                    .cloned()
                    .unwrap_or_default(),
            );
        }
        let terminal: HashSet<String> = statuses
            .iter()
            .filter(|s| TERMINAL.contains(&s.as_str()))
            .cloned()
            .collect();
        let stale = !terminal.is_empty()
            && terminal == statuses.iter().filter(|s| !s.is_empty()).cloned().collect();
        n["flags"] = json!({
            "review_open": cids.iter().any(|c| open_affected.contains(c)),
            "disputed": statuses.contains("disputed"),
            "superseded": statuses.contains("superseded"),
            "stale": stale,
            "orphan": degree.get(&(n["id"].as_u64().unwrap_or(0) as usize)).copied().unwrap_or(0) == 0,
        });
    }

    let generated_at = now_iso();
    let review = review_summary(wiki)?;
    Ok(json!({
        "project": project,
        "nodes": all_nodes,
        "links": links,
        "claim_count": all_claims.len() as i64,
        "source_count": src_count,
        "decision_count": dec_count,
        "page_count": page_count,
        "generated_at": generated_at,
        "meta": {"review": review, "generated_at": generated_at},
        "warnings": warnings,
    }))
}

/// Sorted entity keys — Python emits nodes in `sorted(by_ent)` order while
/// iterating `by_ent` in insertion order for edges. Both orderings needed.
fn ent_order_sorted<'a>(keys: impl Iterator<Item = &'a String>) -> Vec<String> {
    let mut v: Vec<String> = keys.cloned().collect();
    v.sort();
    v
}
