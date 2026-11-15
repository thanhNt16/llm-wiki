//! `triage` — deterministic ingest proposals for a directory
//! (wiki.py:cmd_triage → sources.triage manifest), merged with the
//! Rust-only incremental layer: `cursors::scan` adds top-level
//! `changed`/`new`/`removed` buckets (spec §8 — additive keys allowed
//! under §6.1). Buckets only appear once a wiki exists; the manifest
//! itself stays usable on a bare directory, matching Python.

use std::path::PathBuf;

use serde_json::Value;

use crate::err::EngineError;
use crate::evidence::{cursors, sources};
use crate::store::Wiki;
use crate::util;

#[derive(Default)]
pub struct TriageArgs {
    pub dir: PathBuf,
    /// Also write the manifest JSON here (stdout always gets it).
    pub out: Option<PathBuf>,
}

pub fn run(wiki: &Wiki, args: &TriageArgs) -> Result<Value, EngineError> {
    if !args.dir.is_dir() {
        return Err(EngineError::Txn(format!(
            "not a directory: {}",
            args.dir.display()
        )));
    }
    let mut manifest = sources::triage(&args.dir)?;
    // Incremental dir-cursor diff — persists `.state/dir-cursors.json`
    // under its own txn. Only when the wiki is initialized; a bare
    // directory still gets the Python-shaped manifest.
    if wiki.exists() {
        let triage = cursors::scan(wiki, &args.dir)?;
        manifest["changed"] = serde_json::to_value(&triage.changed)?;
        manifest["new"] = serde_json::to_value(&triage.new)?;
        manifest["removed"] = serde_json::to_value(&triage.removed)?;
    }
    if let Some(out) = &args.out {
        util::write_json_atomic(out, &manifest).map_err(|e| {
            EngineError::Txn(format!("cannot write --out {}: {e}", out.display()))
        })?;
    }
    Ok(manifest)
}
