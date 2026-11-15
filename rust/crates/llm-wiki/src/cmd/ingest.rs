//! `ingest` — CLI wiring for sources.ingest (wiki.py:cmd_ingest parity).
//! Lanes: --file, --text, --url, --normalized-content/--parser-name,
//! --dir, --triage, --connector/--ref/--limit (JSON-API presets),
//! --list-connectors.

use std::io::Read;
use std::path::PathBuf;

use serde_json::{json, Value};
use crate::err::EngineError;
use crate::evidence::connectors;
use crate::evidence::dirs;
use crate::evidence::sources::{self, IngestOpts};
use crate::ids;
use crate::store::Wiki;

pub use crate::evidence::dirs::DEFAULT_MAX_BYTES;

#[derive(Default)]
pub struct IngestArgs {
    pub file: Option<PathBuf>,
    pub url: Option<String>,
    pub dir: Option<PathBuf>,
    pub text: Option<String>,
    pub text_ref: Option<String>,
    pub kind: Option<String>,
    pub source_id: Option<String>,
    pub normalized_content: Option<PathBuf>,
    pub parser_name: Option<String>,
    pub include_hidden: bool,
    pub max_bytes: u64,
    pub triage: Option<PathBuf>,
    /// preset connector name (see --list-connectors)
    pub connector: Option<String>,
    /// endpoint ref for --connector (URL or repo path)
    pub ref_: Option<String>,
    /// max items for --connector
    pub limit: usize,
    pub list_connectors: bool,
    pub force: bool,
}

pub fn run(wiki: &Wiki, args: &IngestArgs) -> Result<Value, EngineError> {
    if args.list_connectors {
        return Ok(json!({ "connectors": connectors::list_presets() }));
    }
    if let Some(connector) = &args.connector {
        let rref = match &args.ref_ {
            Some(r) if !r.is_empty() => r,
            _ => {
                return Err(EngineError::Txn(
                    "--connector requires --ref (endpoint URL or repo path)".into(),
                ));
            }
        };
        return connectors::ingest_preset(wiki, connector, rref, args.limit, args.force);
    }
    if args.kind.is_some() && (args.url.is_some() || args.dir.is_some() || args.triage.is_some()) {
        return Err(EngineError::Txn(
            "--kind only applies to --file/--text ingest".into(),
        ));
    }
    if let Some(m) = &args.triage {
        if args.url.is_some() || args.dir.is_some() || args.file.is_some() || args.text.is_some() {
            return Err(EngineError::Txn(
                "--triage does not combine with --file/--url/--dir/--text".into(),
            ));
        }
        if args.normalized_content.is_some() || args.source_id.is_some() || args.parser_name.is_some()
        {
            return Err(EngineError::Txn(
                "--normalized-content/--source-id/--parser-name are per-file; not valid with --triage"
                    .into(),
            ));
        }
        // `_load_json_file` + ingest_triage (wiki.py:109-113).
        let text = std::fs::read_to_string(m).map_err(|e| {
            EngineError::Txn(format!("cannot read triage manifest {}: {e}", m.display()))
        })?;
        let manifest: Value = serde_json::from_str(&text).map_err(|e| {
            EngineError::Txn(format!("cannot read triage manifest {}: {e}", m.display()))
        })?;
        return Ok(sources::ingest_triage(wiki, &manifest, args.force));
    }
    if let Some(d) = &args.dir {
        if args.normalized_content.is_some() {
            return Err(EngineError::Txn(
                "--normalized-content is per-file; not valid with --dir".into(),
            ));
        }
        return dirs::ingest_dir(wiki, d, args.include_hidden, args.max_bytes, args.force);
    }
    let mut opts = IngestOpts {
        source_id: args.source_id.clone(),
        parser_name: args.parser_name.clone(),
        force: args.force,
        ..Default::default()
    };
    if let Some(nc) = &args.normalized_content {
        opts.normalized_content = Some(
            std::fs::read_to_string(nc).map_err(|e| {
                EngineError::Txn(format!(
                    "cannot read --normalized-content {}: {e}",
                    nc.display()
                ))
            })?,
        );
    }
    if let Some(url) = &args.url {
        let data = fetch_url(url, args.max_bytes)?;
        return sources::ingest(wiki, "url", url, &data, &opts);
    }
    let (rref, data): (String, Vec<u8>) = if let Some(f) = &args.file {
        // A directory is a lane-selection mistake, not a read failure —
        // say so as usage (exit 2); --dir is the (P2) directory lane.
        if f.is_dir() {
            return Err(EngineError::Usage(format!(
                "--file is a directory, not a file: {} \
                 (directory ingest is --dir)",
                f.display()
            )));
        }
        let data = std::fs::read(f)
            .map_err(|e| EngineError::Txn(format!("cannot read {}: {e}", f.display())))?;
        (f.to_string_lossy().into_owned(), data)
    } else if let Some(t) = &args.text {
        let rref = args
            .text_ref
            .clone()
            .unwrap_or_else(|| ids::new("text"));
        (rref, t.as_bytes().to_vec())
    } else {
        return Err(EngineError::Txn(
            "provide --file, --url or --text".into(),
        ));
    };
    let kind = args
        .kind
        .clone()
        .unwrap_or_else(|| if args.text.is_some() { "text" } else { "file" }.to_string());
    sources::ingest(wiki, &kind, &rref, &data, &opts)
}

/// `_fetch_url` (wiki.py:46-87): http/https only, response capped at
/// --max-bytes.
pub(super) fn fetch_url(url: &str, max_bytes: u64) -> Result<Vec<u8>, EngineError> {
    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(EngineError::Txn(format!(
            "only http/https URLs may be ingested (got {url:?})"
        )));
    }
    let resp = ureq::get(url)
        .call()
        .map_err(|e| EngineError::Txn(format!("cannot fetch {url}: {e}")))?;
    let mut body = resp.into_body();
    let mut reader = body.as_reader().take(max_bytes + 1);
    let mut data = Vec::new();
    reader
        .read_to_end(&mut data)
        .map_err(|e| EngineError::Txn(format!("cannot fetch {url}: {e}")))?;
    if data.len() as u64 > max_bytes {
        return Err(EngineError::Txn(format!(
            "response from {url} exceeds --max-bytes {max_bytes}"
        )));
    }
    Ok(data)
}
