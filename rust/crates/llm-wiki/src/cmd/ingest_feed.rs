//! `ingest-rss` — CLI wiring for connectors.ingest_feed
//! (wiki.py:cmd_ingest_feed parity). `--url` fetches via the shared
//! _fetch_url lane; `--file` reads local bytes; `--url` wins when both
//! are given (argparse truthiness order).

use std::path::PathBuf;

use serde_json::Value;

use crate::err::EngineError;
use crate::evidence::connectors;
use crate::store::Wiki;

use super::ingest::fetch_url;

#[derive(Default)]
pub struct IngestFeedArgs {
    pub url: Option<String>,
    pub file: Option<PathBuf>,
    pub limit: usize,
    pub max_bytes: u64,
    pub force: bool,
}

pub fn run(wiki: &Wiki, args: &IngestFeedArgs) -> Result<Value, EngineError> {
    let (rref, data) = if let Some(url) = &args.url {
        (url.clone(), fetch_url(url, args.max_bytes)?)
    } else if let Some(file) = &args.file {
        let data = std::fs::read(file)
            .map_err(|e| EngineError::Txn(format!("cannot read {}: {e}", file.display())))?;
        let abs = std::path::absolute(file)
            .map_err(|e| EngineError::Txn(format!("cannot read {}: {e}", file.display())))?;
        (abs.to_string_lossy().into_owned(), data)
    } else {
        return Err(EngineError::Txn("provide --url or --file".into()));
    };
    connectors::ingest_feed(wiki, &rref, &data, args.limit, args.force)
}
