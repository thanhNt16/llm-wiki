mod common;

use std::path::Path;
use std::process::Command;

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

/// `contextpack._est` — ceil(chars/4), minimum 1.
fn est(text: &str) -> i64 {
    (((text.chars().count() as i64) + 3) / 4).max(1)
}

fn claim_id(n: u64) -> String {
    format!("claim_{n:026}")
}

fn decision_id(n: u64) -> String {
    format!("decision_{n:026}")
}

/// Canonical claim file written directly (context-pack reads the
/// canonical scan — no txn needed for fixtures).
fn seed_claim(
    dir: &Path,
    id: &str,
    subject: &str,
    predicate: &str,
    value: Value,
    status: &str,
    authority: &str,
    evidence: Value,
) {
    let mut c = json!({
        "id": id,
        "subject": subject,
        "predicate": predicate,
        "value": value,
        "status": status,
        "authority": {"type": authority},
        "scope": {},
        "evidence": evidence,
        "supersedes": [],
        "recorded_at": "2026-01-01T00:00:00Z",
        "version": 1,
    });
    if status == "superseded" || status == "rejected" {
        c["acceptance"] = json!({"reason": "seed fixture"});
    }
    std::fs::write(
        dir.join(format!(".llm-wiki/claims/{id}.json")),
        serde_json::to_string_pretty(&c).unwrap(),
    )
    .unwrap();
}

fn seed_decision(dir: &Path, id: &str, title: &str, body: &str) {
    let d = json!({
        "id": id,
        "title": title,
        "status": "accepted",
        "decided_on": "2026-01-10",
        "recorded_at": "2026-01-01T00:00:00Z",
        "version": 1,
        "claims": [],
        "evidence": [],
        "body": body,
    });
    std::fs::write(
        dir.join(format!(".llm-wiki/decisions/{id}.json")),
        serde_json::to_string_pretty(&d).unwrap(),
    )
    .unwrap();
}

fn seed_source(dir: &Path, sid: &str) {
    let mdir = dir.join(format!(".llm-wiki/sources/{sid}"));
    std::fs::create_dir_all(&mdir).unwrap();
    std::fs::write(
        mdir.join("manifest.json"),
        serde_json::to_string(&json!({
            "id": sid,
            "title": "memo",
            "kind": "text",
            "origin": "text://memo",
            "versions": [{"version": 1}],
        }))
        .unwrap(),
    )
    .unwrap();
}

fn pack_text(dir: &Path, out: &Value) -> String {
    std::fs::read_to_string(
        dir.join(".llm-wiki").join(out["pack_path"].as_str().unwrap()),
    )
    .unwrap()
}

#[test]
fn pack_priority_sections_in_order() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let root = dir.path().to_str().unwrap().to_string();

    // constraint: accepted + auto-accept authority (PRD §19 priority 1)
    seed_claim(
        dir.path(),
        &claim_id(1),
        "security",
        "must_encrypt_pii",
        json!("required"),
        "accepted",
        "explicit_project_decision",
        json!([]),
    );
    // generic relevant claim
    seed_claim(
        dir.path(),
        &claim_id(2),
        "payments",
        "retention_days",
        json!(365),
        "candidate",
        "agent_inference",
        json!([]),
    );
    // superseded claim — excluded from every section's candidates
    seed_claim(
        dir.path(),
        &claim_id(3),
        "oldpolicy",
        "stale_thing",
        json!("gone"),
        "superseded",
        "agent_inference",
        json!([]),
    );
    // decision that overlaps the task so it lands in-section (Q5)
    seed_decision(
        dir.path(),
        &decision_id(1),
        "Payments retention policy",
        "We chose Postgres for durability.",
    );
    let (code, out) = run_json(&[
        "--root", &root, "context-pack", "--task", "encrypt payments retention",
    ]);
    assert_eq!(code, 0, "{out}");
    let pack = pack_text(dir.path(), &out);

    let c = pack.find("## Critical constraints").expect("constraints section");
    let d = pack.find("## Accepted decisions").expect("decisions section");
    let r = pack.find("## Relevant claims").expect("relevant section");
    assert!(c < d && d < r, "PRD §19 order violated:\n{pack}");

    assert!(pack.contains("security / must_encrypt_pii = required"));
    assert!(pack.contains("- Payments retention policy: We chose Postgres for durability."));
    assert!(pack.contains("payments / retention_days = 365"));
    // superseded claim is neither a constraint nor relevant
    assert!(!pack.contains("oldpolicy"), "superseded leaked:\n{pack}");

    let receipt = &out["receipt"];
    assert!(receipt["context_id"].as_str().unwrap().starts_with("context_"));
    assert!(receipt["budget_used"].is_number(), "{receipt}");
    let claims: Vec<&str> = receipt["claims"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(claims.iter().any(|c| c.ends_with("@1")), "{claims:?}");
    let decisions: Vec<&str> = receipt["decisions"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(decisions.iter().any(|d| d.ends_with("@1")), "{decisions:?}");

    // pack file is a committed artifact with registered deps
    let deps_file = dir.path().join(".llm-wiki/.state/dependencies.json");
    let deps: Value =
        serde_json::from_str(&std::fs::read_to_string(deps_file).unwrap()).unwrap();
    let edges = deps["edges"].as_object().unwrap();
    assert!(edges.contains_key(out["pack_path"].as_str().unwrap()), "{deps}");
}

#[test]
fn narrow_task_marks_offtopic_sections_omitted() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let root = dir.path().to_str().unwrap().to_string();
    seed_claim(
        dir.path(),
        &claim_id(1),
        "payments",
        "retention_days",
        json!(365),
        "accepted",
        "explicit_project_decision",
        json!([]),
    );
    seed_decision(
        dir.path(),
        &decision_id(1),
        "Use GraphQL",
        "totally unrelated choice",
    );
    let (code, out) = run_json(&[
        "--root", &root, "context-pack", "--task", "payment retention",
    ]);
    assert_eq!(code, 0, "{out}");
    let pack = pack_text(dir.path(), &out);
    assert!(pack.contains("## Omitted sections"), "{pack}");
    assert!(pack.contains("- Accepted decisions (off-topic)"), "{pack}");
    let omitted: Vec<&str> = out["receipt"]["omitted"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(
        omitted.iter().any(|o| *o == "Accepted decisions (off-topic)"),
        "{omitted:?}"
    );
}

#[test]
fn budget_clamp_drops_lowest_priority_first() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let root = dir.path().to_str().unwrap().to_string();
    for i in 0..15u64 {
        seed_claim(
            dir.path(),
            &claim_id(i + 1),
            &format!("topic{i}"),
            "detail",
            json!(i),
            "candidate",
            "agent_inference",
            json!([]),
        );
    }
    let (code, out) = run_json(&[
        "--root", &root, "context-pack", "--task", "topic3 detail",
        "--budget", "250",
    ]);
    assert_eq!(code, 0, "{out}");
    let receipt = &out["receipt"];
    let estimated = receipt["budget"]["estimated"].as_i64().unwrap();
    assert!(estimated <= 250, "estimated {estimated} > 250");
    // spec context_budget metric: budget_used mirrors estimated.
    assert_eq!(receipt["budget_used"].as_i64().unwrap(), estimated);
    let text = pack_text(dir.path(), &out);
    assert!(est(&text) <= 250, "pack est {} > 250", est(&text));
    // lowest-priority (last) section dropped first and recorded.
    let omitted: Vec<&str> = receipt["omitted"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert!(omitted.iter().any(|o| o.ends_with("(budget)")), "{omitted:?}");
}

#[test]
fn receipt_sources_from_claim_evidence() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let root = dir.path().to_str().unwrap().to_string();
    seed_source(dir.path(), "source_00000000000000000000000001");
    seed_claim(
        dir.path(),
        &claim_id(1),
        "db",
        "engine",
        json!("postgres"),
        "accepted",
        "agent_inference",
        json!([{"source_id": "source_00000000000000000000000001",
                "source_version": 1, "locator": {"type": "text", "value": "l1"}}]),
    );
    // broad task → supplementary evidence section lists the manifest.
    let (code, out) = run_json(&["--root", &root, "context-pack"]);
    assert_eq!(code, 0, "{out}");
    let pack = pack_text(dir.path(), &out);
    assert!(pack.contains("## Supplementary evidence"), "{pack}");
    let sources: Vec<&str> = out["receipt"]["sources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(sources, vec!["source_00000000000000000000000001"], "{sources:?}");
}

#[test]
fn print_pack_emits_pack_bytes_not_json() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let root = dir.path().to_str().unwrap().to_string();
    seed_claim(
        dir.path(),
        &claim_id(1),
        "db",
        "engine",
        json!("postgres"),
        "accepted",
        "explicit_project_decision",
        json!([]),
    );
    let out = bin()
        .args(["--root", &root, "context-pack", "--task", "db", "--print-pack"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let stdout = out.stdout;
    assert!(!stdout.starts_with(b"{"), "print-pack emitted JSON");
    let text = String::from_utf8(stdout).unwrap();
    assert!(text.starts_with("---\ntype: context-pack\ncontext: context_"));
    assert!(text.contains("# Context pack context_"));
    // Each invocation mints its own pack; derive this pack's id from the
    // emitted header and compare bytes: python `print(f.read())` = file + \n.
    let cid = text
        .lines()
        .find_map(|l| l.strip_prefix("context: context_"))
        .expect("frontmatter context id");
    let cid = format!("context_{cid}");
    let file_bytes =
        std::fs::read(dir.path().join(format!(".llm-wiki/context/{cid}.md"))).unwrap();
    let mut expected = file_bytes;
    expected.push(b'\n');
    assert_eq!(text.as_bytes(), expected.as_slice());
}

#[test]
fn empty_wiki_minimal_pack_byte_identical() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let root = dir.path().to_str().unwrap().to_string();
    let (code, out) = run_json(&["--root", &root, "context-pack"]);
    assert_eq!(code, 0, "{out}");
    let text = pack_text(dir.path(), &out);
    let cid = out["context_id"].as_str().unwrap();
    // Python-identical minimal pack: frontmatter + header, no sections.
    let receipt = &out["receipt"];
    let generated = receipt["generated_at"].as_str().unwrap();
    let expected = format!(
        "---\ntype: context-pack\ncontext: {cid}\ndeps: []\n---\n\n\
# Context pack {cid}\n\n\
- Task: change briefing\n\
- Generated: {generated}\n\
- Budget: 6000 (estimated tokens, hard)\n\
- Evidence reference: current state\n\n"
    );
    assert_eq!(text, expected, "minimal pack bytes diverge");
    assert_eq!(receipt["budget"]["estimated"], est(&text));
    assert_eq!(receipt["budget_used"], est(&text));
    assert_eq!(receipt["omitted"], json!([]));
}

#[test]
fn oversized_budget_adds_no_padding() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let root = dir.path().to_str().unwrap().to_string();
    seed_claim(
        dir.path(),
        &claim_id(1),
        "db",
        "engine",
        json!("postgres"),
        "accepted",
        "explicit_project_decision",
        json!([]),
    );
    let (code, out) = run_json(&[
        "--root", &root, "context-pack", "--task", "db",
        "--budget", "100000",
    ]);
    assert_eq!(code, 0, "{out}");
    let text = pack_text(dir.path(), &out);
    let estimated = out["receipt"]["budget"]["estimated"].as_i64().unwrap();
    assert_eq!(estimated, est(&text));
    assert_eq!(out["receipt"]["budget_used"], estimated);
    // nothing dropped, nothing stretched: natural python tail is the last
    // section line + the file's closing newlines only.
    assert_eq!(out["receipt"]["omitted"], json!([]));
    assert!(text.ends_with('\n'));
    assert!(!text.ends_with("\n\n\n"), "pack was padded:\n{text:?}");
}

#[test]
fn budget_zero_is_a_txn_error() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let root = dir.path().to_str().unwrap().to_string();
    let (code, v) = run_json(&["--root", &root, "context-pack", "--budget", "0"]);
    assert_eq!(code, 3, "{v}");
    assert!(v["error"].as_str().unwrap().contains("budget"), "{v}");
}

#[test]
fn changes_since_timestamp_diffs_operations() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let root = dir.path().to_str().unwrap().to_string();
    seed_claim(
        dir.path(),
        &claim_id(1),
        "db",
        "engine",
        json!("postgres"),
        "accepted",
        "explicit_project_decision",
        json!([]),
    );
    // A committed pack gives operations.jsonl one real finished_at line.
    let (code, _first) = run_json(&["--root", &root, "context-pack", "--task", "db"]);
    assert_eq!(code, 0, "{_first}");
    let (code, out) = run_json(&[
        "--root", &root, "context-pack",
        "--changes-since", "2000-01-01T00:00:00Z",
    ]);
    assert_eq!(code, 0, "{out}");
    let pack = pack_text(dir.path(), &out);
    assert!(pack.contains("## Change briefing"), "{pack}");
    assert!(pack.contains("db / engine"), "{pack}");
    let (code, out) = run_json(&[
        "--root", &root, "context-pack",
        "--changes-since", "2999-01-01",
    ]);
    assert_eq!(code, 0, "{out}");
    let pack = pack_text(dir.path(), &out);
    assert!(pack.contains("No committed wiki operations"), "{pack}");
    assert!(pack.contains("2999-01-01"), "{pack}");
}
