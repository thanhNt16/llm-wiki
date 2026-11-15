//! `page-targets`, `write-page`, `page-show`, `build-pages` and the
//! pages.verify citation gate (P3 unit A). CLI-level tests drive the real
//! binary; engine-level helpers (deps graph, txns) seed the wiki state
//! where the CLI has no write lane.

mod common;

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

use llm_wiki::deps;
use llm_wiki::store::Wiki;
use llm_wiki::txn::Txn;
use serde_json::{json, Value};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_llm-wiki"))
}

fn run_json(args: &[&str]) -> (i32, Value) {
    let out = bin().args(args).output().unwrap();
    let code = out.status.code().unwrap_or(-1);
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!("stdout not JSON ({e}): {}", String::from_utf8_lossy(&out.stdout))
    });
    (code, v)
}

fn write_claim(dir: &Path, id: &str, subject: &str, predicate: &str, status: &str, sources: &[&str]) {
    let evidence: Vec<Value> = sources
        .iter()
        .map(|s| json!({"source_id": s, "version": 1}))
        .collect();
    let claim = json!({
        "id": id,
        "subject": subject,
        "predicate": predicate,
        "value": "v",
        "status": status,
        "version": 1,
        "evidence": evidence,
    });
    std::fs::write(
        dir.join(format!(".llm-wiki/claims/{id}.json")),
        serde_json::to_string(&claim).unwrap(),
    )
    .unwrap();
}

fn txn(wiki: &Wiki) -> Txn<'_> {
    Txn::begin(wiki, &llm_wiki::txn::mint_run_id()).unwrap()
}

/// Minimal valid page text for `write-page` (`type`/`subject`/`title`/
/// `created` gates; body carries `[[claim_…]]` links).
fn page_text(subject: &str, title: &str, kind_type: &str, links: &[&str]) -> String {
    let body_links: String = links
        .iter()
        .map(|l| format!("See [[{l}]].\n"))
        .collect();
    format!(
        "---\ntype: {kind_type}\nsubject: {subject}\ntitle: {title}\ncreated: 2026-11-15T00:00:00Z\n---\n\n# {title}\n\n{body_links}"
    )
}

// ---- page-targets -------------------------------------------------------

#[test]
fn page_targets_routing_waiting_and_filters() {
    let (dir, _wiki) = common::wiki();
    let root = dir.path();
    // default-routed (no heuristic matches "api.search" → concepts)
    write_claim(root, "claim_t1", "api.search", "status", "accepted", &["source_s1"]);
    write_claim(root, "claim_t2", "api.search", "feature", "provisional", &["source_s2"]);
    // procedural predicate → heuristic:1 → procedures
    write_claim(root, "claim_t3", "runbook.deploy", "step migrate", "accepted", &["source_s3"]);
    write_claim(root, "claim_t4", "runbook.deploy", "step verify", "accepted", &["source_s4"]);
    // below thresholds → waiting
    write_claim(root, "claim_t5", "tiny.topic", "status", "accepted", &["source_s1"]);

    let (code, v) = run_json(&["--root", root.to_str().unwrap(), "page-targets"]);
    assert_eq!(code, 0, "{v}");
    let targets = v["targets"].as_array().unwrap();
    let by_subject = |s: &str| {
        targets
            .iter()
            .find(|t| t["subject"] == s)
            .unwrap_or_else(|| panic!("no target for {s}: {v}"))
    };
    let api = by_subject("api.search");
    assert_eq!(api["kind"], "concepts", "{api}");
    assert_eq!(api["artifact"], "wiki/concepts/api-search.md");
    assert_eq!(api["claim_count"], 2);
    assert_eq!(api["distinct_origins"], 2);
    assert_eq!(api["reason"], "corroborated");
    assert_eq!(api["exists"], false);
    assert_eq!(api["stale"], false);
    assert_eq!(api["claim_ids"], json!(["claim_t1@1", "claim_t2@1"]));
    let rb = by_subject("runbook.deploy");
    assert_eq!(rb["kind"], "procedures", "{rb}");
    assert_eq!(rb["matched_rule"], "heuristic:1");
    // non-eligible/below-threshold subject is waiting, not targeted
    assert_eq!(
        v["waiting"],
        json!([{
            "subject": "tiny.topic",
            "claim_count": 1,
            "distinct_origins": 1,
            "reason": "below_threshold",
        }]),
        "{v}"
    );
    assert!(v["stale_artifacts"].is_array());
    assert!(v["warnings"].is_array());

    // --kind forces routing for every target
    let (code, v) = run_json(&[
        "--root", root.to_str().unwrap(), "page-targets", "--kind", "entities",
    ]);
    assert_eq!(code, 0, "{v}");
    for t in v["targets"].as_array().unwrap() {
        assert_eq!(t["kind"], "entities", "{t}");
    }

    // --min-claims/--min-sources push targets into waiting
    let (code, v) = run_json(&[
        "--root",
        root.to_str().unwrap(),
        "page-targets",
        "--min-claims",
        "3",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["targets"].as_array().unwrap().len(), 0, "{v}");
    assert!(v["waiting"].as_array().unwrap().len() >= 3, "{v}");

    // unknown kind rejected (exit 3 txn, same as Python's TxnError)
    let (code, v) = run_json(&[
        "--root", root.to_str().unwrap(), "page-targets", "--kind", "bogus",
    ]);
    assert_eq!(code, 2, "{v}"); // clap value_parser rejects pre-dispatch
    let _ = v;
}

#[test]
fn page_targets_stale_only_and_decide_noop() {
    let (dir, wiki) = common::wiki();
    let root = dir.path();
    write_claim(root, "claim_s1", "api.search", "status", "accepted", &["source_s1"]);
    write_claim(root, "claim_s2", "api.search", "feature", "accepted", &["source_s2"]);
    write_claim(root, "claim_s3", "runbook.deploy", "step go", "accepted", &["source_s3"]);
    write_claim(root, "claim_s4", "runbook.deploy", "step back", "accepted", &["source_s4"]);

    // materialize + stale-flag one target artifact
    let concepts = root.join(".llm-wiki/wiki/concepts");
    std::fs::create_dir_all(&concepts).unwrap();
    std::fs::write(concepts.join("api-search.md"), "---\ntype: concept\n---\nx\n").unwrap();
    let mut t = txn(&wiki);
    deps::register(&mut t, "wiki/concepts/api-search.md", &["claim_s1@1"]).unwrap();
    deps::mark_stale(&mut t, &["wiki/concepts/api-search.md"]).unwrap();
    t.commit().unwrap();

    let (code, v) = run_json(&[
        "--root",
        root.to_str().unwrap(),
        "page-targets",
        "--stale-only",
        "--decide", // env unset → advisory lane is a no-op
    ]);
    assert_eq!(code, 0, "{v}");
    let targets = v["targets"].as_array().unwrap();
    assert_eq!(targets.len(), 1, "{v}");
    assert_eq!(targets[0]["subject"], "api.search");
    assert_eq!(targets[0]["exists"], true);
    assert_eq!(targets[0]["stale"], true);
    assert!(targets[0].get("decision").is_none(), "{v}");
    assert_eq!(v["stale_artifacts"], json!(["wiki/concepts/api-search.md"]));
}

// ---- write-page / page-show --------------------------------------------

#[test]
fn write_page_commits_deps_and_page_show_reads_back() {
    let (dir, _wiki) = common::wiki();
    let root = dir.path();
    // ULID-format id so page-show's load_claim path surfaces claim_details
    let cid = llm_wiki::ids::new("claim");
    write_claim(root, &cid, "api.search", "status", "accepted", &["source_s1"]);

    let text = page_text("api.search", "API Search", "concept", &[&cid]);
    let text_arg = format!("--text={text}");
    let dep = format!("{cid}@1");
    let (code, v) = run_json(&[
        "--root",
        root.to_str().unwrap(),
        "write-page",
        "--artifact",
        "wiki/concepts/api-search.md",
        &text_arg,
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["written"], "wiki/concepts/api-search.md");
    assert_eq!(v["deps"], json!([dep]), "{v}");
    assert_eq!(v["claims_cited"], 1);

    // deps registered in the graph
    let g = deps::graph(&_wiki).unwrap();
    assert_eq!(
        g["edges"]["wiki/concepts/api-search.md"],
        json!([dep])
    );

    // subject now resolves as a declared topic page (page-show reads it back)
    let (code, v) = run_json(&[
        "--root",
        root.to_str().unwrap(),
        "page-show",
        "--artifact",
        "wiki/concepts/api-search.md",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["artifact"], "wiki/concepts/api-search.md");
    assert_eq!(v["front"]["subject"], "api.search");
    assert_eq!(v["front"]["type"], "concept");
    assert_eq!(v["front"]["stale"], false);
    assert!(v["body"].as_str().unwrap().contains(&format!("[[{cid}]]")), "{v}");
    assert_eq!(v["deps_registered"], json!([dep]));
    assert_eq!(v["claim_details"].as_array().unwrap()[0]["id"], cid.as_str());
    assert_eq!(v["stale"], false);
}

#[test]
fn write_page_deps_flag_and_stdin_file() {
    let (dir, _wiki) = common::wiki();
    let root = dir.path();
    write_claim(root, "claim_d1", "api.auth", "status", "accepted", &["source_s1"]);
    write_claim(root, "claim_d2", "api.auth", "feature", "accepted", &["source_s2"]);

    // --deps comma list supplies extra dep beyond the linked claim
    let text = page_text("api.auth", "API Auth", "concept", &["claim_d1"]);
    let text_arg = format!("--text={text}");
    let (code, v) = run_json(&[
        "--root",
        root.to_str().unwrap(),
        "write-page",
        "--artifact",
        "wiki/concepts/api-auth.md",
        &text_arg,
        "--deps",
        "claim_d1@1, claim_d2@1",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["deps"], json!(["claim_d1@1", "claim_d2@1"]), "{v}");

    // --file - reads stdin; --deps-file supplies a dep list
    let deps_file = root.join("deps.json");
    std::fs::write(&deps_file, r#"["claim_d1@1"]"#).unwrap();
    let mut child = bin()
        .args([
            "--root",
            root.to_str().unwrap(),
            "write-page",
            "--artifact",
            "wiki/concepts/api-auth2.md",
            "--file",
            "-",
            "--deps-file",
            deps_file.to_str().unwrap(),
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let text2 = page_text("api.auth2", "API Auth Two", "concept", &[]);
    child.stdin.as_mut().unwrap().write_all(text2.as_bytes()).unwrap();
    let out = child.wait_with_output().unwrap();
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(0), "{v}");
    // subject must slug-match artifact: api.auth2 → api-auth2 ✓; dep came
    // from --deps-file (no [[claim]] links in body)
    assert_eq!(v["deps"], json!(["claim_d1@1"]), "{v}");
}

#[test]
fn write_page_near_duplicate_refusal_and_allow_new() {
    let (dir, _wiki) = common::wiki();
    let root = dir.path();
    write_claim(root, "claim_n1", "alpha.topic", "status", "accepted", &["source_s1"]);
    write_claim(root, "claim_n2", "beta.topic", "status", "accepted", &["source_s2"]);

    let first = page_text("alpha.topic", "Shared Title", "concept", &["claim_n1"]);
    let first_arg = format!("--text={first}");
    let (code, v) = run_json(&[
        "--root",
        root.to_str().unwrap(),
        "write-page",
        "--artifact",
        "wiki/concepts/alpha-topic.md",
        &first_arg,
    ]);
    assert_eq!(code, 0, "{v}");

    // identical title → twin detected regardless of token overlap
    let second = page_text("beta.topic", "Shared Title", "concept", &["claim_n2"]);
    let second_arg = format!("--text={second}");
    let (code, v) = run_json(&[
        "--root",
        root.to_str().unwrap(),
        "write-page",
        "--artifact",
        "wiki/concepts/beta-topic.md",
        &second_arg,
    ]);
    assert_eq!(code, 3, "{v}");
    assert!(
        v["error"].as_str().unwrap().starts_with("near-duplicate page: wiki/concepts/alpha-topic.md"),
        "{v}"
    );
    assert!(!root.join(".llm-wiki/wiki/concepts/beta-topic.md").exists());

    // --allow-new forces it through and stamps forced_new
    let (code, v) = run_json(&[
        "--root",
        root.to_str().unwrap(),
        "write-page",
        "--artifact",
        "wiki/concepts/beta-topic.md",
        &second_arg,
        "--allow-new",
        "--decide", // env unset → advisory no-op
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["written"], "wiki/concepts/beta-topic.md");
    let (code, v) = run_json(&[
        "--root",
        root.to_str().unwrap(),
        "page-show",
        "--artifact",
        "wiki/concepts/beta-topic.md",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["front"]["forced_new"], true);
}

#[test]
fn write_page_validation_errors() {
    let (dir, _wiki) = common::wiki();
    let root = dir.path();
    write_claim(root, "claim_e1", "api.auth", "status", "accepted", &["source_s1"]);

    // reserved artifact
    let text = page_text("x", "X", "concept", &["claim_e1"]);
    let text_arg = format!("--text={text}");
    let (code, v) = run_json(&[
        "--root",
        root.to_str().unwrap(),
        "write-page",
        "--artifact",
        "wiki/index.md",
        &text_arg,
    ]);
    assert_eq!(code, 3, "{v}");
    assert!(v["error"].as_str().unwrap().starts_with("reserved artifact"), "{v}");

    // unresolved [[claim]] link
    let text = page_text("api.auth", "API Auth", "concept", &["claim_ghost"]);
    let text_arg = format!("--text={text}");
    let (code, v) = run_json(&[
        "--root",
        root.to_str().unwrap(),
        "write-page",
        "--artifact",
        "wiki/concepts/api-auth.md",
        &text_arg,
    ]);
    assert_eq!(code, 3, "{v}");
    assert!(
        v["error"].as_str().unwrap().starts_with("unresolved [[claim]] links: claim_ghost"),
        "{v}"
    );

    // no text source at all
    let (code, v) = run_json(&[
        "--root",
        root.to_str().unwrap(),
        "write-page",
        "--artifact",
        "wiki/concepts/api-auth.md",
    ]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["error"], "provide --file (- for stdin) or --text");
}

// ---- build-pages --------------------------------------------------------

#[test]
fn build_pages_fabric_writes_and_registers() {
    let (dir, wiki) = common::wiki();
    let root = dir.path();
    write_claim(root, "claim_b1", "api.search", "status", "accepted", &["source_s1"]);

    // a real section page so wiki/concepts/index.md fabricates
    let text = page_text("api.search", "API Search", "concept", &["claim_b1"]);
    let text_arg = format!("--text={text}");
    let (code, v) = run_json(&[
        "--root",
        root.to_str().unwrap(),
        "write-page",
        "--artifact",
        "wiki/concepts/api-search.md",
        &text_arg,
    ]);
    assert_eq!(code, 0, "{v}");

    let (code, v) = run_json(&["--root", root.to_str().unwrap(), "build-pages"]);
    assert_eq!(code, 0, "{v}");
    assert!(v["changes"]["pages_built"].as_i64().unwrap() >= 5, "{v}");

    for rel in [
        "wiki/index.md",
        "wiki/overview.md",
        "wiki/review.md",
        "wiki/log.md",
        "wiki/changes/index.md",
        "wiki/concepts/index.md",
    ] {
        assert!(root.join(".llm-wiki").join(rel).is_file(), "missing {rel}");
    }

    // every fabric page is dep-registered (verify's registration gate)
    let g = deps::graph(&wiki).unwrap();
    for rel in [
        "wiki/index.md",
        "wiki/overview.md",
        "wiki/review.md",
        "wiki/log.md",
        "wiki/changes/index.md",
        "wiki/concepts/index.md",
    ] {
        assert!(g["edges"].as_object().unwrap().contains_key(rel), "unregistered {rel}");
    }

    // index.md lists the claim subject row
    let index = std::fs::read_to_string(root.join(".llm-wiki/wiki/index.md")).unwrap();
    assert!(index.contains("api.search"), "{index}");

    // verify is clean after a full build
    let (code, v) = run_json(&["--root", root.to_str().unwrap(), "verify"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["ok"], true, "{v}");
}

// ---- verify (citation gate) --------------------------------------------

#[test]
fn verify_stale_bad_dep_and_missing_artifact_fail() {
    let (dir, wiki) = common::wiki();
    let root = dir.path();
    let concepts = root.join(".llm-wiki/wiki/concepts");
    std::fs::create_dir_all(&concepts).unwrap();

    // stale artifact that was never rebuilt
    std::fs::write(concepts.join("stale-page.md"), "---\ndeps:\n---\nbody\n").unwrap();
    let mut t = txn(&wiki);
    deps::register(&mut t, "wiki/concepts/stale-page.md", &[]).unwrap();
    deps::mark_stale(&mut t, &["wiki/concepts/stale-page.md"]).unwrap();
    t.commit().unwrap();

    let (code, v) = run_json(&["--root", root.to_str().unwrap(), "verify"]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["ok"], false, "{v}");
    let errors: Vec<&str> = v["errors"].as_array().unwrap().iter().map(|e| e.as_str().unwrap()).collect();
    assert!(
        errors.iter().any(|e| *e == "stale artifact not rebuilt: wiki/concepts/stale-page.md"),
        "{errors:?}"
    );
    assert_eq!(v["stale"], json!(["wiki/concepts/stale-page.md"]));
    // audit checks ride along additively
    assert!(v["checks"].as_array().unwrap().iter().any(|c| c["name"] == "dep_edges"), "{v}");

    // dep pinned to an unknown claim id
    let mut t = txn(&wiki);
    deps::clear_stale(&mut t, &["wiki/concepts/stale-page.md"]).unwrap();
    deps::register(&mut t, "wiki/concepts/bad-dep.md", &["claim_ghost@9"]).unwrap();
    t.commit().unwrap();
    std::fs::write(
        concepts.join("bad-dep.md"),
        "---\ndeps:\n  - claim_ghost@9\n---\nbody\n",
    )
    .unwrap();

    let (code, v) = run_json(&["--root", root.to_str().unwrap(), "verify"]);
    assert_eq!(code, 3, "{v}");
    let errors: Vec<&str> = v["errors"].as_array().unwrap().iter().map(|e| e.as_str().unwrap()).collect();
    assert!(
        errors.iter().any(|e| e.contains("claim_ghost") && e.contains("bad-dep.md")),
        "{errors:?}"
    );

    // registered edge whose artifact file is gone
    let mut t = txn(&wiki);
    deps::register(&mut t, "wiki/concepts/gone.md", &[]).unwrap();
    t.commit().unwrap();

    let (code, v) = run_json(&["--root", root.to_str().unwrap(), "verify"]);
    assert_eq!(code, 3, "{v}");
    let errors: Vec<&str> = v["errors"].as_array().unwrap().iter().map(|e| e.as_str().unwrap()).collect();
    assert!(
        errors.iter().any(|e| *e == "missing artifact: wiki/concepts/gone.md"),
        "{errors:?}"
    );
}
