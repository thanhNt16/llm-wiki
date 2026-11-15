//! Secrets policy — secrets.rs port fidelity (scan counts, .env boost,
//! redact) plus the `_secret_gate` policy lane end-to-end through the CLI:
//! warn / deny / redact / allow per `secret_policy` in wiki.json
//! (sources.py:103-114 parity). Contract: secret VALUES never appear in
//! receipts, warnings, or error payloads — kinds + counts only.

mod common;

use std::path::Path;
use std::process::Command;

use llm_wiki::secrets;
use serde_json::{json, Value};
use tempfile::TempDir;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_llm-wiki"))
}

fn run_json(args: &[&str]) -> (i32, Value) {
    let out = bin().args(args).output().unwrap();
    let code = out.status.code().unwrap_or(-1);
    let v: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{args:?} stdout not JSON ({e}): {:?}", out.stdout));
    (code, v)
}

/// Overwrite `.llm-wiki/wiki.json`'s `secret_policy` (the gate reads
/// `wiki.config()` each ingest).
fn set_policy(wiki_root: &Path, policy: &str) {
    let p = wiki_root.join(".llm-wiki/wiki.json");
    let mut cfg: Value =
        serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    cfg["secret_policy"] = json!(policy);
    std::fs::write(&p, serde_json::to_string_pretty(&cfg).unwrap()).unwrap();
}

fn count_of(findings: &[Value], kind: &str) -> i64 {
    findings
        .iter()
        .find(|f| f["kind"].as_str() == Some(kind))
        .map(|f| f["count"].as_i64().unwrap())
        .unwrap_or(0)
}

const AWS: &str = "AKIAIOSFODNN7EXAMPLE";
const GHP: &str = "ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZabcd"; // 30 chars after prefix
const GH_PAT: &str = "github_pat_ABCDEFGHIJKLMNOPQRSTUV"; // 22+ after prefix
const XOXB: &str = "xoxb-1234567890-abcdefghij";
const XAPP: &str = "xapp-1234567890-ab";
const JWT: &str = "eyJhbGciOiJIUzI1NiIs.eyJzdWIiOiIxMjM0NTY3ODkwI.SflKxwRJSMeKKF2QT4";
const SK_GENERIC: &str = "sk-abcdefghijklmnopqrstuv"; // 24 chars after sk-
const SK_LIVE: &str = "sk-live-abcdefghijklmnop";
const URL_CRED: &str = "postgresql://user:p4ssw0rd@db.internal:5432/app";

fn gcp_key() -> String {
    format!("AIza{}", "x".repeat(35))
}

// ---------- scan(): every pattern kind, counts only ----------

#[test]
fn scan_reports_each_pattern_kind() {
    let cases: Vec<(&str, String)> = vec![
        ("private_key", "-----BEGIN RSA PRIVATE KEY-----".into()),
        ("aws_key", AWS.into()),
        ("github_token", GHP.into()),
        ("github_token", GH_PAT.into()),
        ("slack_token", XOXB.into()),
        ("slack_token", XAPP.into()),
        ("google_api_key", gcp_key()),
        ("api_key", SK_LIVE.into()),
        ("api_key", SK_GENERIC.into()),
        ("jwt", JWT.into()),
        ("url_credentials", URL_CRED.into()),
        ("credential_assignment", "passwd = hunter2secret".into()),
    ];
    for (kind, text) in &cases {
        let findings = secrets::scan(text, None);
        assert_eq!(
            findings.len(),
            1,
            "{kind}: expected exactly one merged kind in {text:?}, got {findings:?}"
        );
        assert_eq!(findings[0].kind, *kind, "{text:?}");
        assert_eq!(findings[0].count, 1, "{text:?}");
    }
}

#[test]
fn scan_merges_regexes_sharing_a_kind() {
    // github_token + slack_token each own two patterns — merged to one
    // count per kind, first-seen order preserved (secrets.py _merge_counts).
    let text = format!("{GHP} {GH_PAT} {XOXB} {XAPP}");
    let findings = secrets::scan(&text, None);
    let map: Vec<(String, usize)> =
        findings.iter().map(|f| (f.kind.clone(), f.count)).collect();
    assert_eq!(
        map,
        vec![
            ("github_token".to_string(), 2),
            ("slack_token".to_string(), 2),
        ]
    );
}

#[test]
fn scan_clean_text_is_empty() {
    assert!(secrets::scan("just prose, nothing sensitive", None).is_empty());
}

// ---------- .env filename boost ----------

#[test]
fn env_filename_adds_env_file_finding() {
    for name in [
        ".env",
        ".env.local",
        ".ENV",
        "dir/.env.production",
        "sub\\dir\\.env",
    ] {
        let findings = secrets::scan("KEY=value", Some(name));
        assert!(
            findings.iter().any(|f| f.kind == "env_file" && f.count == 1),
            "{name}: expected env_file finding, got {findings:?}"
        );
    }
    // even an empty .env file is flagged — the filename alone is the signal
    let findings = secrets::scan("", Some(".env"));
    assert_eq!(findings.len(), 1);
    assert_eq!(findings[0].kind, "env_file");
    // lookalikes are not .env files
    for name in ["env", "x.env", ".envrc", ".envx", "foo.env"] {
        let findings = secrets::scan("KEY=value", Some(name));
        assert!(
            !findings.iter().any(|f| f.kind == "env_file"),
            "{name}: unexpected env_file finding {findings:?}"
        );
    }
    // no filename → no boost
    assert!(!secrets::scan("x", None).iter().any(|f| f.kind == "env_file"));
}

// ---------- redact(): value removal, separator preservation, no double-count ----------

#[test]
fn redact_removes_values_preserves_context() {
    let (out, n) = secrets::redact(&format!("token = {AWS}"));
    assert!(!out.contains(AWS), "{out}");
    assert!(out.contains("REDACTED"), "{out}");
    assert!(out.contains("token = "), "{out}: separator not preserved");
    // aws_key bumps once; credential_assignment sees group(3) ==
    // "REDACTED" and skips — total 1, not 2 (secrets.py ordering contract).
    assert_eq!(n, 1, "{out}");
}

#[test]
fn redact_counts_every_secret() {
    let text = format!("{GHP} and {SK_GENERIC} and password: s3cr3tval");
    let (out, n) = secrets::redact(&text);
    assert_eq!(n, 3, "{out}");
    for s in [GHP, SK_GENERIC, "s3cr3tval"] {
        assert!(!out.contains(s), "{out} still contains {s}");
    }
    assert_eq!(out.matches("REDACTED").count(), 3, "{out}");
    // idempotent: a second pass finds nothing left to redact
    let (_, n2) = secrets::redact(&out);
    assert_eq!(n2, 0);
}

#[test]
fn redact_clean_text_unchanged() {
    let (out, n) = secrets::redact("nothing here");
    assert_eq!(out, "nothing here");
    assert_eq!(n, 0);
}

// ---------- CLI e2e: policy lane ----------

fn ingest_text(dir: &TempDir, text: &str) -> (i32, String, Value) {
    let out = bin()
        .args([
            "--root",
            dir.path().to_str().unwrap(),
            "ingest",
            "--text",
            text,
        ])
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let v: Value = serde_json::from_str(&stdout)
        .unwrap_or_else(|e| panic!("stdout not JSON ({e}): {stdout}"));
    (out.status.code().unwrap_or(-1), stdout, v)
}

#[test]
fn warn_policy_stores_as_is_reports_counts_no_values() {
    let (dir, _wiki) = common::wiki();
    let text = format!("config note: aws {AWS}");
    let (code, stdout, v) = ingest_text(&dir, &text);
    assert_eq!(code, 0, "{v}");
    let findings = v["secrets"]["findings"].as_array().unwrap();
    assert!(
        findings
            .iter()
            .any(|f| f["kind"] == "aws_key" && f["count"] == 1),
        "{v}"
    );
    assert_eq!(v["secrets"]["redacted"], 0, "{v}");
    let warnings: Vec<&str> = v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w.as_str())
        .collect();
    assert!(
        warnings.iter().any(|w| w.contains("kinds=aws_key")),
        "{v}"
    );
    // contract: receipt/warnings carry kinds+counts, never the value
    assert!(
        !stdout.contains(AWS),
        "secret value leaked into receipt: {stdout}"
    );
    // warn policy stores content as-is
    let content = std::fs::read_to_string(dir.path().join(format!(
        ".llm-wiki/{}",
        v["normalized_path"].as_str().unwrap()
    )))
    .unwrap();
    assert!(content.contains(AWS));
}

#[test]
fn deny_policy_exits_3_stores_nothing_no_value_in_error() {
    let (dir, _wiki) = common::wiki();
    set_policy(dir.path(), "deny");
    let text = format!("aws {AWS}");
    let (code, stdout, v) = ingest_text(&dir, &text);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["error_type"], "txn", "{v}");
    let err = v["error"].as_str().unwrap();
    assert!(err.contains("ingest denied"), "{v}");
    assert!(err.contains("aws_key"), "{v}");
    assert!(
        !stdout.contains(AWS),
        "secret value leaked into error: {stdout}"
    );
    let sources = dir.path().join(".llm-wiki/sources");
    let empty =
        !sources.exists() || std::fs::read_dir(&sources).unwrap().next().is_none();
    assert!(empty, "denied ingest left sources on disk");
}

#[test]
fn redact_policy_stores_redacted_content() {
    let (dir, _wiki) = common::wiki();
    set_policy(dir.path(), "redact");
    let text = format!("creds {AWS} and {GHP}");
    let (code, _stdout, v) = ingest_text(&dir, &text);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["secrets"]["redacted"], 2, "{v}");
    let findings = v["secrets"]["findings"].as_array().unwrap();
    assert_eq!(count_of(findings, "aws_key"), 1, "{v}");
    assert_eq!(count_of(findings, "github_token"), 1, "{v}");
    assert!(
        v["warnings"]
            .to_string()
            .contains("secrets redacted in normalized content (count=2)"),
        "{v}"
    );
    let content = std::fs::read_to_string(dir.path().join(format!(
        ".llm-wiki/{}",
        v["normalized_path"].as_str().unwrap()
    )))
    .unwrap();
    assert!(!content.contains(AWS), "{content}");
    assert!(!content.contains(GHP), "{content}");
    assert_eq!(content.matches("REDACTED").count(), 2, "{content}");
}

#[test]
fn allow_policy_stores_as_is_no_warning() {
    let (dir, _wiki) = common::wiki();
    set_policy(dir.path(), "allow");
    let text = format!("aws {AWS}");
    let (code, _stdout, v) = ingest_text(&dir, &text);
    assert_eq!(code, 0, "{v}");
    assert_eq!(
        count_of(v["secrets"]["findings"].as_array().unwrap(), "aws_key"),
        1,
        "{v}"
    );
    assert_eq!(v["secrets"]["redacted"], 0, "{v}");
    assert!(
        !v["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap_or("").contains("secret")),
        "{v}"
    );
}

#[test]
fn env_file_boost_through_cli() {
    let (dir, _wiki) = common::wiki();
    let env_path = dir.path().join(".env");
    std::fs::write(&env_path, "PLAIN=1\n").unwrap();
    let (code, v) = run_json(&[
        "--root",
        dir.path().to_str().unwrap(),
        "ingest",
        "--file",
        env_path.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    let findings = v["secrets"]["findings"].as_array().unwrap();
    assert!(
        findings
            .iter()
            .any(|f| f["kind"] == "env_file" && f["count"] == 1),
        "{v}"
    );
    // a non-.env filename gets no boost
    let txt = dir.path().join("notes.txt");
    std::fs::write(&txt, "PLAIN=1\n").unwrap();
    let (code, v) = run_json(&[
        "--root",
        dir.path().to_str().unwrap(),
        "ingest",
        "--file",
        txt.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    assert!(
        !v["secrets"]["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["kind"] == "env_file"),
        "{v}"
    );
}

#[test]
fn normalized_content_lane_is_gated_too() {
    // agent-supplied --normalized-content hits the same gate (sources.py:146)
    let (dir, _wiki) = common::wiki();
    set_policy(dir.path(), "deny");
    let md = dir.path().join("extract.md");
    std::fs::write(&md, format!("parsed text with {JWT}")).unwrap();
    let out = bin()
        .args([
            "--root",
            dir.path().to_str().unwrap(),
            "ingest",
            "--text",
            "raw bytes",
            "--normalized-content",
            md.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3), "{out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("jwt"), "{stdout}");
    assert!(!stdout.contains(JWT), "secret value leaked: {stdout}");
}
