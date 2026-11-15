//! Compile pipeline tests — ingest → compile-plan → stage-candidates →
//! reconcile-prepare → reconcile-apply, exit-code contract, and the §13
//! dual-engine interop (a Python-shaped comparisons.json must feed a
//! Rust reconcile-apply).
mod common;

use std::path::Path;
use std::process::Command;

use llm_wiki::claims;
use llm_wiki::compile;
use llm_wiki::evidence::sources::{self, IngestOpts};
use llm_wiki::store::Wiki;
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

fn init(dir: &Path) {
    let out = bin().arg("init").arg(dir).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "init: {:?}", out);
}

fn ingest_text_lib(wiki: &Wiki, text: &str, text_ref: &str) -> (String, i64) {
    let r = sources::ingest_text(wiki, text, text_ref, "text", &IngestOpts::default())
        .expect("ingest_text");
    (
        r["source_id"].as_str().unwrap().to_string(),
        r["version"].as_i64().unwrap(),
    )
}

fn stage_shorthand(
    wiki: &Wiki,
    sid: &str,
    ver: i64,
    subject: &str,
    predicate: &str,
    value: &str,
) -> String {
    compile::stage_candidates(
        wiki,
        sid,
        ver,
        vec![json!({
            "subject": subject,
            "predicate": predicate,
            "value": value,
        })],
        &json!({"source_id": sid, "source_version": ver}),
        None,
    )
    .expect("stage_candidates")
}

fn claims_on_disk(wiki: &Wiki) -> Vec<Value> {
    claims::list(wiki).expect("claims::list")
}

// ---------------------------------------------------------------------------
// full lib cycle: plan → stage → prepare → apply, claim created + topics +
// mark_compiled + revision bump; second plan is empty
// ---------------------------------------------------------------------------

#[test]
fn full_cycle_claim_created_and_marked_compiled() {
    let (_d, wiki) = common::wiki();
    let rev0 = wiki.revision().unwrap();
    let (sid, ver) = ingest_text_lib(&wiki, "alpha doc body", "doc-alpha");

    let plan = compile::compile_plan(&wiki).unwrap();
    assert_eq!(plan["pending"].as_array().unwrap().len(), 1);
    assert_eq!(plan["pending"][0]["source_id"], json!(sid));

    let run_id = stage_shorthand(&wiki, &sid, ver, "alpha.beta", "status", "active");
    assert!(run_id.starts_with("run_"));
    let rundir = wiki.p(&format!(".state/staging/{run_id}"));
    // §5.2 run-dir contractual set (writer side)
    assert!(rundir.join("compile.json").is_file());
    assert!(rundir.join("candidates.jsonl").is_file());

    let prep = compile::reconcile_prepare(&wiki, &run_id).unwrap();
    assert_eq!(prep["run_id"], json!(run_id));
    let cmp = &prep["comparisons"][0];
    assert_eq!(cmp["index"], 0);
    assert_eq!(cmp["key"], json!("alpha.beta/status"));
    assert_eq!(cmp["matches"], json!([]));
    assert_eq!(cmp["truncated_matches"], 0);
    assert_eq!(cmp["auto"], json!("UNRELATED"));
    assert!(rundir.join("comparisons.json").is_file());
    assert!(rundir.join("stamps.json").is_file());

    let topics_rows = vec![json!({
        "subject": "alpha.beta",
        "kind": "concept",
        "title": "Alpha Beta",
    })];
    let declared_by = json!({"source_id": sid, "version": ver, "title": "Alpha"});
    let receipt = compile::reconcile_apply(
        &wiki,
        &run_id,
        vec![],
        wiki.revision().unwrap(),
        Some(topics_rows),
        Some(&declared_by),
        false,
    )
    .unwrap();
    assert_eq!(receipt["changes"]["claims_created"], 1);
    assert_eq!(receipt["topics"]["declared"], 1);
    assert_eq!(receipt["auto"]["unrelated"], 1);

    // claim created (authority shorthand default → auto-accepted)
    let on_disk = claims_on_disk(&wiki);
    assert_eq!(on_disk.len(), 1);
    let c = &on_disk[0];
    assert_eq!(c["subject"], json!("alpha.beta"));
    assert_eq!(c["predicate"], json!("status"));
    assert_eq!(c["value"], json!("active"));
    assert_eq!(c["status"], json!("accepted"));
    assert_eq!(c["version"], 1);
    assert_eq!(c["evidence"][0]["source_id"], json!(sid));
    assert_eq!(c["root_origin"], json!(sid));

    // topics.json merged: declared subject + source.<slug> entry
    let reg = llm_wiki::topics::load(&wiki).unwrap();
    assert_eq!(reg.topics["alpha.beta"]["kind"], json!("concept"));
    assert!(
        reg.topics.keys().any(|k| k.starts_with("source.")),
        "source topic registered: {:?}",
        reg.topics.keys().collect::<Vec<_>>()
    );

    // mark_compiled → second plan is empty; revision bumped
    let plan2 = compile::compile_plan(&wiki).unwrap();
    assert_eq!(plan2["pending"], json!([]), "{plan2}");
    assert!(wiki.revision().unwrap() > rev0);
    // compile.json consumed
    assert!(!rundir.join("compile.json").is_file());
    // extraction persisted for the compiled version
    assert!(
        wiki.p(&format!("sources/{sid}/extraction_v{ver}.json")).is_file()
    );
}

// ---------------------------------------------------------------------------
// CLI cycle with .jsonl shorthand + topics.jsonl, driving the umbrella
// ---------------------------------------------------------------------------

#[test]
fn cli_compile_umbrella_extract_resume_done() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let r = dir.path().to_str().unwrap();

    let (code, v) = run_json(&[
        "--root", r, "ingest", "--text", "alpha doc body", "--text-ref", "doc-a",
    ]);
    assert_eq!(code, 0, "{v}");
    let sid = v["source_id"].as_str().unwrap().to_string();

    let (code, plan) = run_json(&["--root", r, "compile-plan"]);
    assert_eq!(code, 0, "{plan}");
    assert_eq!(plan["pending"].as_array().unwrap().len(), 1);

    let (code, v) = run_json(&["--root", r, "compile"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["phase"], json!("extract"));
    let pending = &v["pending"][0];
    assert_eq!(pending["source_id"], json!(sid));
    // pending.json fields are contractual (§5.2)
    for k in [
        "source_id",
        "source_version",
        "run_id",
        "content_path",
        "topics_path",
        "candidates_path",
        "classifications_path",
        "corpus_fingerprint",
    ] {
        assert!(!pending[k].is_null(), "pending.json field {k}: {pending}");
    }
    let cand_path = pending["candidates_path"].as_str().unwrap().to_string();
    let topics_path = pending["topics_path"].as_str().unwrap().to_string();

    // agent shorthand: one sparse object per line
    std::fs::write(
        &cand_path,
        "{\"subject\":\"alpha.beta\",\"predicate\":\"status\",\"value\":\"active\"}\n",
    )
    .unwrap();
    std::fs::write(
        &topics_path,
        "{\"subject\":\"alpha.beta\",\"kind\":\"concept\",\"title\":\"Alpha\"}\n",
    )
    .unwrap();

    let (code, v) = run_json(&["--root", r, "compile", "--resume"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["phase"], json!("done"), "{v}");
    assert_eq!(v["changes"]["claims_created"], 1, "{v}");
    assert_eq!(v["auto"]["unrelated"], 1, "{v}");
    assert_eq!(v["receipts"][0]["topics"]["declared"], 1, "{v}");

    // claim landed; version marked compiled
    let (code, plan) = run_json(&["--root", r, "compile-plan"]);
    assert_eq!(code, 0, "{plan}");
    assert_eq!(plan["pending"], json!([]), "{plan}");
    let dot = dir.path().join(".llm-wiki");
    let claims_dir = dot.join("claims");
    let files: Vec<_> = std::fs::read_dir(&claims_dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(files.len(), 1, "{files:?}");
    let c: Value = serde_json::from_str(
        &std::fs::read_to_string(claims_dir.join(&files[0])).unwrap(),
    )
    .unwrap();
    assert_eq!(c["subject"], json!("alpha.beta"));
    assert_eq!(c["evidence"][0]["locator"]["type"], json!("text"));
}

// ---------------------------------------------------------------------------
// CLI: stage-candidates .jsonl shorthand → prepare → apply --classifications
// ---------------------------------------------------------------------------

#[test]
fn cli_stage_prepare_apply_cycle() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let r = dir.path().to_str().unwrap();

    let (code, v) = run_json(&[
        "--root", r, "ingest", "--text", "beta doc", "--text-ref", "doc-b",
    ]);
    assert_eq!(code, 0, "{v}");
    let sid = v["source_id"].as_str().unwrap().to_string();

    let cands = dir.path().join("cands.jsonl");
    std::fs::write(
        &cands,
        "{\"subject\":\"beta.gamma\",\"predicate\":\"size\",\"value\":42,\"authority\":\"verified\"}\n",
    )
    .unwrap();

    let (code, v) = run_json(&[
        "--root", r, "stage-candidates",
        "--file", cands.to_str().unwrap(),
        "--source-id", &sid,
        "--source-version", "1",
    ]);
    assert_eq!(code, 0, "{v}");
    let run_id = v["run_id"].as_str().unwrap().to_string();
    assert_eq!(v["staged"], 1);

    let (code, prep) = run_json(&["--root", r, "reconcile-prepare", "--run", &run_id]);
    assert_eq!(code, 0, "{prep}");
    assert_eq!(prep["comparisons"][0]["auto"], json!("UNRELATED"));

    let cls = dir.path().join("cls.json");
    std::fs::write(&cls, "[]").unwrap();
    let (code, v) = run_json(&[
        "--root", r, "reconcile-apply",
        "--run", &run_id,
        "--classifications", cls.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["changes"]["claims_created"], 1, "{v}");
    let (code, plan) = run_json(&["--root", r, "compile-plan"]);
    assert_eq!(code, 0);
    assert_eq!(plan["pending"], json!([]));
}

// ---------------------------------------------------------------------------
// classification shape mismatch → Txn (exit 3)
// ---------------------------------------------------------------------------

#[test]
fn cli_classification_mismatch_exits_3() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let r = dir.path().to_str().unwrap();
    let (code, v) = run_json(&[
        "--root", r, "ingest", "--text", "doc", "--text-ref", "d",
    ]);
    assert_eq!(code, 0, "{v}");
    let sid = v["source_id"].as_str().unwrap().to_string();
    let cands = dir.path().join("c.jsonl");
    std::fs::write(&cands, "{\"subject\":\"s\",\"predicate\":\"p\",\"value\":1}\n").unwrap();
    let (code, v) = run_json(&[
        "--root", r, "stage-candidates",
        "--file", cands.to_str().unwrap(),
        "--source-id", &sid, "--source-version", "1",
    ]);
    assert_eq!(code, 0, "{v}");
    let run_id = v["run_id"].as_str().unwrap().to_string();

    for bad in [
        "[{\"index\":0,\"relationship\":\"NOT_A_REL\"}]",
        "[{\"index\":9,\"relationship\":\"UNRELATED\"}]",
        "[{\"index\":0,\"relationship\":\"DUPLICATE\"}]",
    ] {
        let cls = dir.path().join("bad.json");
        std::fs::write(&cls, bad).unwrap();
        let (code, v) = run_json(&[
            "--root", r, "reconcile-apply",
            "--run", &run_id,
            "--classifications", cls.to_str().unwrap(),
        ]);
        assert_eq!(code, 3, "{bad} → {v}");
        assert_eq!(v["error_type"], json!("txn"), "{v}");
    }
}

// ---------------------------------------------------------------------------
// prepare→apply freshness: matched claim changed mid-window → Conflict (4)
// ---------------------------------------------------------------------------

#[test]
fn cli_fresh_violation_exits_4() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let r = dir.path().to_str().unwrap();

    // first cycle: claim alpha.beta/status=active exists
    let (code, v) = run_json(&[
        "--root", r, "ingest", "--text", "doc1", "--text-ref", "d1",
    ]);
    assert_eq!(code, 0, "{v}");
    let sid1 = v["source_id"].as_str().unwrap().to_string();
    let c1 = dir.path().join("c1.jsonl");
    std::fs::write(&c1, "{\"subject\":\"alpha.beta\",\"predicate\":\"status\",\"value\":\"active\"}\n").unwrap();
    let (code, v) = run_json(&[
        "--root", r, "stage-candidates", "--file", c1.to_str().unwrap(),
        "--source-id", &sid1, "--source-version", "1",
    ]);
    assert_eq!(code, 0, "{v}");
    let run1 = v["run_id"].as_str().unwrap().to_string();
    let (code, _) = run_json(&["--root", r, "reconcile-prepare", "--run", &run1]);
    assert_eq!(code, 0);
    let cls = dir.path().join("empty.json");
    std::fs::write(&cls, "[]").unwrap();
    let (code, v) = run_json(&[
        "--root", r, "reconcile-apply", "--run", &run1,
        "--classifications", cls.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");

    let dot = dir.path().join(".llm-wiki");
    let claim_path = dot.join("claims").join(
        std::fs::read_dir(dot.join("claims"))
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .file_name(),
    );
    let cid = claim_path
        .file_stem()
        .unwrap()
        .to_string_lossy()
        .into_owned();

    // second source, same-subject candidate → matched claim
    let (code, v) = run_json(&[
        "--root", r, "ingest", "--text", "doc2", "--text-ref", "d2",
    ]);
    assert_eq!(code, 0, "{v}");
    let sid2 = v["source_id"].as_str().unwrap().to_string();
    let c2 = dir.path().join("c2.jsonl");
    std::fs::write(&c2, "{\"subject\":\"alpha.beta\",\"predicate\":\"status\",\"value\":\"retired\"}\n").unwrap();
    let (code, v) = run_json(&[
        "--root", r, "stage-candidates", "--file", c2.to_str().unwrap(),
        "--source-id", &sid2, "--source-version", "1",
    ]);
    assert_eq!(code, 0, "{v}");
    let run2 = v["run_id"].as_str().unwrap().to_string();
    let (code, prep) = run_json(&["--root", r, "reconcile-prepare", "--run", &run2]);
    assert_eq!(code, 0, "{prep}");
    assert_eq!(
        prep["comparisons"][0]["matches"][0]["claim_id"], json!(cid),
        "{prep}"
    );

    // mid-window external mutation → stamps.json catches it (§5.2)
    let mut bytes = std::fs::read(&claim_path).unwrap();
    bytes.push(b' ');
    std::fs::write(&claim_path, &bytes).unwrap();

    let cls = dir.path().join("corr.json");
    std::fs::write(
        &cls,
        &serde_json::to_string(&json!([{
            "index": 0,
            "relationship": "CORRECTION",
            "target_claim_id": cid,
        }]))
        .unwrap(),
    )
    .unwrap();
    let (code, v) = run_json(&[
        "--root", r, "reconcile-apply", "--run", &run2,
        "--classifications", cls.to_str().unwrap(),
    ]);
    assert_eq!(code, 4, "{v}");
    assert_eq!(v["error_type"], json!("conflict"), "{v}");
}

// ---------------------------------------------------------------------------
// §13 dual-engine: a hand-written Python-shaped comparisons.json (no
// truncated_matches, no stamps.json) feeds reconcile_apply — version-
// snapshot check only (honest degradation).
// ---------------------------------------------------------------------------

#[test]
fn python_shaped_comparisons_feed_rust_apply() {
    let (_d, wiki) = common::wiki();
    let (sid1, v1) = ingest_text_lib(&wiki, "doc one", "d1");
    let run1 = stage_shorthand(&wiki, &sid1, v1, "gamma.delta", "mode", "on");
    compile::reconcile_prepare(&wiki, &run1).unwrap();
    compile::reconcile_apply(&wiki, &run1, vec![], wiki.revision().unwrap(), None, None, false)
        .unwrap();
    let target = &claims_on_disk(&wiki)[0];
    let cid = target["id"].as_str().unwrap().to_string();
    assert_eq!(target["version"], 1);

    let (sid2, v2) = ingest_text_lib(&wiki, "doc two", "d2");
    let run2 = stage_shorthand(&wiki, &sid2, v2, "gamma.delta", "mode", "on");
    let rundir = wiki.p(&format!(".state/staging/{run2}"));

    // Python-shaped comparisons.json: no truncated_matches sibling,
    // no stamps.json sidecar at all.
    let python_shape = json!({
        "run_id": run2,
        "base_revision": wiki.revision().unwrap(),
        "comparisons": [{
            "index": 0,
            "key": "gamma.delta/mode",
            "candidate_value": "on",
            "scope": {},
            "authority": {"type": "authoritative_source", "source": "document"},
            "valid_from": null,
            "matches": [{
                "claim_id": cid,
                "same_scope": true,
                "same_value": true,
                "temporal": "overlapping",
                "status": target["status"].clone(),
                "current_value": "on",
                "version": 1,
                "root_origins": [sid1],
            }],
            "auto": null,
            "hint": "DUPLICATE/CORROBORATION",
        }],
    });
    std::fs::write(
        rundir.join("comparisons.json"),
        serde_json::to_string_pretty(&python_shape).unwrap(),
    )
    .unwrap();
    assert!(!rundir.join("stamps.json").is_file());

    let receipt = compile::reconcile_apply(
        &wiki,
        &run2,
        vec![json!({
            "index": 0,
            "relationship": "CORROBORATION",
            "target_claim_id": cid,
        })],
        wiki.revision().unwrap(),
        None,
        None,
        false,
    )
    .unwrap();
    assert_eq!(receipt["changes"]["claims_updated"], 1, "{receipt}");
    let t = &claims_on_disk(&wiki)[0];
    assert_eq!(t["version"], 2, "bumped once: {t}");
    assert_eq!(t["evidence"].as_array().unwrap().len(), 2, "{t}");
}

// ---------------------------------------------------------------------------
// --use-prefill: ≥0.7 auto_prefill fills unclassified candidates (§6.4)
// ---------------------------------------------------------------------------

#[test]
fn use_prefill_consumes_auto_prefill_blocks() {
    let (_d, wiki) = common::wiki();
    let (sid, ver) = ingest_text_lib(&wiki, "doc", "d");
    let run_id = stage_shorthand(&wiki, &sid, ver, "eps.zeta", "flag", "true");
    compile::reconcile_prepare(&wiki, &run_id).unwrap();
    let rundir = wiki.p(&format!(".state/staging/{run_id}"));
    let mut cmp: Value = serde_json::from_str(
        &std::fs::read_to_string(rundir.join("comparisons.json")).unwrap(),
    )
    .unwrap();
    cmp["comparisons"][0]["auto_prefill"] = json!({
        "verdict": "UNRELATED", "confidence": 0.9, "jev": true
    });
    std::fs::write(
        rundir.join("comparisons.json"),
        serde_json::to_string_pretty(&cmp).unwrap(),
    )
    .unwrap();

    let receipt = compile::reconcile_apply(
        &wiki,
        &run_id,
        vec![],
        wiki.revision().unwrap(),
        None,
        None,
        true, // --use-prefill
    )
    .unwrap();
    assert_eq!(receipt["changes"]["claims_created"], 1, "{receipt}");
    assert_eq!(receipt["auto"]["prefill"], 1, "{receipt}");

    // without the flag the same unclassified candidate still falls back
    // to the engine auto-verdict — both paths converge for UNRELATED.
    let (sid2, ver2) = ingest_text_lib(&wiki, "doc2", "d2");
    let run2 = stage_shorthand(&wiki, &sid2, ver2, "eta.theta", "n", "0");
    compile::reconcile_prepare(&wiki, &run2).unwrap();
    let receipt2 = compile::reconcile_apply(
        &wiki, &run2, vec![], wiki.revision().unwrap(), None, None, false,
    )
    .unwrap();
    assert_eq!(receipt2["changes"]["claims_created"], 1, "{receipt2}");
}

// ---------------------------------------------------------------------------
// umbrella is idempotent over a parked run: second extract-phase call
// re-adopts the same run (resumed_run:true)
// ---------------------------------------------------------------------------

#[test]
fn compile_extract_is_idempotent() {
    let (_d, wiki) = common::wiki();
    let (_sid, _v) = ingest_text_lib(&wiki, "doc", "d");
    let p1 = compile::compile_umbrella(&wiki, false).unwrap();
    assert_eq!(p1["phase"], json!("extract"));
    let rid = p1["pending"][0]["run_id"].as_str().unwrap().to_string();
    let p2 = compile::compile_umbrella(&wiki, false).unwrap();
    assert_eq!(p2["pending"][0]["run_id"], json!(rid));
    assert_eq!(p2["pending"][0]["resumed_run"], json!(true));
    // no duplicate staging dir
    let staging = wiki.p(".state/staging");
    let n = std::fs::read_dir(&staging).unwrap().count();
    assert_eq!(n, 1);
}
