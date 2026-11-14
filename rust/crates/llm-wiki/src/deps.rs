//! Dependency graph between canonical knowledge and derived artifacts —
//! `.state/dependencies.json` edge registry. Port of wikicore/deps.py plus
//! the dep-liveness rule set used by dream.py:182-197.
//!
//! Deps are "<id>@<version>"; invalidation matches ANY version of a changed
//! id (deps.py:5-6, `dep.split("@", 1)[0]`). Changed ids propagate through
//! reverse edges: every artifact depending directly or transitively on a
//! changed id joins `stale` and is returned to the caller.

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use serde_json::{json, Map, Value};

use crate::claims;
use crate::err::EngineError;
use crate::store::Wiki;
use crate::txn::Txn;

/// deps.py:10 — DEPS_FILE.
pub const DEPS_FILE: &str = ".state/dependencies.json";

/// review.py:15 — review queue, scanned for `review_` dep liveness.
pub const REVIEW_QUEUE: &str = ".state/review-queue.jsonl";

/// The registry file's JSON shape: `{"edges": {artifact: [dep]}, "stale": []}`.
pub type Graph = Value;

fn empty_graph() -> Graph {
    json!({"edges": {}, "stale": []})
}

/// `deps.graph` (deps.py:35-36) — `wiki.load_json(DEPS_FILE)`. A missing
/// file yields the empty registry shape instead of Python's IOError so
/// first-run callers can read before anything registered.
pub fn graph(wiki: &Wiki) -> Result<Graph, EngineError> {
    Ok(wiki.load_json_opt(DEPS_FILE)?.unwrap_or_else(empty_graph))
}

fn edges_of(data: &Value) -> Map<String, Value> {
    data["edges"].as_object().cloned().unwrap_or_default()
}


/// stage_state closures index `data` by key; a corrupt non-object file
/// (Python would raise) is reset to `{}` rather than panicking.
fn ensure_object(data: &mut Value) {
    if !data.is_object() {
        *data = json!({});
    }
}

fn stale_of(data: &Value) -> HashSet<String> {
    data["stale"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

/// `dep.split("@", 1)[0]` — bare id of a possibly-versioned dep string.
fn dep_id(dep: &Value) -> &str {
    dep.as_str().unwrap_or("").split('@').next().unwrap_or("")
}

/// `deps.register` (deps.py:13-23) — merge `dep_ids` into
/// `edges[artifact]`: existing entries first, new deps appended in order,
/// duplicates skipped.
pub fn register(txn: &mut Txn, artifact: &str, dep_ids: &[&str]) -> Result<(), EngineError> {
    let artifact = artifact.to_string();
    let incoming: Vec<String> = dep_ids.iter().map(|s| s.to_string()).collect();
    txn.stage_state_with(DEPS_FILE, move |data| {
        ensure_object(data);
        if !data["edges"].is_object() {
            data["edges"] = json!({});
        }
        let edges = data["edges"].as_object_mut().unwrap();
        let mut merged: Vec<Value> = edges
            .get(&artifact)
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        for d in &incoming {
            if !merged.iter().any(|m| m.as_str() == Some(d.as_str())) {
                merged.push(json!(d));
            }
        }
        edges.insert(artifact, json!(merged));
    })?;
    Ok(())
}

/// `deps.replace` (deps.py:26-32) — set `edges[artifact]` to `dep_ids`
/// exactly (replaces, never merges).
pub fn replace(txn: &mut Txn, artifact: &str, dep_ids: &[&str]) -> Result<(), EngineError> {
    let artifact = artifact.to_string();
    let exact: Vec<String> = dep_ids.iter().map(|s| s.to_string()).collect();
    txn.stage_state_with(DEPS_FILE, move |data| {
        ensure_object(data);
        if !data["edges"].is_object() {
            data["edges"] = json!({});
        }
        data["edges"].as_object_mut().unwrap().insert(artifact, json!(exact));
    })?;
    Ok(())
}

/// `deps.invalidate` (deps.py:39-86) — mark artifacts depending (directly
/// or transitively) on `changed_ids` stale.
///
/// Reads the committed graph plus the caller txn's staged overlay for
/// DEPS_FILE (deps.py:50-55), seeds the frontier with artifacts depending
/// on a changed id, then walks reverse edges: an artifact is invalidated
/// when one of its dep ids names an already-stale artifact. The merged
/// stale set is staged into the caller's txn; the sorted list of *newly*
/// stale artifacts is returned for caller handling (claims/decisions that
/// turn up here are the caller's to process — only the stale list is
/// recorded).
pub fn invalidate(
    wiki: &Wiki,
    txn: &mut Txn,
    changed_ids: &[&str],
) -> Result<Vec<String>, EngineError> {
    let g = graph(wiki)?;
    let mut edges = edges_of(&g);
    let mut stale = stale_of(&g);
    // include staged-but-uncommitted edge changes from the caller's txn
    // (deps.py:50-55 — txn._state_overlay read-through).
    if let Some(overlay) = txn.staged_value(DEPS_FILE) {
        if overlay["edges"].is_object() {
            edges = edges_of(overlay);
        }
        if overlay["stale"].is_array() {
            stale = stale_of(overlay);
        }
    }

    let changed: HashSet<&str> = changed_ids.iter().copied().collect();
    let mut frontier: Vec<String> = edges
        .iter()
        .filter(|(_, ds)| {
            ds.as_array().map_or(false, |arr| {
                arr.iter().any(|d| changed.contains(dep_id(d)))
            })
        })
        .map(|(a, _)| a.clone())
        .collect();

    let mut newly: Vec<String> = Vec::new();
    while let Some(artifact) = frontier.pop() {
        if stale.contains(&artifact) {
            continue;
        }
        stale.insert(artifact.clone());
        newly.push(artifact.clone());
        for (other, ds) in &edges {
            if !stale.contains(other)
                && ds.as_array().map_or(false, |arr| {
                    arr.iter().any(|d| dep_id(d) == artifact)
                })
            {
                frontier.push(other.clone());
            }
        }
    }

    if !newly.is_empty() {
        let mut sorted_stale: Vec<String> = stale.into_iter().collect();
        sorted_stale.sort();
        txn.stage_state_with(DEPS_FILE, move |data| {
            ensure_object(data);
            data["stale"] = json!(sorted_stale);
        })?;
    }
    newly.sort();
    Ok(newly)
}

/// `deps.is_stale` (deps.py:89-90).
pub fn is_stale(wiki: &Wiki, artifact: &str) -> Result<bool, EngineError> {
    Ok(stale_of(&graph(wiki)?).contains(artifact))
}

/// `deps.stale_artifacts` (deps.py:93-94) — stored order preserved.
pub fn stale_artifacts(wiki: &Wiki) -> Result<Vec<String>, EngineError> {
    Ok(graph(wiki)?["stale"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default())
}

/// `deps.clear_stale` (deps.py:97-101).
pub fn clear_stale(txn: &mut Txn, artifacts: &[&str]) -> Result<(), EngineError> {
    let drop: HashSet<String> = artifacts.iter().map(|s| s.to_string()).collect();
    txn.stage_state_with(DEPS_FILE, move |data| {
        ensure_object(data);
        // keep non-string entries verbatim; drop matching strings
        let kept: Vec<Value> = data["stale"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter(|v| !v.as_str().map_or(false, |s| drop.contains(s)))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        data["stale"] = json!(kept);
    })?;
    Ok(())
}

/// `deps.mark_stale` (deps.py:104-113) — mark specific derived artifacts
/// stale without dependency lookup.
pub fn mark_stale(txn: &mut Txn, artifacts: &[&str]) -> Result<(), EngineError> {
    if artifacts.is_empty() {
        return Ok(());
    }
    let add: Vec<String> = artifacts.iter().map(|s| s.to_string()).collect();
    txn.stage_state_with(DEPS_FILE, move |data| {
        ensure_object(data);
        if !data["stale"].is_array() {
            data["stale"] = json!([]);
        }
        let stale = data["stale"].as_array_mut().unwrap();
        for a in &add {
            if !stale.iter().any(|v| v.as_str() == Some(a.as_str())) {
                stale.push(json!(a));
            }
        }
    })?;
    Ok(())
}

/// Live dep-id set (dream.py:63-64 + 182-197 port): canonical claim and
/// decision ids, plus the resolved `wiki/` and `review_` namespaces so
/// `is_live_dep` needs no filesystem access.
///
///   - claims: `claims.list_claims` (canonical scan/index fallback)
///   - decisions: canonical `decisions/decision_*.json` scan
///     (claims.py:_scan_decisions)
///   - `wiki/` deps: live when an edge key OR an on-disk file under
///     `wiki/` exists (dream.py:185-187)
///   - `review_` deps: live when the id appears in review-queue.jsonl
///     (dream.py:189-191)
pub fn live_ids(wiki: &Wiki) -> Result<HashSet<String>, EngineError> {
    let mut live = HashSet::new();
    for c in claims::list(wiki)? {
        if let Some(id) = c["id"].as_str() {
            live.insert(id.to_string());
        }
    }

    let ddir = wiki.p("decisions");
    if let Ok(rd) = fs::read_dir(&ddir) {
        for entry in rd.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if !(name.starts_with("decision_") && name.ends_with(".json")) {
                continue;
            }
            if let Ok(v) = fs::read_to_string(entry.path())
                .and_then(|t| serde_json::from_str::<Value>(&t).map_err(|e| e.into()))
            {
                if let Some(id) = v["id"].as_str() {
                    live.insert(id.to_string());
                }
            }
        }
    }

    let g = graph(wiki)?;
    for key in edges_of(&g).keys() {
        if key.starts_with("wiki/") {
            live.insert(key.clone());
        }
    }
    collect_wiki_files(&wiki.p("wiki"), &wiki.p("wiki"), &mut live);

    if let Ok(text) = fs::read_to_string(wiki.p(REVIEW_QUEUE)) {
        for line in text.lines().map(str::trim).filter(|l| !l.is_empty()) {
            if let Ok(v) = serde_json::from_str::<Value>(line) {
                if let Some(id) = v["id"].as_str() {
                    live.insert(id.to_string());
                }
            }
        }
    }
    Ok(live)
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

/// Dead-dep detection (dream.py:182-197 / spec §8 ghost rule): strip the
/// `@version` suffix and check membership in the resolved live set.
/// `wiki/` and `review_` ids resolve through `live_ids`'s edge-key,
/// file-existence, and queue-item rules. An unparseable dep (empty id)
/// counts as LIVE — conservative, never drops on a malformed string.
pub fn is_live_dep(dep: &str, live: &HashSet<String>) -> bool {
    let id = dep.split('@').next().unwrap_or("");
    if id.is_empty() {
        return true;
    }
    live.contains(id)
}
