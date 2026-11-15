use std::process::Command;

use serde_json::Value;

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_llm-wiki"))
}

fn init(dir: &std::path::Path) {
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

fn sha256_file(p: &std::path::Path) -> String {
    llm_wiki::util::sha256_hex(&std::fs::read(p).unwrap())
}

#[test]
fn ingest_text_file_creates_source_and_raw() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let doc = dir.path().join("doc.txt");
    std::fs::write(&doc, b"hello deterministic world").unwrap();

    let (code, v) = run_json(&["--root", dir.path().to_str().unwrap(), "ingest", "--file", doc.to_str().unwrap()]);
    assert_eq!(code, 0, "{v}");
    let sid = v["source_id"].as_str().unwrap().to_string();
    assert!(sid.starts_with("source_"), "{v}");
    assert_eq!(v["version"], 1);
    assert_eq!(v["deduplicated"], false);
    assert_eq!(v["sha256"].as_str().unwrap(), sha256_file(&doc));

    let dot = dir.path().join(".llm-wiki");
    let raw_rel = v["raw_path"].as_str().unwrap().to_string();
    assert!(raw_rel.starts_with("raw/files/"), "{raw_rel}");
    assert!(dot.join(&raw_rel).is_file());
    assert_eq!(std::fs::read(dot.join(&raw_rel)).unwrap(), b"hello deterministic world");
    assert!(dot.join(format!("sources/{sid}/manifest.json")).is_file());
    assert!(dot.join(format!("sources/{sid}/content.md")).is_file());
    assert_eq!(
        std::fs::read_to_string(dot.join(format!("sources/{sid}/content.md"))).unwrap(),
        "hello deterministic world"
    );
    assert!(dot.join(format!("sources/{sid}/extraction.json")).is_file());
}

#[test]
fn ingest_inline_text() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let (code, v) = run_json(&[
        "--root", dir.path().to_str().unwrap(), "ingest",
        "--text", "inline note body", "--text-ref", "notes/first",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["version"], 1);
    assert!(v["raw_path"].as_str().unwrap().starts_with("raw/text/"), "{v}");
    let sid = v["source_id"].as_str().unwrap();
    let dot = dir.path().join(".llm-wiki");
    assert_eq!(
        std::fs::read_to_string(dot.join(format!("sources/{sid}/content.md"))).unwrap(),
        "inline note body"
    );
}

#[test]
fn ingest_dedup_by_content() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let doc = dir.path().join("doc.txt");
    std::fs::write(&doc, b"same content").unwrap();
    let r = dir.path().to_str().unwrap();

    let (_, v1) = run_json(&["--root", r, "ingest", "--file", doc.to_str().unwrap()]);
    let (_, v2) = run_json(&["--root", r, "ingest", "--file", doc.to_str().unwrap()]);
    assert_eq!(v2["deduplicated"], true, "{v2}");
    assert_eq!(v2["source_id"], v1["source_id"]);
    assert_eq!(v2["version"], 1);
}

#[test]
fn ingest_changed_content_bumps_version() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let doc = dir.path().join("doc.txt");
    std::fs::write(&doc, b"version one").unwrap();
    let r = dir.path().to_str().unwrap();

    let (_, v1) = run_json(&["--root", r, "ingest", "--file", doc.to_str().unwrap()]);
    std::fs::write(&doc, "version two — changed".as_bytes()).unwrap();
    let (code, v2) = run_json(&["--root", r, "ingest", "--file", doc.to_str().unwrap()]);
    assert_eq!(code, 0, "{v2}");
    assert_eq!(v2["deduplicated"], false);
    assert_eq!(v2["version"], 2);
    assert_eq!(v2["source_id"], v1["source_id"]);

    let dot = dir.path().join(".llm-wiki");
    let sid = v1["source_id"].as_str().unwrap();
    assert!(dot.join(format!("sources/{sid}/content_v2.md")).is_file());
    assert_eq!(
        std::fs::read_to_string(dot.join(format!("sources/{sid}/content_v2.md"))).unwrap(),
        "version two — changed"
    );
}

fn last_receipt(root: &std::path::Path) -> Value {
    let text = std::fs::read_to_string(root.join(".llm-wiki/.state/operations.jsonl"))
        .expect("operations.jsonl readable");
    let line = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .next_back()
        .expect("operations.jsonl has a receipt");
    serde_json::from_str(line).unwrap()
}

#[test]
fn ingest_receipts_carry_command_and_changes() {
    // python parity (sources.py:182/217): ingest receipts in
    // .state/operations.jsonl must record command "wiki-ingest" and
    // changes {sources_registered, versions_added} — they feed
    // wiki/log.md and wiki/changes/index.md verbatim.
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let root = dir.path();
    let doc = root.join("notes.txt");
    std::fs::write(&doc, b"v1 content").unwrap();

    let (code, v1) = run_json(&["--root", root.to_str().unwrap(), "ingest", "--file", doc.to_str().unwrap()]);
    assert_eq!(code, 0, "{v1}");
    let r1 = last_receipt(root);
    assert_eq!(r1["command"], "wiki-ingest", "{r1}");
    assert_eq!(r1["changes"]["sources_registered"], 1, "{r1}");
    assert_eq!(r1["changes"]["versions_added"], 1, "{r1}");

    std::fs::write(&doc, b"v2 content").unwrap();
    let (code, v2) = run_json(&["--root", root.to_str().unwrap(), "ingest", "--file", doc.to_str().unwrap()]);
    assert_eq!(code, 0, "{v2}");
    assert_eq!(v2["version"], 2, "{v2}");
    assert_eq!(v2["source_id"], v1["source_id"], "{v2}");
    let r2 = last_receipt(root);
    assert_eq!(r2["command"], "wiki-ingest", "{r2}");
    assert_eq!(r2["changes"]["sources_registered"], 0, "{r2}");
    assert_eq!(r2["changes"]["versions_added"], 1, "{r2}");
}

#[test]
fn ingest_office_binary_goes_pending_or_markitdown() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    // minimal fake docx (zip magic bytes, not UTF-8 text-like content)
    let docx = dir.path().join("report.docx");
    let mut blob = vec![0x50u8, 0x4B, 0x03, 0x04];
    blob.extend(std::iter::repeat(0xAAu8).take(4096));
    std::fs::write(&docx, &blob).unwrap();

    let (code, v) = run_json(&[
        "--root", dir.path().to_str().unwrap(), "ingest", "--file", docx.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    let text_cov = v["coverage"]["text"].as_str().unwrap();
    if text_cov == "not_extracted" {
        // no markitdown on PATH (or probe failed): raw preserved, warning issued
        assert!(v["warnings"].as_array().unwrap().iter().any(|w| {
            w.as_str().unwrap().contains("not semantically extracted")
        }), "{v}");
    } else {
        assert_eq!(text_cov, "partial", "{v}");
        assert_eq!(v["normalized_source"], "markitdown", "{v}");
    }
    // raw preserved either way; version pending until compiled
    let dot = dir.path().join(".llm-wiki");
    assert!(dot.join(v["raw_path"].as_str().unwrap()).is_file());
    assert_eq!(std::fs::read(dot.join(v["raw_path"].as_str().unwrap())).unwrap(), blob);
}

#[test]
fn ingest_normalized_content_accept_lane() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let doc = dir.path().join("scan.pdf");
    std::fs::write(&doc, &[0x25u8, 0x50, 0x44, 0x46, 0x00, 0x01, 0x02, 0x03]).unwrap();
    let norm = dir.path().join("norm.md");
    std::fs::write(&norm, "# extracted\n\nagent vision output").unwrap();
    let r = dir.path().to_str().unwrap();

    let (code, v) = run_json(&[
        "--root", r, "ingest", "--file", doc.to_str().unwrap(),
        "--normalized-content", norm.to_str().unwrap(),
        "--parser-name", "agent-vision",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["normalized_source"], "agent");
    assert_eq!(v["parser"]["name"], "agent-vision");
    let sid = v["source_id"].as_str().unwrap();
    let dot = dir.path().join(".llm-wiki");
    assert_eq!(
        std::fs::read_to_string(dot.join(format!("sources/{sid}/content.md"))).unwrap(),
        "# extracted\n\nagent vision output"
    );
}

#[test]
fn ingest_normalized_fill_on_dedup_hit() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let doc = dir.path().join("scan.pdf");
    std::fs::write(&doc, &[0x25u8, 0x50, 0x44, 0x46, 0x00, 0x01, 0x02, 0x03]).unwrap();
    let r = dir.path().to_str().unwrap();

    // first: raw-only ingest (pdf, no parser → not_extracted)
    let (_, v1) = run_json(&["--root", r, "ingest", "--file", doc.to_str().unwrap()]);
    let sid = v1["source_id"].as_str().unwrap().to_string();
    // second: same bytes + normalized content → _fill_normalized lane
    let norm = dir.path().join("norm.md");
    std::fs::write(&norm, "filled text").unwrap();
    let (code, v2) = run_json(&[
        "--root", r, "ingest", "--file", doc.to_str().unwrap(),
        "--normalized-content", norm.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v2}");
    assert_eq!(v2["normalized_status"], "filled", "{v2}");
    assert_eq!(v2["version"], 1);
    let dot = dir.path().join(".llm-wiki");
    assert_eq!(
        std::fs::read_to_string(dot.join(format!("sources/{sid}/content.md"))).unwrap(),
        "filled text"
    );
    // extraction coverage updated to complete
    let ex: Value = serde_json::from_str(
        &std::fs::read_to_string(dot.join(format!("sources/{sid}/extraction.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(ex["extraction_report"]["coverage"]["text"], "complete");
}

#[test]
fn ingest_list_connectors_reports_presets() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let r = dir.path().to_str().unwrap();

    let (code, v) = run_json(&["--root", r, "ingest", "--list-connectors"]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(
        v["connectors"].as_array().unwrap(),
        &vec![serde_json::json!("github-issues"), serde_json::json!("rest")],
        "{v}"
    );
}

#[test]
fn ingest_requires_initialized_wiki() {
    let dir = tempfile::tempdir().unwrap();
    let doc = dir.path().join("doc.txt");
    std::fs::write(&doc, b"x").unwrap();
    let (code, v) = run_json(&[
        "--root", dir.path().to_str().unwrap(), "ingest", "--file", doc.to_str().unwrap(),
    ]);
    assert_eq!(code, 3, "{v}");
    assert!(v["error"].as_str().unwrap().contains("not initialized"), "{v}");
}
