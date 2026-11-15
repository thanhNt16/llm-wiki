//! `graph-data` — knowledge-graph payload (wiki.py:cmd_graph_data
//! 647-657 parity). Without `--output` the payload itself is the stdout
//! JSON; with `--output` the payload is written to the file and the
//! stdout payload is `{"written","nodes","links"}`.

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::err::EngineError;
use crate::graphdata;
use crate::store::Wiki;

#[derive(Default)]
pub struct GraphDataArgs {
    /// write JSON to file instead of stdout
    pub output: Option<PathBuf>,
    /// embed full claim dicts in entity nodes (default emits claim_ids)
    pub full: bool,
}

pub fn run(wiki: &Wiki, args: &GraphDataArgs) -> Result<Value, EngineError> {
    let payload = graphdata::graph_payload(wiki, args.full)?;
    match &args.output {
        Some(path) => {
            let text = serde_json::to_string(&payload)?;
            std::fs::write(path, format!("{text}\n"))?;
            Ok(json!({
                "written": path.to_string_lossy(),
                "nodes": payload["nodes"].as_array().map(|n| n.len()).unwrap_or(0) as i64,
                "links": payload["links"].as_array().map(|n| n.len()).unwrap_or(0) as i64,
            }))
        }
        None => Ok(payload),
    }
}
