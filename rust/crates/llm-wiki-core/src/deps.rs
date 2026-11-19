//! Dependency graph between canonical knowledge and derived artifacts —
//! port of wikicore/deps.py. Registry file `.state/dependencies.json`,
//! shape `{"edges": {artifact: [dep]}, "stale": [artifact]}`.
//!
//! Deps are `<id>@<version>`; invalidation matches ANY version of a changed
//! id (deps.py:5-6 — bare-id comparison). Staleness propagates through
//! reverse edges: an artifact goes stale when one of its dep ids names an
//! already-stale artifact (deps.py:69).
//!
//! Mutations stage through the crate's [`crate::txn::Txn`]
//! (`stage_state_with(DEPS_FILE, …)` — Python's
//! `txn.stage_state(DEPS_FILE, _upd)`), so they commit atomically with the
//! caller's other changes. The graph algorithms themselves are exposed as
//! pure functions over the registry [`Value`].

use serde_json::{Map, Value};
use std::collections::{BTreeSet, HashSet};

use crate::err::EngineError;
use crate::store::Wiki;
use crate::txn::Txn;

/// deps.py:10 — DEPS_FILE.
pub const DEPS_FILE: &str = ".state/dependencies.json";

/// `dep.split("@", 1)[0]` — bare id of a possibly-versioned dep string.
/// A non-string dep entry degrades to `""` (never matches a real id),
/// mirroring the defensive engine port; Python would raise there.
pub fn dep_id(dep: &Value) -> &str {
    dep.as_str().unwrap_or("").split('@').next().unwrap_or("")
}

pub fn edges_of(data: &Value) -> Map<String, Value> {
    data["edges"].as_object().cloned().unwrap_or_default()
}

pub fn stale_of(data: &Value) -> BTreeSet<String> {
    data["stale"].as_array().map_or_else(BTreeSet::new, |a| {
        a.iter()
            .filter_map(|v| v.as_str().map(String::from))
            .collect()
    })
}

/// The empty registry shape written by `init` (store.py init_wiki).
pub fn empty_graph() -> Value {
    serde_json::json!({"edges": {}, "stale": []})
}

/// deps.py:13-23 `register` — merge `deps_list` into `edges[artifact]`:
/// existing entries first, new deps appended in order, duplicates skipped.
/// Used as a `stage_state_with` closure body.
pub fn merge_edges(data: &mut Value, artifact: &str, deps_list: &[String]) {
    ensure_object(data);
    if !data["edges"].is_object() {
        data["edges"] = serde_json::json!({});
    }
    let edges = data["edges"].as_object_mut().expect("just ensured object");
    let mut merged: Vec<Value> = edges
        .get(artifact)
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();
    for d in deps_list {
        if !merged.iter().any(|m| m.as_str() == Some(d.as_str())) {
            merged.push(Value::String(d.clone()));
        }
    }
    edges.insert(artifact.to_string(), Value::Array(merged));
}

/// deps.py:26-32 `replace` — set `edges[artifact]` to `deps_list` exactly
/// (replaces, never merges; `register` is the merging legacy path).
pub fn set_edges(data: &mut Value, artifact: &str, deps_list: &[String]) {
    ensure_object(data);
    if !data["edges"].is_object() {
        data["edges"] = serde_json::json!({});
    }
    data["edges"]
        .as_object_mut()
        .expect("just ensured object")
        .insert(
            artifact.to_string(),
            Value::Array(deps_list.iter().map(|d| Value::String(d.clone())).collect()),
        );
}

/// deps.py:74-77 / 81-84 — persist the merged stale set, sorted.
pub fn set_stale_sorted(data: &mut Value, stale: BTreeSet<String>) {
    ensure_object(data);
    let sorted: Vec<Value> = stale.into_iter().map(Value::String).collect();
    data["stale"] = Value::Array(sorted);
}

/// deps.py:104-113 `mark_stale` — append artifacts absent from `stale`.
/// Python's empty-list early return happens in the wrapper.
pub fn add_stale(data: &mut Value, artifacts: &[String]) {
    ensure_object(data);
    if !data["stale"].is_array() {
        data["stale"] = serde_json::json!([]);
    }
    let stale = data["stale"].as_array_mut().expect("just ensured array");
    for a in artifacts {
        if !stale.iter().any(|v| v.as_str() == Some(a.as_str())) {
            stale.push(Value::String(a.clone()));
        }
    }
}

/// deps.py:97-101 `clear_stale` — drop the named artifacts, keeping
/// non-string entries verbatim.
pub fn remove_stale(data: &mut Value, artifacts: &[String]) {
    ensure_object(data);
    let drop: HashSet<&str> = artifacts.iter().map(String::as_str).collect();
    let kept: Vec<Value> = data["stale"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter(|v| !v.as_str().is_some_and(|s| drop.contains(s)))
                .cloned()
                .collect()
        })
        .unwrap_or_default();
    data["stale"] = Value::Array(kept);
}

/// deps.py:39-86 `invalidate` — transitive stale-marking over the merged
/// graph view. `edges` and `stale` are the CALLER's merged view (committed
/// graph read-through the txn's staged DEPS_FILE overlay when present: the
/// overlay replaces the committed view wholesale, deps.py:50-55 — it does
/// not merge). Returns `(newly_stale_sorted, full_stale_sorted)`; the
/// wrapper stages `set_stale_sorted` only when `newly` is non-empty
/// (deps.py:72 — no write when nothing changed).
pub fn compute_invalidation(
    edges: &Map<String, Value>,
    stale: &BTreeSet<String>,
    changed_ids: &[&str],
) -> (Vec<String>, BTreeSet<String>) {
    let changed: HashSet<&str> = changed_ids.iter().copied().collect();
    let mut stale = stale.clone();
    // Seed: artifacts depending directly on a changed id (deps.py:60).
    let mut frontier: Vec<String> = edges
        .iter()
        .filter(|(_, ds)| {
            ds.as_array()
                .is_some_and(|arr| arr.iter().any(|d| changed.contains(dep_id(d))))
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
        // Propagate over reverse edges (deps.py:68-70): an artifact joins
        // the frontier when one of its dep ids names an artifact that just
        // went stale. Duplicate pushes are absorbed by the `stale` check.
        for (other, ds) in edges {
            if !stale.contains(other)
                && ds
                    .as_array()
                    .is_some_and(|arr| arr.iter().any(|d| dep_id(d) == artifact))
            {
                frontier.push(other.clone());
            }
        }
    }
    newly.sort();
    (newly, stale)
}

/// A corrupt non-object registry file (Python's `data.setdefault` would
/// raise AttributeError) is reset to `{}` rather than panicking — the
/// engine-port convention for stage_state closures.
fn ensure_object(data: &mut Value) {
    if !data.is_object() {
        *data = serde_json::json!({});
    }
}

// ---- committed-graph reads ----------------------------------------------

/// `deps.graph` (deps.py:35-36) — `wiki.load_json(DEPS_FILE)`. A missing
/// file yields the empty registry shape instead of Python's IOError so
/// first-run callers can read before anything registered (init_wiki writes
/// the file anyway).
pub fn graph(wiki: &Wiki) -> Result<Value, EngineError> {
    Ok(wiki.load_json_opt(DEPS_FILE)?.unwrap_or_else(empty_graph))
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

// ---- txn staging wrappers (deps.py API surface) --------------------------

/// `deps.register` (deps.py:13-23) — merge `dep_ids` into
/// `edges[artifact]`: existing entries first, new deps appended in order,
/// duplicates skipped.
pub fn register(txn: &mut Txn, artifact: &str, dep_ids: &[&str]) -> Result<(), EngineError> {
    let artifact = artifact.to_string();
    let incoming: Vec<String> = dep_ids.iter().map(|s| s.to_string()).collect();
    txn.stage_state_with(DEPS_FILE, move |data| {
        merge_edges(data, &artifact, &incoming)
    })?;
    Ok(())
}

/// `deps.replace` (deps.py:26-32) — set `edges[artifact]` to `dep_ids`
/// exactly (replaces, never merges; `register` is the merging legacy path).
pub fn replace(txn: &mut Txn, artifact: &str, dep_ids: &[&str]) -> Result<(), EngineError> {
    let artifact = artifact.to_string();
    let exact: Vec<String> = dep_ids.iter().map(|s| s.to_string()).collect();
    txn.stage_state_with(DEPS_FILE, move |data| set_edges(data, &artifact, &exact))?;
    Ok(())
}

/// `deps.clear_stale` (deps.py:97-101).
pub fn clear_stale(txn: &mut Txn, artifacts: &[&str]) -> Result<(), EngineError> {
    let drop: Vec<String> = artifacts.iter().map(|s| s.to_string()).collect();
    txn.stage_state_with(DEPS_FILE, move |data| remove_stale(data, &drop))?;
    Ok(())
}

/// `deps.mark_stale` (deps.py:104-113) — mark specific derived artifacts
/// stale without dependency lookup. Empty list stages nothing (early
/// return parity with Python).
pub fn mark_stale(txn: &mut Txn, artifacts: &[&str]) -> Result<(), EngineError> {
    if artifacts.is_empty() {
        return Ok(());
    }
    let add: Vec<String> = artifacts.iter().map(|s| s.to_string()).collect();
    txn.stage_state_with(DEPS_FILE, move |data| add_stale(data, &add))?;
    Ok(())
}

/// `deps.invalidate` (deps.py:39-86) — mark artifacts depending (directly
/// or transitively) on `changed_ids` stale; returns the sorted list of
/// newly stale artifacts.
///
/// The graph view is the committed registry read through the caller txn's
/// staged `DEPS_FILE` overlay when one exists (deps.py:50-55): the overlay
/// REPLACES the committed view wholesale — it does not merge — matching the
/// Python `overlay.get("edges", edges)` / `overlay.get("stale", stale)`
/// semantics. The merged stale set is staged into the caller's txn only
/// when something turned stale (deps.py:72 `if newly:`).
pub fn invalidate(
    wiki: &Wiki,
    txn: &mut Txn,
    changed_ids: &[&str],
) -> Result<Vec<String>, EngineError> {
    let g = graph(wiki)?;
    let mut edges = edges_of(&g);
    let mut stale = stale_of(&g);
    if let Some(overlay) = txn.staged_value(DEPS_FILE) {
        if overlay.get("edges").is_some() {
            edges = edges_of(overlay);
        }
        if overlay.get("stale").is_some() {
            stale = stale_of(overlay);
        }
    }
    let (newly, full_stale) = compute_invalidation(&edges, &stale, changed_ids);
    if !newly.is_empty() {
        txn.stage_state_with(DEPS_FILE, move |data| set_stale_sorted(data, full_stale))?;
    }
    Ok(newly)
}

/// deps.py:78-86 — the `txn=None` convenience path: a wiki-internal
/// transaction ("wiki-internal-invalidate") stages and commits the merged
/// stale set on its own.
pub fn invalidate_standalone(
    wiki: &Wiki,
    changed_ids: &[&str],
) -> Result<Vec<String>, EngineError> {
    let mut txn = Txn::begin(wiki, &crate::ids::new("run"))?;
    txn.command = "wiki-internal-invalidate".to_string();
    // No staged overlay of our own: compute over the committed graph.
    let g = graph(wiki)?;
    let (newly, full_stale) = compute_invalidation(&edges_of(&g), &stale_of(&g), changed_ids);
    if !newly.is_empty() {
        txn.stage_state_with(DEPS_FILE, move |data| set_stale_sorted(data, full_stale))?;
        txn.commit()?;
    }
    Ok(newly)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn edges(pairs: &[(&str, &[&str])]) -> Map<String, Value> {
        let mut m = Map::new();
        for (k, ds) in pairs {
            m.insert(
                (*k).to_string(),
                Value::Array(ds.iter().map(|d| json!(d)).collect()),
            );
        }
        m
    }

    #[test]
    fn direct_invalidation_only() {
        // s1
        let e = edges(&[("wiki/a.md", &["claim_1@1"]), ("wiki/b.md", &["claim_2@1"])]);
        let (newly, stale) = compute_invalidation(&e, &BTreeSet::new(), &["claim_1"]);
        assert_eq!(newly, ["wiki/a.md"]);
        assert_eq!(stale.iter().cloned().collect::<Vec<_>>(), ["wiki/a.md"]);
    }

    #[test]
    fn transitive_chain() {
        // s2 — wiki/c.md depends on wiki/a.md@2 which depends on claim_1@1.
        let e = edges(&[
            ("wiki/c.md", &["wiki/a.md@2"]),
            ("wiki/a.md", &["claim_1@1"]),
        ]);
        let (newly, stale) = compute_invalidation(&e, &BTreeSet::new(), &["claim_1"]);
        assert_eq!(newly, ["wiki/a.md", "wiki/c.md"]);
        assert_eq!(
            stale.iter().cloned().collect::<Vec<_>>(),
            ["wiki/a.md", "wiki/c.md"]
        );
    }

    #[test]
    fn version_suffix_ignored() {
        // s3 — ANY version of a changed id matches.
        let e = edges(&[("wiki/a.md", &["claim_1@999"])]);
        let (newly, _) = compute_invalidation(&e, &BTreeSet::new(), &["claim_1"]);
        assert_eq!(newly, ["wiki/a.md"]);
    }

    #[test]
    fn already_stale_not_rereported() {
        // s4 — wiki/b.md already stale; only wiki/a.md is newly stale, but
        // the persisted set keeps both.
        let e = edges(&[
            ("wiki/a.md", &["claim_1@1"]),
            ("wiki/b.md", &["wiki/a.md@1"]),
        ]);
        let stale0: BTreeSet<String> = ["wiki/b.md".to_string()].into_iter().collect();
        let (newly, stale) = compute_invalidation(&e, &stale0, &["claim_1"]);
        assert_eq!(newly, ["wiki/a.md"]);
        assert_eq!(
            stale.iter().cloned().collect::<Vec<_>>(),
            ["wiki/a.md", "wiki/b.md"]
        );
    }

    #[test]
    fn cycles_terminate() {
        // s5 — a↔b cycle never entered (no changed seed), c invalidated.
        let e = edges(&[
            ("wiki/a.md", &["wiki/b.md@1"]),
            ("wiki/b.md", &["wiki/a.md@1"]),
            ("wiki/c.md", &["claim_9@1"]),
        ]);
        let (newly, stale) = compute_invalidation(&e, &BTreeSet::new(), &["claim_9"]);
        assert_eq!(newly, ["wiki/c.md"]);
        assert_eq!(stale.iter().cloned().collect::<Vec<_>>(), ["wiki/c.md"]);
    }

    #[test]
    fn diamond_deduplicates() {
        // s6 — top depends on both l and r; each depends on claim_1.
        let e = edges(&[
            ("wiki/top.md", &["wiki/l.md@1", "wiki/r.md@1"]),
            ("wiki/l.md", &["claim_1@1"]),
            ("wiki/r.md", &["claim_1@1"]),
        ]);
        let (newly, _) = compute_invalidation(&e, &BTreeSet::new(), &["claim_1"]);
        assert_eq!(newly, ["wiki/l.md", "wiki/r.md", "wiki/top.md"]);
    }

    #[test]
    fn stale_downstream_suppressed() {
        // s7 — c already stale; a, b newly stale; persisted keeps c.
        let e = edges(&[
            ("wiki/a.md", &["claim_1@1"]),
            ("wiki/b.md", &["wiki/a.md@1"]),
            ("wiki/c.md", &["wiki/b.md@1"]),
        ]);
        let stale0: BTreeSet<String> = ["wiki/c.md".to_string()].into_iter().collect();
        let (newly, stale) = compute_invalidation(&e, &stale0, &["claim_1"]);
        assert_eq!(newly, ["wiki/a.md", "wiki/b.md"]);
        assert_eq!(
            stale.iter().cloned().collect::<Vec<_>>(),
            ["wiki/a.md", "wiki/b.md", "wiki/c.md"]
        );
    }

    #[test]
    fn no_match_nothing_new() {
        // s8 — changed id touches nothing: empty newly (and the wrapper
        // then stages nothing — deps.py:72 `if newly:`).
        let e = edges(&[("wiki/a.md", &["claim_1@1"])]);
        let (newly, stale) = compute_invalidation(&e, &BTreeSet::new(), &["claim_2"]);
        assert!(newly.is_empty());
        assert!(stale.is_empty());
    }

    #[test]
    fn unversioned_and_bare_deps_match() {
        let e = edges(&[("wiki/a.md", &["claim_1"])]);
        let (newly, _) = compute_invalidation(&e, &BTreeSet::new(), &["claim_1"]);
        assert_eq!(newly, ["wiki/a.md"]);
    }

    #[test]
    fn non_string_dep_never_matches() {
        let mut e = Map::new();
        e.insert("wiki/a.md".to_string(), json!([42, null, "claim_1@1"]));
        let (newly, _) = compute_invalidation(&e, &BTreeSet::new(), &["claim_1"]);
        assert_eq!(newly, ["wiki/a.md"]);
        e.insert("wiki/b.md".to_string(), json!([42]));
        let (newly, _) = compute_invalidation(&e, &BTreeSet::new(), &["claim_1"]);
        assert!(!newly.contains(&"wiki/b.md".to_string()));
    }

    #[test]
    fn merge_edges_dedupes_and_appends() {
        // register merge ground truth from deps.py.
        let mut data = json!({"edges": {}, "stale": []});
        merge_edges(
            &mut data,
            "wiki/a.md",
            &["claim_1@1".into(), "claim_2@1".into()],
        );
        merge_edges(
            &mut data,
            "wiki/a.md",
            &["claim_2@1".into(), "claim_3@1".into()],
        );
        assert_eq!(
            data["edges"]["wiki/a.md"],
            json!(["claim_1@1", "claim_2@1", "claim_3@1"])
        );
    }

    #[test]
    fn set_edges_replaces_exactly() {
        let mut data = json!({"edges": {"wiki/a.md": ["claim_1@1", "claim_2@1"]}, "stale": []});
        set_edges(&mut data, "wiki/a.md", &["claim_9@1".into()]);
        assert_eq!(data["edges"]["wiki/a.md"], json!(["claim_9@1"]));
    }

    #[test]
    fn add_and_remove_stale() {
        let mut data = json!({"edges": {}, "stale": []});
        add_stale(&mut data, &["wiki/x.md".into(), "wiki/a.md".into()]);
        assert_eq!(data["stale"], json!(["wiki/x.md", "wiki/a.md"]));
        // empty artifacts list: Python mark_stale returns before staging;
        // the updater itself is a no-op append of nothing.
        add_stale(&mut data, &[]);
        assert_eq!(data["stale"], json!(["wiki/x.md", "wiki/a.md"]));
        add_stale(&mut data, &["wiki/a.md".into()]);
        assert_eq!(data["stale"], json!(["wiki/x.md", "wiki/a.md"]));
        remove_stale(&mut data, &["wiki/x.md".into()]);
        assert_eq!(data["stale"], json!(["wiki/a.md"]));
    }

    #[test]
    fn corrupt_registry_resets() {
        let mut data = json!("not an object");
        merge_edges(&mut data, "wiki/a.md", &["claim_1@1".into()]);
        assert_eq!(data["edges"]["wiki/a.md"], json!(["claim_1@1"]));
    }

    #[test]
    fn dep_id_strips_version() {
        assert_eq!(dep_id(&json!("claim_1@7")), "claim_1");
        assert_eq!(dep_id(&json!("claim_1")), "claim_1");
        assert_eq!(dep_id(&json!("a@b@c")), "a");
        assert_eq!(dep_id(&json!(42)), "");
    }
}

// ---- txn-level tests (register/invalidate/commit round-trip) -------------

#[cfg(test)]
mod txn_tests {
    use super::*;
    use crate::store::init_wiki;
    use serde_json::json;

    fn fresh_wiki() -> (tempfile::TempDir, Wiki) {
        let dir = tempfile::tempdir().unwrap();
        let wiki = Wiki::open(dir.path()).unwrap();
        init_wiki(&wiki, "proj").unwrap();
        (dir, wiki)
    }

    fn committed(wiki: &Wiki) -> Value {
        wiki.load_json(DEPS_FILE).unwrap_or_else(|_| empty_graph())
    }

    #[test]
    fn register_and_replace_commit_atomically() {
        let (_d, wiki) = fresh_wiki();
        let mut txn = Txn::begin(&wiki, "run1").unwrap();
        register(&mut txn, "wiki/a.md", &["claim_1@1", "claim_2@1"]).unwrap();
        register(&mut txn, "wiki/a.md", &["claim_2@1", "claim_3@1"]).unwrap();
        // visible through the txn overlay before commit (read-your-writes)
        let overlay = txn.staged_value(DEPS_FILE).unwrap();
        assert_eq!(
            overlay["edges"]["wiki/a.md"],
            json!(["claim_1@1", "claim_2@1", "claim_3@1"])
        );
        txn.commit().unwrap();
        let g = committed(&wiki);
        assert_eq!(
            g["edges"]["wiki/a.md"],
            json!(["claim_1@1", "claim_2@1", "claim_3@1"])
        );
        // replace: exact swap, committed on revision 1
        let mut txn = Txn::begin(&wiki, "run2").unwrap();
        replace(&mut txn, "wiki/a.md", &["claim_9@1"]).unwrap();
        txn.commit().unwrap();
        assert_eq!(committed(&wiki)["edges"]["wiki/a.md"], json!(["claim_9@1"]));
    }

    #[test]
    fn invalidate_commits_through_caller_txn() {
        let (_d, wiki) = fresh_wiki();
        let mut txn = Txn::begin(&wiki, "run1").unwrap();
        register(&mut txn, "wiki/a.md", &["claim_1@1"]).unwrap();
        register(&mut txn, "wiki/b.md", &["wiki/a.md@1"]).unwrap();
        register(&mut txn, "wiki/c.md", &["wiki/b.md@1"]).unwrap();
        txn.commit().unwrap();

        let mut txn = Txn::begin(&wiki, "run2").unwrap();
        let newly = invalidate(&wiki, &mut txn, &["claim_1"]).unwrap();
        assert_eq!(newly, ["wiki/a.md", "wiki/b.md", "wiki/c.md"]);
        txn.commit().unwrap();
        let g = committed(&wiki);
        assert_eq!(g["stale"], json!(["wiki/a.md", "wiki/b.md", "wiki/c.md"]));
        // second run: nothing newly stale, nothing staged
        let mut txn = Txn::begin(&wiki, "run3").unwrap();
        let newly = invalidate(&wiki, &mut txn, &["claim_1"]).unwrap();
        assert!(newly.is_empty());
        txn.commit().unwrap();
        assert_eq!(
            committed(&wiki)["stale"],
            json!(["wiki/a.md", "wiki/b.md", "wiki/c.md"])
        );
    }

    #[test]
    fn invalidate_reads_the_staged_view() {
        // deps.py:50-55 — invalidate sees the txn's staged DEPS_FILE value
        // (deps.py's `txn._state_overlay` read-through). stage_state_with
        // seeds from the committed file, so the staged view accumulates:
        // replace() swapped old.md for new.md, and the uncommitted state
        // already drives invalidation.
        let (_d, wiki) = fresh_wiki();
        let mut txn = Txn::begin(&wiki, "run1").unwrap();
        register(&mut txn, "wiki/old.md", &["claim_1@1"]).unwrap();
        txn.commit().unwrap();

        let mut txn = Txn::begin(&wiki, "run2").unwrap();
        replace(&mut txn, "wiki/new.md", &["claim_1@1"]).unwrap(); // staged, uncommitted
        let newly = invalidate(&wiki, &mut txn, &["claim_1"]).unwrap();
        assert_eq!(newly, ["wiki/new.md", "wiki/old.md"]);
        txn.commit().unwrap();
        let g = committed(&wiki);
        assert_eq!(g["stale"], json!(["wiki/new.md", "wiki/old.md"]));
        assert_eq!(g["edges"]["wiki/new.md"], json!(["claim_1@1"]));
    }

    #[test]
    fn standalone_invalidate_commits_internally() {
        let (_d, wiki) = fresh_wiki();
        let mut txn = Txn::begin(&wiki, "run1").unwrap();
        register(&mut txn, "wiki/a.md", &["claim_1@1"]).unwrap();
        txn.commit().unwrap();
        let newly = invalidate_standalone(&wiki, &["claim_1"]).unwrap();
        assert_eq!(newly, ["wiki/a.md"]);
        assert_eq!(committed(&wiki)["stale"], json!(["wiki/a.md"]));
        assert_eq!(wiki.revision().unwrap(), 2); // register + invalidate commits
    }

    #[test]
    fn clear_and_mark_stale_round_trip() {
        let (_d, wiki) = fresh_wiki();
        let mut txn = Txn::begin(&wiki, "run1").unwrap();
        mark_stale(&mut txn, &["wiki/x.md", "wiki/y.md"]).unwrap();
        txn.commit().unwrap();
        assert_eq!(committed(&wiki)["stale"], json!(["wiki/x.md", "wiki/y.md"]));
        assert!(is_stale(&wiki, "wiki/x.md").unwrap());
        assert_eq!(stale_artifacts(&wiki).unwrap(), ["wiki/x.md", "wiki/y.md"]);
        // empty mark_stale stages no dependencies.json write; the empty
        // commit still bumps the manifest revision (Python parity —
        // transaction.py:275 records before→after unconditionally).
        let mut txn = Txn::begin(&wiki, "run2").unwrap();
        mark_stale(&mut txn, &[]).unwrap();
        txn.commit().unwrap();
        assert_eq!(wiki.revision().unwrap(), 2);
        let mut txn = Txn::begin(&wiki, "run3").unwrap();
        clear_stale(&mut txn, &["wiki/x.md"]).unwrap();
        txn.commit().unwrap();
        assert_eq!(committed(&wiki)["stale"], json!(["wiki/y.md"]));
    }

    #[test]
    fn missing_registry_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let wiki = Wiki::open(dir.path()).unwrap();
        init_wiki(&wiki, "proj").unwrap();
        let g = graph(&wiki).unwrap();
        assert_eq!(g, empty_graph());
        assert!(!is_stale(&wiki, "wiki/a.md").unwrap());
        assert!(stale_artifacts(&wiki).unwrap().is_empty());
    }
}
