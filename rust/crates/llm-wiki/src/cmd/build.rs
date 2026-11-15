//! `build` — one command: compile (until agent input is required) →
//! build-pages → verify (wiki.py:cmd_build 170-200 parity).
//!
//! Honest blocking: while a human/agent step is pending the payload is
//! `{"phase": "blocked", "step", …, "next"}` and the process exits 3 —
//! never inventing content to finish the pipeline. A `done` payload
//! exits 0 iff `verify.ok`.

use std::path::Path;

use serde_json::{json, Value};

use crate::compile;
use crate::err::EngineError;
use crate::pages;
use crate::store::Wiki;

pub fn run(wiki: &Wiki) -> Result<Value, EngineError> {
    let plan = compile::compile_umbrella(wiki, false)?;
    let unstaged: Vec<Value> = plan["pending"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|p| {
            !p["candidates_path"]
                .as_str()
                .map(|cp| Path::new(cp).is_file())
                .unwrap_or(false)
        })
        .collect();
    if !unstaged.is_empty() {
        return Ok(json!({
            "phase": "blocked",
            "step": "extract",
            "pending": unstaged,
            "next": "write shorthand candidates to each candidates_path \
                     (wiki-compile), then re-run build",
        }));
    }
    let r = compile::compile_umbrella(wiki, true)?;
    if r["phase"].as_str() == Some("classify") {
        return Ok(json!({
            "phase": "blocked",
            "step": "classify",
            "needs_review": r["needs_review"],
            "next": "write classifications.json for the flagged runs \
                     (wiki-review), then re-run build",
        }));
    }
    if r["phase"].as_str() == Some("waiting") {
        let pg = pages::build_pages(wiki, wiki.revision()?)?;
        let v = pages::verify(wiki)?;
        return Ok(json!({
            "phase": "done",
            "compile": r,
            "pages": pg,
            "verify": v,
        }));
    }
    // phase == "done": apply already rebuilt pages and verified
    Ok(json!({
        "phase": "done",
        "compile": r,
        "pages": {"note": "rebuilt during apply"},
        "verify": r["verify"],
    }))
}

/// Exit code for a build payload: `blocked` → 3; `done` → `0 if
/// verify["ok"] else 3` (wiki.py:184/191/196/200 parity).
pub fn exit_code(payload: &Value) -> i32 {
    if payload["phase"].as_str() == Some("blocked") {
        return 3;
    }
    if payload["verify"]["ok"].as_bool().unwrap_or(false) {
        0
    } else {
        3
    }
}
