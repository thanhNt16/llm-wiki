//! `graph-export` — self-contained static visualizer bundle
//! (wiki.py:cmd_graph_export 663-667 parity). Emits vizexport.export's
//! `{dir, files, nodes, links, bytes}` receipt.

use std::path::PathBuf;

use serde_json::Value;

use crate::err::EngineError;
use crate::store::Wiki;
use crate::vizexport;

pub fn run(
    wiki: &Wiki,
    output: &PathBuf,
    ui_dist: Option<&PathBuf>,
) -> Result<Value, EngineError> {
    vizexport::export(wiki, output, ui_dist.map(|p| p.as_path()))
}
