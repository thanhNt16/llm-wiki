//! One-command static graph export — port of `wikicore/vizexport.py`.
//!
//! Copies the prebuilt wiki-visualize UI (graph-ui/dist) plus a freshly
//! generated full graph-data payload into a target directory — openwiki's
//! `visualize --export` contract, mirrored. Deterministic for a given
//! store (except generated_at); destination is replaced atomically,
//! never partially.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::err::EngineError;
use crate::graphdata;
use crate::store::Wiki;
use crate::util::{atomic_replace_dir, collect_files, copy_tree, make_tmp_dir, TmpDir};

/// `ui_dist(explicit)` — resolve the prebuilt UI template dir.
///
/// Resolution order (vizexport.py:23-39): `--ui-dist` flag > `WIKI_UI_DIST`
/// env > packaged candidates. The Python candidate is relative to the
/// wikicore/ module dir; the binary instead probes (a) `graph-ui/dist`
/// next to the executable (installed layout, spec §12 embedded-asset
/// layout) and (b) the repo checkout `skill/wiki-visualize/...` found by
/// walking up from the executable (dev layout).
pub fn ui_dist(explicit: Option<&Path>) -> Result<PathBuf, EngineError> {
    let mut cands: Vec<PathBuf> = Vec::new();
    if let Some(e) = explicit {
        cands.push(e.to_path_buf());
    }
    if let Ok(env) = std::env::var("WIKI_UI_DIST") {
        if !env.is_empty() {
            cands.push(PathBuf::from(env));
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            cands.push(dir.join("graph-ui/dist"));
            // walk ancestors for the repo-checkout layout
            let mut anc = Some(dir);
            while let Some(a) = anc {
                cands.push(a.join("skill/wiki-visualize/assets/graph-ui/dist"));
                anc = a.parent();
            }
        }
    }
    for c in &cands {
        if c.join("index.html").is_file() {
            return Ok(c.clone());
        }
    }
    Err(EngineError::Txn(format!(
        "graph UI template not found (need index.html): tried {} — pass \
         --ui-dist <dir> or set WIKI_UI_DIST",
        cands
            .iter()
            .map(|c| c.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    )))
}

/// `vizexport.export(wiki, out_dir, ui_dist_dir)` → receipt
/// `{dir, files, nodes, links, bytes}`.
pub fn export(
    wiki: &Wiki,
    out_dir: &Path,
    ui_dist_dir: Option<&Path>,
) -> Result<Value, EngineError> {
    let payload = graphdata::graph_payload(wiki, true)?;
    let node_count = payload
        .get("nodes")
        .and_then(Value::as_array)
        .map(|n| n.len())
        .unwrap_or(0);
    if node_count == 0 {
        return Err(EngineError::Txn(
            "wiki graph is empty: nothing to export".to_string(),
        ));
    }
    let src = ui_dist(ui_dist_dir)?;
    let tmp = make_tmp_dir("graph-export-", out_dir.parent().unwrap_or(Path::new(".")))?;
    let _guard = TmpDir(tmp.clone());
    copy_tree(&src, &tmp)?;
    fs::write(
        tmp.join("graph-data.json"),
        format!("{}\n", serde_json::to_string(&payload)?),
    )?;
    atomic_replace_dir(&tmp, out_dir)?;
    let files = collect_files(out_dir)?;
    let bytes: u64 = files
        .iter()
        .filter_map(|p| fs::metadata(p).ok().map(|m| m.len()))
        .sum();
    Ok(json!({
        "dir": out_dir.to_string_lossy(),
        "files": files.len() as i64,
        "nodes": payload["nodes"].as_array().map(|n| n.len()).unwrap_or(0) as i64,
        "links": payload["links"].as_array().map(|n| n.len()).unwrap_or(0) as i64,
        "bytes": bytes as i64,
    }))
}
