//! `write-page` — validate + commit an agent-authored derived page
//! (wiki.py:cmd_write_page 337-406 parity). Text source: `--text`, or
//! `--file` (with `-` for stdin); deps come from `--deps-file` (JSON list
//! or page-targets target dict), `--deps` (comma list), the page's own
//! `deps:` frontmatter, or auto-derivation from `[[claim_*]]`/`[[wiki:]]`
//! links — in that precedence order. Commits under commit.lock.
//!
//! `--decide` parity note: Python attaches an advisory Jev noul verdict
//! (`decision.same_page_p`) to a near-duplicate refusal's error payload.
//! `jev::ask` is a P4 stub that never reports ok, and `EngineError` has
//! no sidecar field — the advisory lane is therefore an unconditional
//! no-op here until P4 lands (same observable result as an unset env).

use std::io::Read;
use std::path::PathBuf;

use serde_json::Value;

use crate::err::EngineError;
use crate::jev;
use crate::store::Wiki;
use crate::synthesize;

#[derive(Default)]
pub struct WritePageArgs {
    /// markdown page file, '-' for stdin (body + optional deps frontmatter)
    pub file: Option<PathBuf>,
    /// markdown page as a string argument
    pub text: Option<String>,
    /// target wiki path, e.g. wiki/concepts/foo.md (required)
    pub artifact: String,
    /// JSON file: list of dep strings, or a page-targets target dict
    pub deps_file: Option<PathBuf>,
    /// comma-separated dep list (overrides deps: frontmatter)
    pub deps: Option<String>,
    /// allow creating a page that near-duplicates an existing one
    /// (records forced_new: true)
    pub allow_new: bool,
    /// optimistic-concurrency base revision
    pub base_revision: Option<i64>,
    /// attach an advisory Jev same-page verdict to a near-duplicate
    /// refusal (no-op when Jev env is unset)
    pub decide: bool,
}

pub fn run(wiki: &Wiki, args: &WritePageArgs) -> Result<Value, EngineError> {
    let text = if let Some(t) = &args.text {
        t.clone()
    } else if args.file.as_deref() == Some(std::path::Path::new("-")) {
        let mut buf = String::new();
        std::io::stdin().read_to_string(&mut buf)?;
        buf
    } else if let Some(f) = &args.file {
        std::fs::read_to_string(f).map_err(|e| {
            EngineError::Txn(format!("cannot read --file {}: {e}", f.display()))
        })?
    } else {
        return Err(EngineError::Txn(
            "provide --file (- for stdin) or --text".into(),
        ));
    };

    // dep_list=None (absent everywhere) -> write_page auto-derives from links.
    let mut dep_list: Option<Vec<Value>> = None;
    if let Some(df) = &args.deps_file {
        let parsed: Value = (|| -> Result<Value, EngineError> {
            let text = std::fs::read_to_string(df)?;
            Ok(serde_json::from_str(&text)?)
        })()
        .map_err(|e| {
            EngineError::Txn(format!("cannot parse --deps-file {}: {e}", df.display()))
        })?;
        if let Some(dict) = parsed.as_object() {
            match dict.get("claim_ids").and_then(Value::as_array) {
                Some(ids) => dep_list = Some(ids.clone()),
                None => {
                    return Err(EngineError::Txn(
                        "--deps-file dict must contain \"claim_ids\"".into(),
                    ));
                }
            }
        } else if let Some(list) = parsed.as_array() {
            dep_list = Some(list.clone());
        } else {
            return Err(EngineError::Txn(
                "--deps-file must be a JSON list or a page-target dict".into(),
            ));
        }
    } else if let Some(deps) = &args.deps {
        dep_list = Some(
            deps.split(',')
                .map(str::trim)
                .filter(|d| !d.is_empty())
                .map(|d| Value::String(d.to_string()))
                .collect(),
        );
    } else {
        if let Ok((front, _body)) = crate::frontmatter::split_front(&text) {
            if let Some(fm) = front.get("deps").and_then(Value::as_array) {
                dep_list = Some(fm.clone());
            }
        }
    }

    let base = match args.base_revision {
        Some(b) => b,
        None => wiki.revision()?,
    };
    let result = synthesize::write_page(
        wiki,
        &args.artifact,
        &text,
        dep_list,
        base,
        args.allow_new,
    );
    match result {
        Ok(v) => Ok(v),
        Err(e) => {
            // --decide: advisory noul verdict would ride on the near-dup
            // refusal (Python sets e.decision); EngineError carries no
            // decision sidecar, so the verdict is consulted (and cached)
            // but cannot reach the payload — a documented gap, never a
            // silent exception.
            if args.decide
                && jev::is_ready()
                && e.to_string().starts_with("near-duplicate page")
            {
                let front = crate::frontmatter::split_front(&text)
                    .map(|(f, _)| f)
                    .unwrap_or_default();
                let twin = e
                    .to_string()
                    .strip_prefix("near-duplicate page: ")
                    .and_then(|rest| rest.split_whitespace().next())
                    .unwrap_or("")
                    .to_string();
                let state = format!(
                    "subject: {}; title: {}; proposed: {}; existing twin: {}",
                    front.get("subject").map(|v| v.to_string()).unwrap_or_default(),
                    front.get("title").map(|v| v.to_string()).unwrap_or_default(),
                    args.artifact,
                    twin,
                );
                let _ = jev::ask_with(
                    Some(&wiki.root),
                    &state,
                    &serde_json::json!({
                        "same_page": {
                            "type": "noul",
                            "instructions": "Is the new page genuinely a near-duplicate of the existing one — should creation be refused?"
                        }
                    }),
                );
            }
            Err(e)
        }
    }
}
