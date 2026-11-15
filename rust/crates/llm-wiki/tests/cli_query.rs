mod common;

use std::path::Path;
use std::process::Command;

use llm_wiki::index::Index;
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

fn write_claim(root: &Path, claim: Value) {
    let id = claim["id"].as_str().unwrap();
    std::fs::write(
        root.join(format!(".llm-wiki/claims/{id}.json")),
        serde_json::to_string(&claim).unwrap(),
    )
    .unwrap();
}

fn claim(id: &str, subject: &str, predicate: &str, value: &str, status: &str) -> Value {
    json!({
        "id": id,
        "subject": subject,
        "predicate": predicate,
        "value": value,
        "status": status,
        "version": 1,
        "recorded_at": "2026-01-15T00:00:00Z",
        "evidence": [{"source_id": "source_s1", "version": 1},
                     {"source_id": "source_s2", "version": 1}],
        "authority": {"type": "authoritative_source"},
        "valid_from": "2025-06-01",
    })
}

/// Bring `.state/index.sqlite` fresh (watermark == manifest revision) so
/// queries run the `bm25` lane.
fn fresh_index(wiki: &llm_wiki::store::Wiki) {
    let rev = wiki.revision().unwrap();
    Index::rebuild_under_index_lock(wiki, rev).unwrap();
}

/// Push the watermark ahead of the manifest — `index_stale` — so queries
/// fall to the canonical-scan `legacy` lane.
fn stale_index(wiki: &llm_wiki::store::Wiki) {
    let idx = Index::open_or_rebuild(wiki).unwrap();
    idx.set_built_at_revision(wiki.revision().unwrap() + 99).unwrap();
}

fn query(root: &Path, question: &str, extra: &[&str]) -> (i32, Value) {
    let mut args: Vec<&str> = vec![
        "--root",
        root.to_str().unwrap(),
        "query-prepare",
        "--question",
        question,
    ];
    args.extend_from_slice(extra);
    run_json(&args)
}

fn match_ids(v: &Value) -> Vec<String> {
    v["matches"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["claim_id"].as_str().unwrap().to_string())
        .collect()
}

fn conflict_pairs(v: &Value) -> Vec<Vec<String>> {
    v["conflicts"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|c| c.get("claims"))
        .map(|p| {
            let mut ids: Vec<String> = p
                .as_array()
                .unwrap()
                .iter()
                .map(|x| x.as_str().unwrap().into())
                .collect();
            ids.sort();
            ids
        })
        .collect()
}

/// Corpus A: contradiction pair on scope us + same-fields twin on eu
/// (NOT a contradiction, PRD §6), a superseded claim + its live
/// replacement, a disputed claim, and one decision.
fn seed_a(wiki: &llm_wiki::store::Wiki) {
    let root = &wiki.root;
    let mut a1 = claim("claim_a1", "api.orders", "uses database", "PostgreSQL", "accepted");
    a1["scope"] = json!({"region": "us"});
    write_claim(root, a1);
    let mut a2 = claim("claim_a2", "api.orders", "uses database", "MongoDB", "accepted");
    a2["scope"] = json!({"region": "us"});
    write_claim(root, a2);
    let mut a3 = claim("claim_a3", "api.orders", "uses database", "MySQL", "accepted");
    a3["scope"] = json!({"region": "eu"});
    write_claim(root, a3);
    // superseded claim on a fully isolated subject: its subject tokens
    // overlap no live claim's subject, so a value-only question reaches
    // ONLY it (the anchor bridge still expands — Python-identical).
    let mut old = claim("claim_old", "zzz.archive", "uses database", "SQLite", "superseded");
    old["superseded_by"] = json!("claim_a1");
    old["updated_at"] = json!("2026-03-01T00:00:00Z");
    write_claim(root, old);
    write_claim(
        root,
        claim("claim_d1", "api.orders", "team owner", "payments team", "disputed"),
    );
    std::fs::create_dir_all(root.join(".llm-wiki/decisions")).unwrap();
    std::fs::write(
        root.join(".llm-wiki/decisions/decision_d1.json"),
        serde_json::to_string(&json!({
            "id": "decision_d1",
            "title": "Postgres over Mongo for orders",
            "status": "accepted",
            "claims": ["claim_a1"],
        }))
        .unwrap(),
    )
    .unwrap();
    fresh_index(wiki);
}

#[test]
fn bm25_lane_matches_order_and_minimal_payload() {
    let (_dir, wiki) = common::wiki();
    seed_a(&wiki);
    let (code, v) = query(&wiki.root, "what database does api orders use", &[]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["ranker"], "bm25", "{v}");
    // minimal contractual set (query.py:377-385) present
    for k in ["question", "as_of", "matches", "superseded", "decisions",
              "conflicts", "gaps", "warnings"] {
        assert!(v.get(k).is_some(), "missing key {k}: {v}");
    }
    let ids = match_ids(&v);
    // a1/a2/a3 tie on identical field docs — deterministic claim_id order.
    assert_eq!(ids[..3], ["claim_a1", "claim_a2", "claim_a3"], "{v}");
    // superseded claim never matches; it surfaces in `superseded`
    assert!(!ids.contains(&"claim_old".to_string()), "{v}");
    let sup = v["superseded"].as_array().unwrap();
    assert_eq!(sup.len(), 1, "{v}");
    assert_eq!(sup[0]["claim_id"], "claim_old");
    assert_eq!(sup[0]["status"], "superseded");
    assert_eq!(sup[0]["replaced_by"], "claim_a1");
    assert_eq!(v["decisions"], json!(["decision_d1"]), "{v}");
    // scope rule: only the same-scope pair conflicts (PRD §6)
    let pairs = conflict_pairs(&v);
    assert!(pairs.contains(&vec!["claim_a1".into(), "claim_a2".into()]), "{v}");
    assert!(!pairs.contains(&vec!["claim_a1".into(), "claim_a3".into()]), "{v}");
    assert!(!pairs.contains(&vec!["claim_a2".into(), "claim_a3".into()]), "{v}");
    // disputed claim in the pool → claim_id conflict entry
    assert!(
        v["conflicts"].as_array().unwrap().iter().any(|c| {
            c.get("claim_id") == Some(&json!("claim_d1"))
                && c["reason"].as_str().unwrap().contains("disputed")
        }),
        "{v}"
    );
    // match entry shape (query.py:267-283)
    let m0 = &v["matches"][0];
    for k in ["claim_id", "subject", "predicate", "value", "unit", "scope",
              "status", "valid_from", "valid_to", "authority", "evidence",
              "score", "score_breakdown"] {
        assert!(m0.get(k).is_some(), "missing match key {k}: {m0}");
    }
    assert_eq!(m0["scope"], json!({"region": "us"}));
}

#[test]
fn stale_knowledge_gap_when_only_superseded_answers() {
    let (_dir, wiki) = common::wiki();
    seed_a(&wiki);
    // "sqlite" is only carried by the superseded claim → matches empty,
    // superseded nonempty → the stale_knowledge gap fires (query.py:376).
    let (code, v) = query(&wiki.root, "sqlite", &[]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["matches"], json!([]), "{v}");
    assert_eq!(v["gaps"], json!(["stale_knowledge"]), "{v}");
    assert_eq!(v["superseded"][0]["claim_id"], "claim_old", "{v}");
}

#[test]
fn ranking_parity_between_lanes() {
    let (_dir, wiki) = common::wiki();
    seed_a(&wiki);
    let q = "what database does api orders use";
    let (_, fresh) = query(&wiki.root, q, &[]);
    assert_eq!(fresh["ranker"], "bm25", "{fresh}");

    // stale watermark → canonical-scan lane, identical ordering
    stale_index(&wiki);
    let (code, stale) = query(&wiki.root, q, &[]);
    assert_eq!(code, 0, "{stale}");
    assert_eq!(stale["ranker"], "legacy", "{stale}");
    assert_eq!(match_ids(&fresh), match_ids(&stale), "ordering parity");
    assert_eq!(fresh["matches"], stale["matches"], "full row parity");
    assert_eq!(fresh["superseded"], stale["superseded"]);
    assert_eq!(fresh["conflicts"], stale["conflicts"]);
    assert_eq!(fresh["gaps"], stale["gaps"]);

    // missing index → legacy lane, still identical
    std::fs::remove_file(wiki.p(".state/index.sqlite")).unwrap();
    let (_, gone) = query(&wiki.root, q, &[]);
    assert_eq!(gone["ranker"], "legacy", "{gone}");
    assert_eq!(match_ids(&fresh), match_ids(&gone), "ordering parity");

    // corrupt index → legacy lane (probe reports -1)
    std::fs::write(wiki.p(".state/index.sqlite"), b"not sqlite").unwrap();
    let (_, corrupt) = query(&wiki.root, q, &[]);
    assert_eq!(corrupt["ranker"], "legacy", "{corrupt}");
    assert_eq!(match_ids(&fresh), match_ids(&corrupt), "ordering parity");
}

#[test]
fn as_of_historical_supersede_window() {
    let (_dir, wiki) = common::wiki();
    seed_a(&wiki);
    let q = "sqlite database";
    // "sqlite database" hits the old claim's value+predicate (it outranks
    // the predicate-only api.orders rows) — before its supersede date it
    // was still live → it appears as a match
    let (code, v) = query(&wiki.root, q, &["--as-of", "2026-02-15"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["as_of"], "2026-02-15");
    assert!(match_ids(&v).contains(&"claim_old".to_string()), "{v}");
    assert_eq!(v["superseded"], json!([]), "{v}");
    // after the supersede date it is excluded entirely
    let (_, v) = query(&wiki.root, q, &["--as-of", "2026-04-01"]);
    assert!(!match_ids(&v).contains(&"claim_old".to_string()), "{v}");
}

#[test]
fn contradiction_window_beyond_display_limit() {
    // A conflicting twin ranked past the 10-deep display window must still
    // surface in conflicts — the widened _FTS_LIMIT=60 pool exists
    // precisely for this (spec §10.4, PRD §34).
    let (_dir, wiki) = common::wiki();
    let root = &wiki.root;
    write_claim(root, claim("claim_twin_a", "pay.service", "database use", "mysql", "accepted"));
    // twin_b shares the subject+predicate+scope but its value differs —
    // a textbook contradiction — and mentions mysql in a longer value so
    // it scores above the 40% precision floor yet below the fillers:
    // inside the 60-deep contradiction pool, outside the top-10 display.
    write_claim(
        root,
        claim(
            "claim_twin_b",
            "pay.service",
            "database use",
            "mariadb mysql",
            "accepted",
        ),
    );
    // 12 fillers identical to twin_a on every scored field, so they tie
    // its score exactly and win the claim_id tie-break (claim_f00 <
    // claim_twin_a); distinct scopes keep filler pairs clean of the
    // contradiction rule (PRD §6).
    for i in 0..12 {
        let mut f = claim(
            &format!("claim_f{i:02}"),
            "pay.service",
            "database use",
            "mysql",
            "accepted",
        );
        f["scope"] = json!({"tag": i});
        write_claim(root, f);
    }
    fresh_index(&wiki);
    let (code, v) = query(&wiki.root, "which database does pay service use mysql", &[]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["ranker"], "bm25", "{v}");
    let matches = v["matches"].as_array().unwrap();
    // display window intact at _MATCH_LIMIT=10 claims — all fillers
    assert_eq!(matches.len(), 10, "{v}");
    let ids = match_ids(&v);
    // both twins ranked below the display window …
    assert!(!ids.contains(&"claim_twin_a".to_string()), "{v}");
    assert!(!ids.contains(&"claim_twin_b".to_string()), "{v}");
    // … yet the contradiction is still surfaced (pool = _FTS_LIMIT=60)
    assert!(
        conflict_pairs(&v)
            .contains(&vec!["claim_twin_a".into(), "claim_twin_b".into()]),
        "{v}"
    );
}

#[test]
fn unsupported_question_still_exits_zero() {
    let (_dir, wiki) = common::wiki();
    seed_a(&wiki);
    let (code, v) = query(&wiki.root, "kubernetes cluster budget", &[]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["matches"], json!([]), "{v}");
    assert_eq!(
        v["gaps"],
        json!(["budget", "cluster", "kubernete"]),
        "{v}"
    );
}

#[test]
fn empty_question_python_identical_payload() {
    let (_dir, wiki) = common::wiki();
    seed_a(&wiki);
    let (code, v) = query(&wiki.root, "", &[]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["question"], "");
    assert_eq!(v["as_of"], Value::Null);
    assert_eq!(v["matches"], json!([]));
    assert_eq!(v["superseded"], json!([]));
    assert_eq!(v["decisions"], json!(["decision_d1"]));
    assert_eq!(v["conflicts"], json!([]));
    assert_eq!(v["gaps"], json!([]));
    assert_eq!(v["warnings"], json!([]));
    // ftx.search parity: empty MATCH query → the legacy lane answers
    assert_eq!(v["ranker"], "legacy", "{v}");
}

#[test]
fn missing_question_is_usage_error() {
    let (_dir, wiki) = common::wiki();
    let out = bin()
        .args(["--root", wiki.root.to_str().unwrap(), "query-prepare"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["exit_code"], 2);
    assert_eq!(v["error_type"], "usage");
}
