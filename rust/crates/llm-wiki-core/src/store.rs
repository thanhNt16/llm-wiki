//! Project-root / `.llm-wiki` layout, `wiki.json` sentinel, config and
//! manifest io, and the idempotent `init_wiki` scaffold.
//!
//! `wiki.json` is the initialization sentinel: `init_wiki` writes it
//! LAST (via `save_config` → `atomic_write`), so a crash mid-init leaves a
//! partial scaffold that a later `init` completes — never a torn
//! "initialized" wiki. `load_json_opt` collapses missing-file to `None`
//! while `load_json` errors, matching `wiki.py`'s two read modes.
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::err::EngineError;
use crate::util::write_json_atomic;

pub const SKILL_VERSION: &str = "0.3.0";

pub fn default_config() -> Value {
    json!({
        "acceptance_policy": {
            "auto_accept_authority": [
                "explicit_project_decision",
                "authoritative_source",
                "implementation_verification",
            ],
            "corroboration_auto_accept": 2,
        },
        "context_budget_default": 6000,
        "secret_policy": "warn",
        "allow_cross_project": false,
    })
}

pub const README: &str = "# .llm-wiki\n\nEvidence-backed project memory. Canonical data lives in `sources/`, `claims/`,\n`decisions/`, `topics.json`; everything under `wiki/`, `context/`, `views/` is derived and\nrebuildable. Do not edit canonical JSON by hand — use the llm-wiki skill\ncommands (wiki-init, wiki-ingest, wiki-compile, wiki-query, wiki-context,\nwiki-review, wiki-doctor).\n";

pub struct Wiki {
    pub root: PathBuf,
    pub dot: PathBuf,
    /// CLI `--allow-repair`: `Txn::begin_with` ORs this into
    /// `TxnOpts.allow_repair` so every txn-owning path gets the override.
    pub allow_repair: bool,
}

pub fn absolute(p: &Path) -> Result<PathBuf, EngineError> {
    if let Ok(abs) = fs::canonicalize(p) {
        return Ok(abs);
    }
    if p.is_absolute() {
        return Ok(p.to_path_buf());
    }
    Ok(std::env::current_dir()?.join(p))
}

impl Wiki {
    pub fn open(root: &Path) -> Result<Wiki, EngineError> {
        let root = absolute(root)?;
        // A project root that exists but is a regular file can never host
        // `.llm-wiki/` — every later probe would degrade into an ENOTDIR
        // mid-command. Reject up front as usage (exit 2).
        if root.exists() && !root.is_dir() {
            return Err(EngineError::Usage(format!(
                "wiki root is not a directory: {}",
                root.display()
            )));
        }
        let dot = root.join(".llm-wiki");
        Ok(Wiki { root, dot, allow_repair: false })
    }

    pub fn p(&self, rel: &str) -> PathBuf {
        self.dot.join(rel)
    }

    /// `wiki.json` is the sentinel: init writes it LAST so a crash mid-init
    /// only leaves a partial scaffold a later `init` completes.
    pub fn exists(&self) -> bool {
        self.p("wiki.json").is_file()
    }

    pub fn state(&self) -> Result<Value, EngineError> {
        let text = fs::read_to_string(self.p(".state/manifest.json"))?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn save_state(&self, s: &Value) -> Result<(), EngineError> {
        write_json_atomic(&self.p(".state/manifest.json"), s)?;
        Ok(())
    }

    /// `wiki.load_json(rel)` parity — canonical JSON read under the wiki root.
    /// Missing file → Err; callers wanting None-for-missing use `load_json_opt`.
    pub fn load_json(&self, rel: &str) -> Result<Value, EngineError> {
        let text = fs::read_to_string(self.p(rel))?;
        Ok(serde_json::from_str(&text)?)
    }

    pub fn load_json_opt(&self, rel: &str) -> Result<Option<Value>, EngineError> {
        match fs::read_to_string(self.p(rel)) {
            Ok(text) => Ok(Some(serde_json::from_str(&text)?)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e.into()),
        }
    }

    /// `wiki.read_jsonl` — missing file → empty; a corrupt line fails
    /// unless it is the last non-blank one (crash-truncated trailing
    /// append is tolerated, mirroring store.py).
    pub fn read_jsonl(&self, rel: &str) -> Result<Vec<Value>, EngineError> {
        let path = self.p(rel);
        let text = match fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(e) => return Err(e.into()),
        };
        let lines: Vec<&str> = text.lines().collect();
        let mut last = lines.len() as i64 - 1;
        while last >= 0 && lines[last as usize].trim().is_empty() {
            last -= 1;
        }
        let mut out = Vec::new();
        for (i, line) in lines.iter().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str(line) {
                Ok(v) => out.push(v),
                Err(e) => {
                    if i as i64 == last {
                        continue;
                    }
                    return Err(e.into());
                }
            }
        }
        Ok(out)
    }

    pub fn revision(&self) -> Result<i64, EngineError> {
        Ok(self.state()?["revision"].as_i64().unwrap_or(0))
    }

    /// DEFAULT_CONFIG merged under stored wiki.json, with acceptance_policy
    /// merged at the nested level (parity with store.py load_config).
    pub fn config(&self) -> Result<Value, EngineError> {
        let mut cfg = default_config();
        let stored: Value =
            serde_json::from_str(&fs::read_to_string(self.p("wiki.json"))?)?;
        let cfg_obj = cfg.as_object_mut().unwrap();
        if let Some(stored_obj) = stored.as_object() {
            for (k, v) in stored_obj {
                cfg_obj.insert(k.clone(), v.clone());
            }
            // nested merge for acceptance_policy
            let mut pol = default_config()["acceptance_policy"].clone();
            if let Some(stored_pol) = stored.get("acceptance_policy") {
                let pol_obj = pol.as_object_mut().unwrap();
                if let Some(sp) = stored_pol.as_object() {
                    for (k, v) in sp {
                        pol_obj.insert(k.clone(), v.clone());
                    }
                }
            }
            cfg_obj.insert("acceptance_policy".to_string(), pol);
        }
        Ok(cfg)
    }

    pub fn save_config(&self, cfg: &Value) -> Result<(), EngineError> {
        write_json_atomic(&self.p("wiki.json"), cfg)?;
        Ok(())
    }
}

const INIT_DIRS: &[&str] = &[
    "raw/files",
    "raw/urls",
    "raw/repositories",
    "raw/sessions",
    "raw/text",
    "sources",
    "claims",
    "decisions",
    "notes",
    "wiki/concepts",
    "wiki/entities",
    "wiki/decisions",
    "wiki/procedures",
    "wiki/questions",
    "wiki/changes",
    "context",
    "views",
    ".state/locks",
    ".state/operations",
    ".state/staging",
];

/// Idempotent, non-destructive scaffold of the .llm-wiki layout.
/// Ports store.py:init_wiki (lines 105-155) verbatim in ordering.
///
/// Concurrency: the exists() check is only a fast path — scaffolding then
/// proceeds UNDER `commit.lock` (blocking, re-checked) so two racing
/// inits serialize instead of colliding on the fixed `.tmp` write targets,
/// and an in-flight commit can never interleave with a half-scaffold.
pub fn init_wiki(wiki: &Wiki, name: &str) -> Result<Value, EngineError> {
    if wiki.exists() {
        return Ok(json!({
            "initialized": false,
            "revision": wiki.revision()?,
            "root": wiki.root,
        }));
    }
    let _lock = crate::txn::lock::Lock::acquire_blocking(wiki)?;
    if wiki.exists() {
        // lost the race: the other init completed the scaffold
        return Ok(json!({
            "initialized": false,
            "revision": wiki.revision()?,
            "root": wiki.root,
        }));
    }
    for d in INIT_DIRS {
        fs::create_dir_all(wiki.p(d))?;
    }
    fs::write(wiki.p("README.md"), README)?;
    for (rel, body) in [
        (".state/aliases.json", json!({})),
        (".state/dependencies.json", json!({"edges": {}, "stale": []})),
        (".state/graph.json", json!({"relations": []})),
    ] {
        write_json_atomic(&wiki.p(rel), &body)?;
    }
    // Kind-routing scaffold: empty rules activate synthesize's built-in
    // heuristics. Guarded — a pre-existing file (init completing a partial
    // scaffold) is kept.
    if !wiki.p("page-kinds.json").is_file() {
        write_json_atomic(
            &wiki.p("page-kinds.json"),
            &json!({
                "rules": [],
                "default": "concepts",
                "_comment": "ordered rules; matchers: subject|prefix|suffix|regex|predicate; when.min_claims; empty rules -> built-in heuristics",
            }),
        )?;
    }
    for rel in [".state/operations.jsonl", ".state/review-queue.jsonl"] {
        fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(wiki.p(rel))?;
    }
    wiki.save_state(&json!({
        "revision": 0,
        "skill_version": SKILL_VERSION,
        "content_hash": "",
        "compiled": {},
    }))?;
    let mut cfg = default_config();
    cfg["project"] = json!(name);
    wiki.save_config(&cfg)?; // atomic; last write = exists() sentinel
    Ok(json!({
        "initialized": true,
        "revision": 0,
        "root": wiki.root,
    }))
}
