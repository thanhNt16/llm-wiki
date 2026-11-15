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
    let v: Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout not JSON ({e}): {}",
            String::from_utf8_lossy(&out.stdout)
        )
    });
    (code, v)
}

const RSS: &str = r#"<?xml version="1.0"?>
<rss version="2.0"><channel>
  <title>example</title>
  <item><title>First post</title>
    <link>https://ex/1</link><guid>g1</guid>
    <pubDate>Tue, 01 Jan 2030</pubDate>
    <description>hello world</description>
  </item>
  <item><title>Second post</title>
    <link>https://ex/2</link><guid>g2</guid>
    <description><![CDATA[<p>raw</p> here]]></description>
  </item>
</channel></rss>"#;

#[test]
fn ingest_list_connectors() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let (code, v) = run_json(&[
        "--root",
        dir.path().to_str().unwrap(),
        "ingest",
        "--list-connectors",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(
        v["connectors"].as_array().unwrap(),
        &vec![Value::from("github-issues"), Value::from("rest")],
        "{v}"
    );
}

#[test]
fn ingest_connector_requires_ref() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let (code, v) = run_json(&[
        "--root",
        dir.path().to_str().unwrap(),
        "ingest",
        "--connector",
        "rest",
    ]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["error_type"], "txn");
    assert!(
        v["error"].as_str().unwrap().contains("--connector requires --ref"),
        "{v}"
    );
}

#[test]
fn ingest_connector_unknown_preset() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let (code, v) = run_json(&[
        "--root",
        dir.path().to_str().unwrap(),
        "ingest",
        "--connector",
        "nope",
        "--ref",
        "https://api.example.com/x",
    ]);
    assert_eq!(code, 3, "{v}");
    let err = v["error"].as_str().unwrap();
    assert!(err.contains("unknown connector preset"), "{v}");
    assert!(err.contains("github-issues"), "{v}");
}

#[test]
fn ingest_connector_rejects_non_http_ref() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let feed = dir.path().join("data.json");
    std::fs::write(&feed, r#"{"items":[]}"#).unwrap();
    let (code, v) = run_json(&[
        "--root",
        dir.path().to_str().unwrap(),
        "ingest",
        "--connector",
        "rest",
        "--ref",
        feed.to_str().unwrap(),
    ]);
    assert_eq!(code, 3, "{v}");
    assert!(
        v["error"].as_str().unwrap().contains("only http/https"),
        "{v}"
    );
}

#[test]
fn ingest_rss_file_ingests_items() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let feed = dir.path().join("feed.xml");
    std::fs::write(&feed, RSS).unwrap();

    let (code, v) = run_json(&[
        "--root",
        dir.path().to_str().unwrap(),
        "ingest-rss",
        "--file",
        feed.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["connector"], "feed");
    assert_eq!(v["items"], 2);
    assert_eq!(v["ingested"], 2);
    assert_eq!(v["deduplicated"], 0);
    assert_eq!(v["sources"].as_array().unwrap().len(), 2);
    // file ref is the absolute path
    assert!(v["feed"].as_str().unwrap().ends_with("feed.xml"), "{v}");

    // re-run dedups per item
    let (code, v) = run_json(&[
        "--root",
        dir.path().to_str().unwrap(),
        "ingest-rss",
        "--file",
        feed.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["deduplicated"], 2);
    assert_eq!(v["ingested"], 0);
}

#[test]
fn ingest_rss_limit() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let feed = dir.path().join("feed.xml");
    std::fs::write(&feed, RSS).unwrap();
    let (code, v) = run_json(&[
        "--root",
        dir.path().to_str().unwrap(),
        "ingest-rss",
        "--file",
        feed.to_str().unwrap(),
        "--limit",
        "1",
    ]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["items"], 1);
}

#[test]
fn ingest_rss_malformed_is_txn() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let feed = dir.path().join("bad.xml");
    std::fs::write(&feed, "<rss><channel><item>").unwrap();
    let (code, v) = run_json(&[
        "--root",
        dir.path().to_str().unwrap(),
        "ingest-rss",
        "--file",
        feed.to_str().unwrap(),
    ]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["error_type"], "txn");
    assert!(v["error"].as_str().unwrap().contains("malformed XML"), "{v}");
}

#[test]
fn ingest_rss_requires_input() {
    let dir = tempfile::tempdir().unwrap();
    init(dir.path());
    let (code, v) = run_json(&[
        "--root",
        dir.path().to_str().unwrap(),
        "ingest-rss",
    ]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["error_type"], "txn");
}
