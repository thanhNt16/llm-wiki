mod common;

use std::path::Path;
use std::process::Command;

use llm_wiki::claims;
use llm_wiki::ids;
use llm_wiki::txn::Txn;
use llm_wiki::util;
use serde_json::{json, Value};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_llm-wiki"))
}

fn init(dir: &Path) {
    let out = bin().arg("init").arg(dir).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "init: {:?}", out);
}

fn run_json(args: &[&str]) -> (i32, Value) {
    let out = bin().args(args).output().unwrap();
    let code = out.status.code().unwrap_or(-1);
    let v: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout not JSON ({e}): {}", String::from_utf8_lossy(&out.stdout)));
    (code, v)
}

fn claim_fixture(subject: &str, status: &str) -> Value {
    json!({
        "id": ids::new("claim"),
        "subject": subject,
        "predicate": "is",
        "value": "v-" ,
        "status": status,
        "authority": {"type": "agent_inference"},
        "evidence": [],
        "recorded_at": util::now_iso(),
        "version": 1,
    })
}

/// Commit one claim through the real save path (index stays derived).
fn seed_claim(dir: &Path, subject: &str, status: &str) -> Value {
    let wiki = llm_wiki::store::Wiki::open(dir).unwrap();
    let mut c = claim_fixture(subject, status);
    let mut txn = Txn::begin(&wiki, &ids::new_run()).unwrap();
    claims::save_claim(&wiki, &mut txn, &mut c).unwrap();
    txn.commit().unwrap();
    c
}

fn seed_queue(dir: &Path, items: &[Value]) {
    let q = dir.join(".llm-wiki/.state/review-queue.jsonl");
    std::fs::create_dir_all(q.parent().unwrap()).unwrap();
    let mut body = String::new();
    for item in items {
        body.push_str(&serde_json::to_string(item).unwrap());
        body.push('\n');
    }
    std::fs::write(q, body).unwrap();
}

fn review_item(kind: &str, problem: &str, affected: &[&str]) -> Value {
    json!({
        "id": ids::new("review"),
        "created_at": util::now_iso(),
        "kind": kind,
        "status": "open",
        "problem": problem,
        "evidence": [],
        "affected": affected,
        "risk": "medium",
        "proposed_resolution": "",
    })
}

fn params_file(dir: &Path, params: &Value) -> std::path::PathBuf {
    let p = dir.join("params.json");
    std::fs::write(&p, serde_json::to_string(params).unwrap()).unwrap();
    p
}

fn review_args(dir: &Path, sub: &str) -> Vec<String> {
    vec![
        "--root".to_string(),
        dir.to_str().unwrap().to_string(),
        "review".to_string(),
        sub.to_string(),
    ]
}

#[test]
fn list_filters_kind_and_status() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let a = review_item("needs_review", "needs checking", &[]);
    let b = review_item("authority_conflict", "two authorities", &[]);
    let mut c = review_item("needs_review", "deferred one", &[]);
    c["status"] = json!("deferred");
    seed_queue(dir.path(), &[a, b, c]);

    let root = dir.path().to_str().unwrap().to_string();
    let (code, v) = run_json(&["--root", &root, "review", "list"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["items"].as_array().unwrap().len(), 2);
    assert_eq!(v["summary"]["open"], 2);
    assert_eq!(v["summary"]["deferred"], 1);
    assert_eq!(v["summary"]["total"], 3);

    let (code, v) = run_json(&[
        "--root", &root, "review", "list", "--kind", "authority_conflict",
    ]);
    assert_eq!(code, 0, "{v}");
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["kind"], "authority_conflict");

    let (code, v) = run_json(&["--root", &root, "review", "list", "--all"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["items"].as_array().unwrap().len(), 3);

    let (code, v) = run_json(&[
        "--root", &root, "review", "list", "--status", "deferred",
    ]);
    assert_eq!(code, 0, "{v}");
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["status"], "deferred");
}

#[test]
fn show_returns_item_and_errors_on_unknown() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let item = review_item("needs_review", "q", &[]);
    let id = item["id"].as_str().unwrap().to_string();
    seed_queue(dir.path(), &[item]);
    let mut args = review_args(dir.path(), "show");
    args.extend(["--item".to_string(), id.clone()]);
    let argrefs: Vec<&str> = args.iter().map(String::as_str).collect();
    let (code, v) = run_json(&argrefs);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["id"], id);

    let mut args = review_args(dir.path(), "show");
    args.extend(["--item".to_string(), "review_missing".to_string()]);
    let argrefs: Vec<&str> = args.iter().map(String::as_str).collect();
    let (code, v) = run_json(&argrefs);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["error_type"], "txn");
}

#[test]
fn triage_groups_by_claim_subject_with_suggestion() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let claim = seed_claim(dir.path(), "widget", "candidate");
    let cid = claim["id"].as_str().unwrap();
    let item = review_item("needs_review", "untrusted text", &[cid]);
    seed_queue(dir.path(), &[item]);

    let root = dir.path().to_str().unwrap().to_string();
    let (code, v) = run_json(&["--root", &root, "review", "triage"]);
    assert_eq!(code, 0, "{v}");
    let groups = v["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["key"], "widget");
    assert_eq!(groups[0]["count"], 1);

    // non-subject group_by names an item field (review.py:320)
    let (code, v) = run_json(&[
        "--root", &root, "review", "triage", "--group-by", "kind",
    ]);
    assert_eq!(code, 0, "{v}");
    let groups = v["groups"].as_array().unwrap();
    assert_eq!(groups.len(), 1);
    assert_eq!(groups[0]["key"], "needs_review");
    // 0 independent origins → defer (independence < 2)
    assert_eq!(groups[0]["items"][0]["suggested_action"], "defer");
}

#[test]
fn act_accept_and_reject_flip_claim_and_resolve_item() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let claim = seed_claim(dir.path(), "s", "candidate");
    let cid = claim["id"].as_str().unwrap().to_string();
    let item = review_item("needs_review", "check", &[&cid]);
    let item_id = item["id"].as_str().unwrap().to_string();
    seed_queue(dir.path(), &[item]);

    let params = params_file(dir.path(), &json!({"claim_id": cid}));
    let root = dir.path().to_str().unwrap().to_string();
    let (code, v) = run_json(&[
        "--root", &root, "review", "act",
        "--item", &item_id,
        "--action", "accept",
        "--params", params.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["item"]["status"], "resolved");
    assert!(v["item"]["resolved_at"].is_string());
    assert_eq!(v["item"]["history"][0]["action"], "accept");
    assert!(v["run_id"].as_str().unwrap().starts_with("run_"));

    let wiki = llm_wiki::store::Wiki::open(dir.path()).unwrap();
    let c = claims::load_claim(&wiki, &cid).unwrap();
    assert_eq!(c["status"], "accepted");
    assert_eq!(c["acceptance"]["reason"], "human_review");
    assert_eq!(c["version"], 2); // bump once per commit
    assert_eq!(c["updated_at"].as_str().is_some(), true);

    // resolved item refuses further non-reopen actions
    let (code, v) = run_json(&[
        "--root", &root, "review", "act",
        "--item", &item_id,
        "--action", "reject",
        "--params", params.to_str().unwrap(),
    ]);
    assert_eq!(code, 3, "{v}");
    assert!(v["error"].as_str().unwrap().contains("already resolved"));
}

#[test]
fn act_reject_terminal_requires_reason_ok() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let claim = seed_claim(dir.path(), "s", "candidate");
    let cid = claim["id"].as_str().unwrap().to_string();
    let item = review_item("needs_review", "injection found", &[&cid]);
    let item_id = item["id"].as_str().unwrap().to_string();
    seed_queue(dir.path(), &[item]);
    let params = params_file(dir.path(), &json!({"claim_id": cid}));
    let root = dir.path().to_str().unwrap().to_string();
    let (code, v) = run_json(&[
        "--root", &root, "review", "act",
        "--item", &item_id,
        "--action", "reject",
        "--params", params.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    let wiki = llm_wiki::store::Wiki::open(dir.path()).unwrap();
    let c = claims::load_claim(&wiki, &cid).unwrap();
    assert_eq!(c["status"], "rejected");
    assert_eq!(c["acceptance"]["reason"], "human_review");
}

#[test]
fn act_mark_duplicate_merges_evidence_and_rejects() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    // dup claim carries the evidence; target gets it appended.
    let wiki = llm_wiki::store::Wiki::open(dir.path()).unwrap();
    let mut dup = claim_fixture("s", "candidate");
    dup["evidence"] = json!([{
        "source_id": ids::new("source"),
        "source_version": 1,
        "locator": {"type": "text"},
    }]);
    let mut txn = Txn::begin(&wiki, &ids::new_run()).unwrap();
    claims::save_claim(&wiki, &mut txn, &mut dup).unwrap();
    txn.commit().unwrap();
    let target = seed_claim(dir.path(), "s", "accepted");

    let dup_id = dup["id"].as_str().unwrap().to_string();
    let target_id = target["id"].as_str().unwrap().to_string();
    let item = review_item("duplicate_identity", "same thing twice", &[&dup_id]);
    let item_id = item["id"].as_str().unwrap().to_string();
    seed_queue(dir.path(), &[item]);

    let params = params_file(
        dir.path(),
        &json!({"claim_id": dup_id, "merge_into": target_id}),
    );
    let root = dir.path().to_str().unwrap().to_string();
    let (code, v) = run_json(&[
        "--root", &root, "review", "act",
        "--item", &item_id,
        "--action", "mark_duplicate",
        "--params", params.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    let wiki = llm_wiki::store::Wiki::open(dir.path()).unwrap();
    let d = claims::load_claim(&wiki, &dup_id).unwrap();
    assert_eq!(d["status"], "rejected");
    assert_eq!(d["acceptance"]["reason"], format!("duplicate_of:{target_id}"));
    let t = claims::load_claim(&wiki, &target_id).unwrap();
    assert_eq!(t["evidence"].as_array().unwrap().len(), 1);
}

#[test]
fn act_mark_superseded_chains_both_claims() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let old = seed_claim(dir.path(), "s", "accepted");
    let new = seed_claim(dir.path(), "s", "accepted");
    let old_id = old["id"].as_str().unwrap().to_string();
    let new_id = new["id"].as_str().unwrap().to_string();
    let item = review_item("possible_correction", "newer info", &[&old_id]);
    let item_id = item["id"].as_str().unwrap().to_string();
    seed_queue(dir.path(), &[item]);
    let params = params_file(dir.path(), &json!({"claim_id": old_id, "target": new_id}));
    let root = dir.path().to_str().unwrap().to_string();
    let (code, v) = run_json(&[
        "--root", &root, "review", "act",
        "--item", &item_id,
        "--action", "mark_superseded",
        "--params", params.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    let wiki = llm_wiki::store::Wiki::open(dir.path()).unwrap();
    let o = claims::load_claim(&wiki, &old_id).unwrap();
    assert_eq!(o["status"], "superseded");
    assert_eq!(o["superseded_by"], new_id);
    let n = claims::load_claim(&wiki, &new_id).unwrap();
    assert_eq!(n["supersedes"], json!([old_id]));
}

#[test]
fn act_set_scope_and_validity_update_claim() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let claim = seed_claim(dir.path(), "s", "accepted");
    let cid = claim["id"].as_str().unwrap().to_string();
    let item = review_item("ambiguous_scope", "where does this hold", &[&cid]);
    let item_id = item["id"].as_str().unwrap().to_string();
    seed_queue(dir.path(), &[item]);

    let root = dir.path().to_str().unwrap().to_string();
    let params = params_file(
        dir.path(),
        &json!({"claim_id": cid, "scope": {"repo": "orca"}}),
    );
    let (code, v) = run_json(&[
        "--root", &root, "review", "act",
        "--item", &item_id,
        "--action", "set_scope",
        "--params", params.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    let wiki = llm_wiki::store::Wiki::open(dir.path()).unwrap();
    let c = claims::load_claim(&wiki, &cid).unwrap();
    assert_eq!(c["scope"]["repo"], "orca");
}

#[test]
fn act_defer_then_reopen_roundtrip() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let item = review_item("needs_review", "later", &[]);
    let item_id = item["id"].as_str().unwrap().to_string();
    seed_queue(dir.path(), &[item]);
    let root = dir.path().to_str().unwrap().to_string();

    let (code, v) = run_json(&[
        "--root", &root, "review", "act",
        "--item", &item_id, "--action", "defer",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["item"]["status"], "deferred");
    assert!(v["item"]["deferred_at"].is_string());

    let (code, v) = run_json(&[
        "--root", &root, "review", "act",
        "--item", &item_id, "--action", "reopen",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["item"]["status"], "open");
    assert!(v["item"].get("deferred_at").is_none());
    assert_eq!(v["item"]["resolution"], "");
}

#[test]
fn act_errors_on_unknown_action_and_missing_claim_id() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let item = review_item("needs_review", "q", &[]);
    let item_id = item["id"].as_str().unwrap().to_string();
    seed_queue(dir.path(), &[item]);
    let root = dir.path().to_str().unwrap().to_string();

    let (code, v) = run_json(&[
        "--root", &root, "review", "act",
        "--item", &item_id, "--action", "bogus",
    ]);
    assert_eq!(code, 3, "{v}");
    assert!(v["error"].as_str().unwrap().contains("unknown review action"));

    let (code, v) = run_json(&[
        "--root", &root, "review", "act",
        "--item", &item_id, "--action", "accept",
    ]);
    assert_eq!(code, 3, "{v}");
    assert!(v["error"].as_str().unwrap().contains("requires claim_id"));
}

#[test]
fn apply_batch_commits_all_actions_in_one_revision() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let c1 = seed_claim(dir.path(), "a", "candidate");
    let c2 = seed_claim(dir.path(), "b", "candidate");
    let i1 = review_item("needs_review", "p1", &[c1["id"].as_str().unwrap()]);
    let i2 = review_item("needs_review", "p2", &[c2["id"].as_str().unwrap()]);
    seed_queue(dir.path(), &[i1.clone(), i2.clone()]);

    let actions = json!([
        {"id": i1["id"], "action": "accept",
         "params": {"claim_id": c1["id"]}},
        {"id": i2["id"], "action": "reject",
         "params": {"claim_id": c2["id"]}},
    ]);
    let f = dir.path().join("actions.json");
    std::fs::write(&f, serde_json::to_string(&actions).unwrap()).unwrap();
    let root = dir.path().to_str().unwrap().to_string();
    let (code, v) = run_json(&[
        "--root", &root, "review", "apply", "--file", f.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["applied"], 2);
    assert_eq!(v["results"][0]["status"], "resolved");
    assert_eq!(v["results"][1]["status"], "resolved");
    assert_eq!(v["revision"], 3); // init claims commits (2) + 1 batch commit

    let wiki = llm_wiki::store::Wiki::open(dir.path()).unwrap();
    assert_eq!(
        claims::load_claim(&wiki, c1["id"].as_str().unwrap()).unwrap()["status"],
        "accepted"
    );
    assert_eq!(
        claims::load_claim(&wiki, c2["id"].as_str().unwrap()).unwrap()["status"],
        "rejected"
    );
    assert_eq!(wiki.revision().unwrap(), 3);
}

#[test]
fn apply_batch_rejects_bad_file_shape() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let f = dir.path().join("bad.json");
    std::fs::write(&f, "{\"actions\": 42}").unwrap();
    let root = dir.path().to_str().unwrap().to_string();
    let (code, v) = run_json(&[
        "--root", &root, "review", "apply", "--file", f.to_str().unwrap(),
    ]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["error_type"], "txn");
}

#[test]
fn stale_base_revision_conflicts() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let claim = seed_claim(dir.path(), "s", "candidate");
    let cid = claim["id"].as_str().unwrap().to_string();
    let item = review_item("needs_review", "q", &[&cid]);
    let item_id = item["id"].as_str().unwrap().to_string();
    seed_queue(dir.path(), &[item]);
    let params = params_file(dir.path(), &json!({"claim_id": cid}));
    let root = dir.path().to_str().unwrap().to_string();
    let (code, v) = run_json(&[
        "--root", &root, "review", "act",
        "--item", &item_id, "--action", "accept",
        "--params", params.to_str().unwrap(),
        "--base-revision", "0",
    ]);
    assert_eq!(code, 4, "{v}");
    assert_eq!(v["error_type"], "conflict");
}

#[test]
fn act_merge_and_mark_authoritative() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let wiki = llm_wiki::store::Wiki::open(dir.path()).unwrap();
    let mut dup = claim_fixture("s", "candidate");
    dup["evidence"] = json!([{
        "source_id": ids::new("source"),
        "source_version": 1,
        "locator": {"type": "text"},
    }]);
    let mut txn = Txn::begin(&wiki, &ids::new_run()).unwrap();
    claims::save_claim(&wiki, &mut txn, &mut dup).unwrap();
    txn.commit().unwrap();
    let target = seed_claim(dir.path(), "s", "accepted");
    let dup_id = dup["id"].as_str().unwrap().to_string();
    let target_id = target["id"].as_str().unwrap().to_string();

    let item = review_item("duplicate_identity", "same thing", &[&dup_id]);
    let item_id = item["id"].as_str().unwrap().to_string();
    seed_queue(dir.path(), &[item]);
    let params = params_file(
        dir.path(),
        &json!({"claim_id": dup_id, "merge_into": target_id}),
    );
    let root = dir.path().to_str().unwrap().to_string();
    let (code, v) = run_json(&[
        "--root", &root, "review", "act",
        "--item", &item_id, "--action", "merge",
        "--params", params.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    let wiki = llm_wiki::store::Wiki::open(dir.path()).unwrap();
    let d = claims::load_claim(&wiki, &dup_id).unwrap();
    assert_eq!(d["status"], "rejected");
    assert_eq!(d["acceptance"]["reason"], format!("duplicate_of:{target_id}"));
    let t = claims::load_claim(&wiki, &target_id).unwrap();
    assert_eq!(t["evidence"].as_array().unwrap().len(), 1);

    // merge into self is refused
    let item2 = review_item("duplicate_identity", "same", &[&target_id]);
    let item2_id = item2["id"].as_str().unwrap().to_string();
    seed_queue(dir.path(), &[item2]);
    let params2 = params_file(
        dir.path(),
        &json!({"claim_id": target_id, "merge_into": target_id}),
    );
    let (code, v) = run_json(&[
        "--root", &root, "review", "act",
        "--item", &item2_id, "--action", "merge",
        "--params", params2.to_str().unwrap(),
    ]);
    assert_eq!(code, 3, "{v}");
    assert!(v["error"].as_str().unwrap().contains("into itself"));

    // mark_authoritative: sets authority, accepts the claim
    let claim = seed_claim(dir.path(), "s2", "candidate");
    let cid = claim["id"].as_str().unwrap().to_string();
    let item3 = review_item("authority_conflict", "two authorities", &[&cid]);
    let item3_id = item3["id"].as_str().unwrap().to_string();
    seed_queue(dir.path(), &[item3]);
    let params3 = params_file(dir.path(), &json!({"claim_id": cid}));
    let (code, v) = run_json(&[
        "--root", &root, "review", "act",
        "--item", &item3_id, "--action", "mark_authoritative",
        "--params", params3.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    let wiki = llm_wiki::store::Wiki::open(dir.path()).unwrap();
    let c = claims::load_claim(&wiki, &cid).unwrap();
    assert_eq!(c["status"], "accepted");
    assert_eq!(c["authority"]["type"], "explicit_project_decision");
    assert_eq!(
        c["authority"]["source"],
        format!("human_review:{item3_id}")
    );
}

#[test]
fn act_set_validity_sets_window() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let claim = seed_claim(dir.path(), "s", "accepted");
    let cid = claim["id"].as_str().unwrap().to_string();
    let item = review_item("stale_claim", "out of date", &[&cid]);
    let item_id = item["id"].as_str().unwrap().to_string();
    seed_queue(dir.path(), &[item]);
    let params = params_file(
        dir.path(),
        &json!({"claim_id": cid, "valid_from": "2026-01-01",
                "valid_to": "2026-06-01"}),
    );
    let root = dir.path().to_str().unwrap().to_string();
    let (code, v) = run_json(&[
        "--root", &root, "review", "act",
        "--item", &item_id, "--action", "set_validity",
        "--params", params.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    let wiki = llm_wiki::store::Wiki::open(dir.path()).unwrap();
    let c = claims::load_claim(&wiki, &cid).unwrap();
    assert_eq!(c["valid_from"], "2026-01-01");
    assert_eq!(c["valid_to"], "2026-06-01");
}

#[test]
fn act_set_topic_kind_resolves_conflict() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    std::fs::write(
        dir.path().join(".llm-wiki/topics.json"),
        serde_json::to_string(&json!({
            "topics": {
                "widget": {
                    "kind": "concept",
                    "title": "Widget",
                    "status": "conflict",
                    "aliases": [],
                    "alternate_kinds": ["entity"],
                    "declared_by": [],
                }
            }
        }))
        .unwrap(),
    )
    .unwrap();
    let item = review_item(
        "topic_kind_conflict",
        "subject 'widget' declared with conflicting kinds",
        &["widget"],
    );
    let item_id = item["id"].as_str().unwrap().to_string();
    seed_queue(dir.path(), &[item]);
    let params = params_file(
        dir.path(),
        &json!({"subject": "widget", "kind": "entity"}),
    );
    let root = dir.path().to_str().unwrap().to_string();
    let (code, v) = run_json(&[
        "--root", &root, "review", "act",
        "--item", &item_id, "--action", "set_topic_kind",
        "--params", params.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["item"]["status"], "resolved");
    let reg: Value = serde_json::from_str(
        &std::fs::read_to_string(dir.path().join(".llm-wiki/topics.json"))
            .unwrap(),
    )
    .unwrap();
    assert_eq!(reg["topics"]["widget"]["kind"], "entity");
    assert_eq!(reg["topics"]["widget"]["status"], "resolved");

    // subject not in the item's affected list is refused
    let item2 = review_item("topic_kind_conflict", "conflict", &["widget"]);
    let item2_id = item2["id"].as_str().unwrap().to_string();
    seed_queue(dir.path(), &[item2]);
    let params2 = params_file(
        dir.path(),
        &json!({"subject": "other", "kind": "entity"}),
    );
    let (code, v) = run_json(&[
        "--root", &root, "review", "act",
        "--item", &item2_id, "--action", "set_topic_kind",
        "--params", params2.to_str().unwrap(),
    ]);
    assert_eq!(code, 3, "{v}");
    assert!(v["error"].as_str().unwrap().contains("not affected"));
}
