mod common;

use std::process::Command;

use serde_json::{json, Value};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_llm-wiki"))
}

fn run_json(args: &[&str]) -> (i32, Value) {
    let out = bin().args(args).output().unwrap();
    let code = out.status.code().unwrap_or(-1);
    let v: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout not JSON ({e}): {}", String::from_utf8_lossy(&out.stdout)));
    (code, v)
}

fn seed_wiki(root: &std::path::Path) {
    // one claim citing a source
    std::fs::write(
        root.join(".llm-wiki/claims/claim_c1.json"),
        serde_json::to_string(&json!({
            "id": "claim_c1",
            "subject": "t.api",
            "predicate": "status",
            "value": "stable",
            "status": "accepted",
            "version": 1,
            "evidence": [{"source_id": "source_s1", "version": 1}],
        }))
        .unwrap(),
    )
    .unwrap();
    // source manifest
    std::fs::create_dir_all(root.join(".llm-wiki/sources/source_s1")).unwrap();
    std::fs::write(
        root.join(".llm-wiki/sources/source_s1/manifest.json"),
        serde_json::to_string(&json!({
            "id": "source_s1",
            "title": "Spec doc",
            "origin": "docs/spec.md",
        }))
        .unwrap(),
    )
    .unwrap();
    // decision resolving the claim
    std::fs::write(
        root.join(".llm-wiki/decisions/decision_d1.json"),
        serde_json::to_string(&json!({
            "id": "decision_d1",
            "title": "Keep api",
            "status": "accepted",
            "decided_on": "2026-01-01",
            "claims": ["claim_c1"],
        }))
        .unwrap(),
    )
    .unwrap();
    // a derived page depending on the claim → documents edge
    let pages = root.join(".llm-wiki/wiki/concepts");
    std::fs::create_dir_all(&pages).unwrap();
    std::fs::write(
        pages.join("api.md"),
        "---\ntitle: API notes\nstale: false\n---\nnotes body\n",
    )
    .unwrap();
    std::fs::write(
        root.join(".llm-wiki/.state/dependencies.json"),
        serde_json::to_string(&json!({
            "edges": {"wiki/concepts/api.md": ["claim_c1@1"]},
            "stale": [],
        }))
        .unwrap(),
    )
    .unwrap();
}

#[test]
fn graph_data_node_and_link_shapes() {
    let (dir, _wiki) = common::wiki();
    let root = dir.path();
    seed_wiki(root);
    let (code, v) = run_json(&["--root", root.to_str().unwrap(), "graph-data"]);
    assert_eq!(code, 0, "{v}");
    for key in [
        "project", "nodes", "links", "claim_count", "source_count",
        "decision_count", "page_count", "generated_at", "meta", "warnings",
    ] {
        assert!(v.get(key).is_some(), "missing key {key}: {v}");
    }
    assert_eq!(v["claim_count"], 1);
    assert_eq!(v["source_count"], 1);
    assert_eq!(v["decision_count"], 1);
    assert_eq!(v["page_count"], 1);

    let nodes = v["nodes"].as_array().unwrap();
    let kinds: Vec<&str> = nodes.iter().map(|n| n["kind"].as_str().unwrap()).collect();
    assert_eq!(kinds, vec!["entity", "source", "decision", "page"], "{nodes:?}");

    let entity = &nodes[0];
    assert_eq!(entity["key"], "t.api");
    assert_eq!(entity["label"], "api");
    assert_eq!(entity["claim_count"], 1);
    assert_eq!(entity["claim_ids"], json!(["claim_c1"]));
    assert_eq!(entity["sources"], json!(["source_s1"]));
    // summary mode: no embedded claim dicts
    assert!(entity.get("claims").is_none(), "{entity}");
    // every node carries flags
    for n in nodes {
        for f in ["review_open", "disputed", "superseded", "stale", "orphan"] {
            assert!(n["flags"].get(f).is_some(), "missing flag {f} in {n}");
        }
    }

    let links: Vec<(&str, &str)> = v["links"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| {
            let kind = |i: u64| nodes[i as usize]["kind"].as_str().unwrap();
            (l["type"].as_str().unwrap(), kind(l["target"].as_u64().unwrap()))
        })
        .collect();
    // entity → source evidence edge
    assert!(links.contains(&("evidence", "source")), "{links:?}");
    // decision → entity decision edge; entity → page documents edge
    assert!(links.contains(&("decision", "entity")), "{links:?}");
    assert!(links.contains(&("documents", "page")), "{links:?}");
}

#[test]
fn graph_data_full_embeds_claims() {
    let (dir, _wiki) = common::wiki();
    let root = dir.path();
    seed_wiki(root);
    let (code, v) = run_json(&["--root", root.to_str().unwrap(), "graph-data", "--full"]);
    assert_eq!(code, 0, "{v}");
    let entity = &v["nodes"][0];
    assert_eq!(entity["claims"][0]["id"], "claim_c1");
    assert_eq!(entity["claims"][0]["subject"], "t.api");
}

#[test]
fn graph_data_output_writes_file() {
    let (dir, _wiki) = common::wiki();
    let root = dir.path();
    seed_wiki(root);
    let out_path = dir.path().join("graph.json");
    let (code, v) = run_json(&[
        "--root", root.to_str().unwrap(),
        "graph-data", "--output", out_path.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["written"], json!(out_path.to_str().unwrap()));
    assert_eq!(v["nodes"], 4);
    assert!(v["links"].as_i64().unwrap() >= 3);
    let written: Value =
        serde_json::from_str(&std::fs::read_to_string(&out_path).unwrap()).unwrap();
    assert_eq!(written["claim_count"], 1);
    assert_eq!(written["nodes"].as_array().unwrap().len(), 4);
}

#[test]
fn graph_data_uninitialized_wiki_txn() {
    let dir = tempfile::tempdir().unwrap();
    let (code, v) = run_json(&["--root", dir.path().to_str().unwrap(), "graph-data"]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["error_type"], "txn");
    assert!(v["error"].as_str().unwrap().contains("not initialized"), "{v}");
}
