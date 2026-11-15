//! `stage-candidates` — stage agent-extracted claim candidates into a
//! run dir (wiki.py:cmd_stage_candidates 256-279 parity).
//!
//! `--file` may be `.jsonl` shorthand (one sparse object per line,
//! normalized with the manifest's root_origin) or `.json` (an array, or
//! `{"candidates": [...]}`). `--report` supplies the extraction report;
//! the default mirrors wiki.py:268-275. `--run` stages into an existing
//! pending run dir instead of minting a fresh one. Emits
//! `{"run_id", "staged"}` — the staging files under
//! `.state/staging/<run>/` are the real output (§5.2).

use std::path::PathBuf;

use serde_json::{json, Value};

use crate::compile;
use crate::err::EngineError;
use crate::evidence::sources;
use crate::shorthand::{normalize_candidate, parse_candidates_file};
use crate::store::Wiki;

#[derive(Default)]
pub struct StageCandidatesArgs {
    /// candidates JSON or JSONL file (required)
    pub file: PathBuf,
    pub source_id: String,
    pub source_version: i64,
    /// extraction report JSON file (optional — default mirrors wiki.py)
    pub report: Option<PathBuf>,
    /// stage into an existing pending run dir instead of minting one
    pub run: Option<String>,
}

fn load_json_file(path: &PathBuf) -> Result<Value, EngineError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| EngineError::Txn(format!("cannot read {}: {e}", path.display())))?;
    Ok(serde_json::from_str(&text)?)
}

pub fn run(wiki: &Wiki, args: &StageCandidatesArgs) -> Result<Value, EngineError> {
    let is_jsonl = args.file.to_string_lossy().ends_with(".jsonl");
    let candidates: Vec<Value> = if is_jsonl {
        // shorthand path — normalize with the manifest's root_origin so
        // evidence refs carry canonical identity (wiki.py:258-263)
        let manifest = sources::get_manifest(wiki, &args.source_id)?.unwrap_or(json!({}));
        let root_origin = manifest["root_origin"]
            .as_str()
            .unwrap_or(&args.source_id)
            .to_string();
        parse_candidates_file(&args.file)?
            .iter()
            .map(|c| {
                normalize_candidate(c, &args.source_id, args.source_version, &root_origin)
            })
            .collect::<Result<_, _>>()?
    } else {
        let data = load_json_file(&args.file)?;
        match data {
            Value::Object(o) => match o.get("candidates").cloned() {
                None | Some(Value::Null) => Vec::new(),
                Some(Value::Array(a)) => a,
                Some(other) => {
                    return Err(EngineError::Txn(format!(
                        "candidates file {}: 'candidates' is not an array (got {})",
                        args.file.display(),
                        crate::schema::py_repr(&other)
                    )))
                }
            },
            Value::Array(a) => a,
            other => {
                return Err(EngineError::Txn(format!(
                    "candidates file {}: expected array or object, got {}",
                    args.file.display(),
                    crate::schema::py_repr(&other)
                )))
            }
        }
    };
    let report = match &args.report {
        Some(p) => load_json_file(p)?,
        None => json!({
            "source_id": args.source_id,
            "source_version": args.source_version,
            "coverage": {"text": "complete", "tables": "skipped", "images": "skipped",
                         "diagrams": "skipped", "formulas": "skipped"},
            "warnings": [],
            "parser": {"name": "agent", "version": "in-session"},
        }),
    };
    let staged = candidates.len();
    let run_id = compile::stage_candidates(
        wiki,
        &args.source_id,
        args.source_version,
        candidates,
        &report,
        args.run.as_deref(),
    )?;
    Ok(json!({"run_id": run_id, "staged": staged}))
}
