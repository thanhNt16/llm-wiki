//! `ingest-git` — CLI wiring for connectors.ingest_git
//! (wiki.py:cmd_ingest_git parity; `git` subprocess inside connectors).

use std::path::PathBuf;

use serde_json::Value;

use crate::err::EngineError;
use crate::evidence::connectors;
use crate::store::Wiki;

#[derive(Default)]
pub struct IngestGitArgs {
    pub repo: PathBuf,
    pub max: usize,
    pub force: bool,
}

pub fn run(wiki: &Wiki, args: &IngestGitArgs) -> Result<Value, EngineError> {
    connectors::ingest_git(wiki, &args.repo.to_string_lossy(), args.max, args.force)
}
