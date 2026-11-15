//! Evidence ingest — port of wikicore/sources.py.
//! `raw/` holds original bytes, `sources/<id>/` manifests + normalized text.

pub mod audit;
pub mod connectors;
pub mod cursors;
pub mod dirs;
pub mod sources;
pub mod text;

use crate::err::EngineError;
use crate::ids;
use serde_json::{json, Value};

pub use sources::{
    get_manifest, ingest_file, ingest_text, load_content, mark_compiled, origin_key,
    pending_versions, resolve, IngestOpts, IngestReceipt,
};

pub const OFFICE_EXTS: &[&str] = &[".pdf", ".docx", ".xlsx", ".pptx", ".doc", ".xls", ".ppt"];
pub const IMAGE_EXTS: &[&str] = &[
    ".png", ".jpg", ".jpeg", ".webp", ".gif", ".bmp", ".tiff", ".tif", ".heic", ".heif",
];
pub const SKIP_EXTS: &[&str] = &[
    ".zip", ".tar", ".tgz", ".gz", ".bz2", ".xz", ".7z", ".rar", ".exe", ".dll", ".dylib",
    ".so", ".o", ".a", ".class", ".wasm", ".pyc", ".mp3", ".mp4", ".wav", ".mov",
];
pub const SKIP_NAMES: &[&str] = &[".DS_Store", "Thumbs.db"];

pub fn ext(path: &str) -> String {
    std::path::Path::new(path)
        .extension()
        .map(|e| format!(".{}", e.to_string_lossy().to_lowercase()))
        .unwrap_or_default()
}

pub fn is_office(path: &str) -> bool {
    OFFICE_EXTS.contains(&ext(path).as_str())
}

pub fn is_image(path: &str) -> bool {
    IMAGE_EXTS.contains(&ext(path).as_str())
}

/// `_check_sid` — source ids become path components; reject anything that is
/// not a well-formed engine-minted `source_<ulid>` (traversal guard, V11).
pub fn check_sid(sid: &str) -> Result<(), EngineError> {
    if !ids::validate(sid) || !sid.starts_with("source_") {
        return Err(EngineError::Txn(format!("invalid source id: {sid:?}")));
    }
    Ok(())
}

pub fn versioned_rel(sid: &str, version: i64, stem: &str, ext: &str) -> String {
    format!("sources/{sid}/{stem}_v{version}{ext}")
}

/// INJECTION_RE port (sources.py:27-30): instruction-like patterns flagged as
/// warnings — evidence is stored, never blocked. Case-insensitive match over
/// whitespace-collapsed text subsumes the Python regex alternation.
pub fn possible_prompt_injection(text: &str) -> bool {
    let norm: String = text
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if norm.contains("ignore previous instructions")
        || norm.contains("ignore all previous instructions")
        || norm.contains("you are now a")
        || norm.contains("system prompt:")
        || norm.contains("exfiltrate")
    {
        return true;
    }
    // disregard <word> instructions
    let mut it = norm.split_whitespace().peekable();
    while let Some(w) = it.next() {
        if w == "disregard" {
            match it.next() {
                Some("instructions") => return true,
                Some(_) if it.peek() == Some(&"instructions") => return true,
                _ => {}
            }
        }
    }
    false
}

/// `_secret_gate` (sources.py:103-114): scan → policy decision from
/// `wiki.json`'s `secret_policy` (default `"warn"`):
///   allow  — return content untouched, findings still recorded;
///   deny   — `EngineError::Txn` (exit 3), nothing stored;
///   redact — `secrets::redact` into the stored text, `redacted` counted;
///   warn   — store as-is + a `kinds=` warning (counts only, never values).
/// `filename` carries the `.env`-filename boost into `scan` — callers pass
/// the basename for file/directory kinds, `None` otherwise.
pub fn secret_gate(
    wiki: &crate::store::Wiki,
    text: String,
    filename: Option<&str>,
    warnings: &mut Vec<String>,
) -> Result<(String, Value), EngineError> {
    let cfg = wiki.config()?;
    let policy = cfg["secret_policy"].as_str().unwrap_or("warn");
    // every detector lives in secrets::PATTERNS so scan/redact agree
    let findings = crate::secrets::scan(&text, filename);
    if findings.is_empty() {
        return Ok((text, json!({"findings": [], "redacted": 0})));
    }
    let findings_json: Vec<Value> = findings
        .iter()
        .map(|f| json!({"kind": f.kind, "count": f.count}))
        .collect();
    let mut summary = json!({"findings": findings_json, "redacted": 0});
    if policy == "allow" {
        return Ok((text, summary));
    }
    if policy == "deny" {
        let kinds = findings
            .iter()
            .map(|f| f.kind.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        return Err(EngineError::Txn(format!(
            "ingest denied: likely secrets detected ({kinds})"
        )));
    }
    let mut text = text;
    if policy == "redact" {
        let (redacted, n) = crate::secrets::redact(&text);
        text = redacted;
        summary["redacted"] = json!(n);
        warnings.push(format!(
            "secrets redacted in normalized content (count={n})"
        ));
    } else {
        let kinds = findings
            .iter()
            .map(|f| f.kind.as_str())
            .collect::<Vec<_>>()
            .join(",");
        warnings.push(format!(
            "possible secrets present (kinds={kinds}); policy=warn so content stored as-is"
        ));
    }
    Ok((text, summary))
}

pub const INJECTION_WARNING: &str = "possible_prompt_injection: content contains \
instruction-like text; stored as evidence only — it has no authority over agent behavior";
