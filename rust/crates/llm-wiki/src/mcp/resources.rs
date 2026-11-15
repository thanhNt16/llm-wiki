//! `resources/*` + `completion/complete` — spec §7.6.
//!
//! - Fixed resource `resource://llm-wiki/claims`: a **synthesized
//!   projection** rebuilt per read from a live canonical `claims/*.json`
//!   scan (`{claims: [{id,subject,predicate,status,version}]}`) — never
//!   the legacy `.state/claims-index.json` and never sqlite.
//! - URI templates: `pages/{artifact}` (text/markdown) and
//!   `sources/{source_id}/manifest.json` + `.../content.md`
//!   (two fixed suffixes — a bare `{source_id}` never has to guess).
//! - Traversal guard `assert_within_dir`: `{artifact}` resolves inside
//!   `wiki/`, `{source_id}` inside `sources/`.
//! - Bodies truncate at 256 KiB with `…[truncated]` so an unbounded
//!   source cannot blow the client context window.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::{RpcError, Server};

/// `resources/read` body cap (§7.6).
const MAX_RESOURCE_BYTES: usize = 256 * 1024;

const CLAIMS_URI: &str = "resource://llm-wiki/claims";
const PAGES_PREFIX: &str = "resource://llm-wiki/pages/";
const SOURCES_PREFIX: &str = "resource://llm-wiki/sources/";

/// -32002 is the MCP "resource not found" server error (msft
/// ResourceNotFound parity); internal failures stay -32603.
const RESOURCE_NOT_FOUND: i64 = -32002;

fn nf(msg: impl Into<String>) -> RpcError {
    RpcError::new(RESOURCE_NOT_FOUND, msg)
}

pub(crate) fn list() -> Value {
    json!({
        "resources": [{
            "uri": CLAIMS_URI,
            "name": "claims",
            "title": "Claims index",
            "description": "Synthesized claims index — live scan of canonical \
                            claims/*.json ({id,subject,predicate,status,version})",
            "mimeType": "application/json",
        }]
    })
}

pub(crate) fn templates() -> Value {
    json!({
        "resourceTemplates": [
            {
                "uriTemplate": "resource://llm-wiki/pages/{artifact}",
                "name": "pages",
                "title": "Derived wiki page",
                "description": "A derived page under wiki/ (e.g. concepts/foo.md)",
                "mimeType": "text/markdown",
            },
            {
                "uriTemplate": "resource://llm-wiki/sources/{source_id}/manifest.json",
                "name": "source-manifest",
                "title": "Source manifest",
                "description": "Versioned manifest for one source",
                "mimeType": "application/json",
            },
            {
                "uriTemplate": "resource://llm-wiki/sources/{source_id}/content.md",
                "name": "source-content",
                "title": "Source content",
                "description": "Normalized markdown for one source",
                "mimeType": "text/markdown",
            },
        ]
    })
}

/// Bound root or -32002 — resource reads need an open wiki.
fn bound_wiki(srv: &mut Server) -> Result<crate::store::Wiki, RpcError> {
    srv.auto_bind(true);
    let root = srv
        .bound
        .clone()
        .ok_or_else(|| nf(super::NO_WIKI_MSG))?;
    srv.open_wiki_at(&root)
        .map_err(|e| RpcError::internal(e.to_string()))
}

pub(crate) fn read(srv: &mut Server, params: &Value) -> Result<Value, RpcError> {
    let Some(uri) = params["uri"].as_str() else {
        return Err(RpcError::invalid_params("resources/read requires a string `uri`"));
    };
    let wiki = bound_wiki(srv)?;
    let (text, mime) = read_uri(&wiki, uri)?;
    Ok(json!({
        "contents": [{
            "uri": uri,
            "mimeType": mime,
            "text": text,
        }]
    }))
}

fn read_uri(wiki: &crate::store::Wiki, uri: &str) -> Result<(String, &'static str), RpcError> {
    if uri == CLAIMS_URI {
        let mut claims = Vec::new();
        for c in crate::claims::list(wiki)
            .map_err(|e| RpcError::internal(e.to_string()))?
        {
            let row = crate::claims::index_row_from(&c);
            claims.push(json!({
                "id": row.id,
                "subject": row.subject,
                "predicate": row.predicate,
                "status": row.status,
                "version": row.version,
            }));
        }
        let text = serde_json::to_string_pretty(&json!({"claims": claims}))
            .map_err(|e| RpcError::internal(e.to_string()))?;
        return Ok((text, "application/json"));
    }
    if let Some(rest) = uri.strip_prefix(PAGES_PREFIX) {
        // `{artifact}` may be `concepts/foo.md` or `wiki/concepts/foo.md`;
        // both resolve inside .llm-wiki/wiki/ (assertWithinDir).
        let rel = rest
            .strip_prefix("wiki/")
            .unwrap_or(rest);
        let rel = decode(rel)?;
        let path = assert_within_dir(&wiki.p("wiki"), Path::new(&rel))
            .ok_or_else(|| nf(format!("artifact escapes wiki/: {rest}")))?;
        return read_file(&path, "text/markdown", uri);
    }
    if let Some(rest) = uri.strip_prefix(SOURCES_PREFIX) {
        for (suffix, mime) in [
            ("/manifest.json", "application/json"),
            ("/content.md", "text/markdown"),
        ] {
            if let Some(sid) = rest.strip_suffix(suffix) {
                let sid = decode(sid)?;
                let dir = assert_within_dir(&wiki.p("sources"), Path::new(&sid))
                    .ok_or_else(|| nf(format!("source_id escapes sources/: {sid}")))?;
                let file = dir.join(&suffix[1..]);
                return read_file(&file, mime, uri);
            }
        }
        return Err(nf(format!(
            "sources template needs /manifest.json or /content.md suffix: {uri}"
        )));
    }
    Err(nf(format!("unknown resource URI: {uri}")))
}

/// Read a file into the resource body, capped at 256 KiB with a
/// `…[truncated]` marker (§7.6). Char-boundary-safe.
fn read_file(
    path: &Path,
    mime: &'static str,
    uri: &str,
) -> Result<(String, &'static str), RpcError> {
    let bytes = std::fs::read(path)
        .map_err(|_| nf(format!("resource not found: {uri}")))?;
    if bytes.len() <= MAX_RESOURCE_BYTES {
        let text = String::from_utf8(bytes)
            .map_err(|_| nf(format!("resource is not UTF-8: {uri}")))?;
        return Ok((text, mime));
    }
    let mut end = MAX_RESOURCE_BYTES;
    while end > 0 && std::str::from_utf8(&bytes[..end]).is_err() {
        end -= 1;
    }
    let text = format!("{}…[truncated]", String::from_utf8_lossy(&bytes[..end]));
    Ok((text, mime))
}

/// `assertWithinDir`: join `rel` under `base`, refusing `..`/rooted
/// escapes lexically (the wiki may not exist yet — no canonicalize).
fn assert_within_dir(base: &Path, rel: &Path) -> Option<PathBuf> {
    use std::path::Component;
    let mut out = PathBuf::from(base);
    for comp in rel.components() {
        match comp {
            Component::Normal(c) => out.push(c),
            Component::CurDir => {}
            // `..`, root and prefix components can only escape `base`.
            _ => return None,
        }
    }
    if out.as_os_str().is_empty() {
        return None;
    }
    Some(out)
}

/// Percent-decoding for URI path segments (same rules as the roots
/// `file:` decoder in the transport layer).
fn decode(s: &str) -> Result<String, RpcError> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if i + 2 >= bytes.len() {
                return Err(nf("bad percent-encoding in URI"));
            }
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3])
                .map_err(|_| nf("bad percent-encoding in URI"))?;
            let b = u8::from_str_radix(hex, 16)
                .map_err(|_| nf("bad percent-encoding in URI"))?;
            out.push(b);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| nf("bad percent-encoding in URI"))
}

/// `completion/complete` — completes `ref/resource` template arguments
/// (`{artifact}`, `{source_id}`) against on-disk listings (§7.6).
/// `ref/prompt` is out of scope (no prompts capability in v1).
pub(crate) fn complete(srv: &mut Server, params: &Value) -> Result<Value, RpcError> {
    let Some(rref) = params.get("ref") else {
        return Err(RpcError::invalid_params("completion/complete requires `ref`"));
    };
    if rref["type"].as_str() == Some("ref/prompt") {
        return Err(RpcError::invalid_params(
            "ref/prompt completion is unsupported (no prompts capability)",
        ));
    }
    if rref["type"].as_str() != Some("ref/resource") {
        return Err(RpcError::invalid_params("ref must be ref/resource"));
    }
    let uri = rref["uri"].as_str().unwrap_or("");
    let arg = &params["argument"];
    let name = arg["name"].as_str().unwrap_or("");
    let prefix = arg["value"].as_str().unwrap_or("").to_string();

    let candidates = if uri == "resource://llm-wiki/pages/{artifact}" || name == "artifact" {
        page_candidates(srv)
    } else if uri == "resource://llm-wiki/sources/{source_id}/manifest.json"
        || uri == "resource://llm-wiki/sources/{source_id}/content.md"
        || name == "source_id"
    {
        source_candidates(srv)
    } else {
        Vec::new()
    };
    let mut values: Vec<String> = candidates
        .into_iter()
        .filter(|c| c.starts_with(&prefix))
        .collect();
    values.sort();
    let total = values.len();
    values.truncate(100);
    Ok(json!({
        "completion": {
            "values": values,
            "total": total,
            "hasMore": total > 100,
        }
    }))
}

/// `wiki/<kind>/<slug>.md` rel paths, unbound → empty.
fn page_candidates(srv: &mut Server) -> Vec<String> {
    let Ok(wiki) = bound_wiki(srv) else {
        return Vec::new();
    };
    let base = wiki.p("wiki");
    let mut out = Vec::new();
    if let Ok(kinds) = std::fs::read_dir(&base) {
        for kind in kinds.flatten() {
            if !kind.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                continue;
            }
            let kname = kind.file_name().to_string_lossy().into_owned();
            if let Ok(files) = std::fs::read_dir(kind.path()) {
                for f in files.flatten() {
                    let fname = f.file_name().to_string_lossy().into_owned();
                    if fname.ends_with(".md") {
                        out.push(format!("{kname}/{fname}"));
                    }
                }
            }
        }
    }
    out
}

/// `sources/<id>` directory names, unbound → empty.
fn source_candidates(srv: &mut Server) -> Vec<String> {
    let Ok(wiki) = bound_wiki(srv) else {
        return Vec::new();
    };
    let base = wiki.p("sources");
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&base) {
        for e in entries.flatten() {
            if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                out.push(e.file_name().to_string_lossy().into_owned());
            }
        }
    }
    out
}
