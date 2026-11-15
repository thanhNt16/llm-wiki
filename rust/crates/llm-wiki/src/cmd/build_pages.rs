//! `build-pages` — (re)build the deterministic derived views
//! (section indexes, overview, review/log/changes pages) and register
//! their dependency sets (wiki.py:cmd_build_pages 298-302 parity).
//! Commits under commit.lock; `--base-revision` is the
//! optimistic-concurrency fence.

use serde_json::Value;

use crate::err::EngineError;
use crate::pages;
use crate::store::Wiki;

#[derive(Default)]
pub struct BuildPagesArgs {
    /// optimistic-concurrency base revision (default: wiki.revision())
    pub base_revision: Option<i64>,
}

pub fn run(wiki: &Wiki, args: &BuildPagesArgs) -> Result<Value, EngineError> {
    let base = match args.base_revision {
        Some(b) => b,
        None => wiki.revision()?,
    };
    pages::build_pages(wiki, base)
}
