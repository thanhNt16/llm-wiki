mod common;

use std::path::{Path, PathBuf};
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

fn write_claim(root: &Path, id: &str, subject: &str, recorded_at: &str) {
    let claim = json!({
        "id": id,
        "subject": subject,
        "predicate": "status",
        "value": "ok",
        "status": "accepted",
        "version": 1,
        "recorded_at": recorded_at,
        "evidence": [{"source_id": "source_s1"}],
    });
    std::fs::write(
        root.join(format!(".llm-wiki/claims/{id}.json")),
        serde_json::to_string(&claim).unwrap(),
    )
    .unwrap();
}

fn tree_snapshot(dir: &Path) -> Vec<(PathBuf, String)> {
    let mut out = Vec::new();
    fn walk(d: &Path, base: &Path, out: &mut Vec<(PathBuf, String)>) {
        for e in std::fs::read_dir(d).unwrap() {
            let e = e.unwrap();
            let p = e.path();
            if p.is_dir() {
                walk(&p, base, out);
            } else {
                out.push((
                    p.strip_prefix(base).unwrap().to_path_buf(),
                    std::fs::read_to_string(&p).unwrap(),
                ));
            }
        }
    }
    walk(dir, dir, &mut out);
    out.sort();
    out
}

#[test]
fn export_okf_layout_and_determinism() {
    let (dir, _wiki) = common::wiki();
    let root = dir.path();
    write_claim(root, "claim_e1", "Feature.Flags", "2026-01-02T03:04:05Z");
    write_claim(root, "claim_e2", "core.db", "2026-01-03T03:04:05Z");

    let dest = dir.path().join("okf");
    let (code, v) = run_json(&["--root", root.to_str().unwrap(), "export-okf", dest.to_str().unwrap()]);
    assert_eq!(code, 0, "{v}");
    assert_eq!(v["pages"], 2);
    assert!(v["links"].is_i64() && v["bytes"].as_i64().unwrap() > 0, "{v}");

    // layout: index.md + concepts/*.md + .claims/*.json
    let index = std::fs::read_to_string(dest.join("index.md")).unwrap();
    assert!(index.contains("okf_version: \"0.2\""), "{index}");
    assert!(index.contains("type: index"), "{index}");
    // generated clock = max claim timestamp (content-derived)
    assert!(index.contains("at: 2026-01-03T03:04:05Z"), "{index}");
    assert!(index.contains("- [Feature.Flags](concepts/feature-flags.md)"), "{index}");

    let concept = std::fs::read_to_string(dest.join("concepts/feature-flags.md")).unwrap();
    assert!(concept.contains("type: concept"), "{concept}");
    assert!(concept.contains("title: Feature.Flags"), "{concept}");
    assert!(concept.contains("  - source_s1"), "{concept}");
    assert!(concept.contains("- `status` = **ok** — status: accepted, version: 1"), "{concept}");

    let sidecar: Value = serde_json::from_str(
        &std::fs::read_to_string(dest.join(".claims/feature-flags.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(sidecar[0]["id"], "claim_e1");
    assert_eq!(sidecar[0]["sources"], json!(["source_s1"]));

    // byte-stable: second run to a fresh dest produces identical bytes
    let dest2 = dir.path().join("okf2");
    let (code, _) = run_json(&["--root", root.to_str().unwrap(), "export-okf", dest2.to_str().unwrap()]);
    assert_eq!(code, 0);
    assert_eq!(tree_snapshot(&dest), tree_snapshot(&dest2));
}

#[test]
fn export_okf_replaces_existing_dest() {
    let (dir, _wiki) = common::wiki();
    let root = dir.path();
    write_claim(root, "claim_r1", "a.b", "2026-01-02T00:00:00Z");
    let dest = dir.path().join("okf");
    std::fs::create_dir_all(&dest).unwrap();
    std::fs::write(dest.join("junk.txt"), "junk").unwrap();
    let (code, v) = run_json(&["--root", root.to_str().unwrap(), "export-okf", dest.to_str().unwrap()]);
    assert_eq!(code, 0, "{v}");
    assert!(!dest.join("junk.txt").exists());
    assert!(dest.join("index.md").is_file());
}

#[test]
fn export_okf_empty_wiki_txn() {
    let (dir, _wiki) = common::wiki();
    let dest = dir.path().join("okf");
    let (code, v) = run_json(&["--root", dir.path().to_str().unwrap(), "export-okf", dest.to_str().unwrap()]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["error_type"], "txn");
    assert!(v["error"].as_str().unwrap().contains("no claims"), "{v}");
}

#[test]
fn graph_export_copies_ui_and_payload() {
    let (dir, _wiki) = common::wiki();
    let root = dir.path();
    write_claim(root, "claim_g1", "a.b", "2026-01-02T00:00:00Z");
    // temp UI dist
    let ui = dir.path().join("ui");
    std::fs::create_dir_all(ui.join("assets")).unwrap();
    std::fs::write(ui.join("index.html"), "<html>viz</html>").unwrap();
    std::fs::write(ui.join("assets/app.js"), "app()").unwrap();

    let dest = dir.path().join("viz");
    let (code, v) = run_json(&[
        "--root", root.to_str().unwrap(),
        "graph-export", dest.to_str().unwrap(),
        "--ui-dist", ui.to_str().unwrap(),
    ]);
    assert_eq!(code, 0, "{v}");
    for k in ["dir", "files", "nodes", "links", "bytes"] {
        assert!(v.get(k).is_some(), "missing {k}: {v}");
    }
    assert_eq!(v["dir"], json!(dest.to_str().unwrap()));
    assert_eq!(v["files"], 3); // index.html + assets/app.js + graph-data.json
    assert_eq!(v["nodes"], 1);
    assert!(dest.join("index.html").is_file());
    assert!(dest.join("assets/app.js").is_file());
    let payload: Value = serde_json::from_str(
        &std::fs::read_to_string(dest.join("graph-data.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(payload["nodes"].as_array().unwrap().len(), 1);
    assert_eq!(payload["nodes"][0]["kind"], "entity");
}

#[test]
fn graph_export_empty_graph_txn() {
    let (dir, _wiki) = common::wiki();
    let dest = dir.path().join("viz");
    let (code, v) = run_json(&["--root", dir.path().to_str().unwrap(), "graph-export", dest.to_str().unwrap()]);
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["error_type"], "txn");
    assert!(v["error"].as_str().unwrap().contains("empty"), "{v}");
}

#[test]
fn graph_export_missing_ui_template_txn() {
    let (dir, _wiki) = common::wiki();
    let root = dir.path();
    write_claim(root, "claim_m1", "a.b", "2026-01-02T00:00:00Z");
    // Run a copy of the binary out of a tempdir so the packaged-dist
    // candidates (found via the real exe's repo path) do not resolve.
    let copy = dir.path().join("llm-wiki-bin");
    std::fs::copy(env!("CARGO_BIN_EXE_llm-wiki"), &copy).unwrap();
    let empty_ui = dir.path().join("empty-ui");
    std::fs::create_dir_all(&empty_ui).unwrap();
    let out = Command::new(&copy)
        .args([
            "--root", root.to_str().unwrap(),
            "graph-export", dir.path().join("viz").to_str().unwrap(),
            "--ui-dist", empty_ui.to_str().unwrap(),
        ])
        .current_dir(dir.path())
        .env("WIKI_UI_DIST", empty_ui.to_str().unwrap())
        .output()
        .unwrap();
    let code = out.status.code().unwrap_or(-1);
    let v: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout not JSON ({e}): {}", String::from_utf8_lossy(&out.stdout)));
    assert_eq!(code, 3, "{v}");
    assert_eq!(v["error_type"], "txn");
    assert!(v["error"].as_str().unwrap().contains("graph UI template not found"), "{v}");
}
