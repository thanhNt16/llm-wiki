//! Office-format extraction (spec §8): docx/pptx/odt → markdown,
//! xlsx/ods → markdown pipe tables.
//!
//! This is a capability upgrade, not a transliteration: Python routed every
//! office file through `markitdown`. The Rust-native readers cover only
//! `docx/pptx/xlsx/odt/ods`; legacy `.doc/.xls/.ppt` and `.pdf` keep the
//! markitdown lane (`detect_ext` returns `None`) and land as
//! `not_extracted` + warning when markitdown is absent.
//!
//! Extraction errors surface as `EngineError::Txn` — the ingest caller
//! turns `Err` into `not_extracted` + a warning, the same shape as a
//! missing markitdown. Stored extraction metadata uses
//! `parser.name = "rust-oxml"` (gates assert the contract, never
//! byte-equal markdown).

pub(crate) mod odf;
pub(crate) mod ooxml;
pub(crate) mod xlsx;
pub(crate) mod zip;

use quick_xml::events::BytesStart;
use quick_xml::Decoder;

use crate::err::EngineError;
use crate::evidence::ext;

/// `parser.name` recorded on extractions produced here (spec §8 contract).
pub const PARSER_NAME: &str = "rust-oxml";

/// Zip-container formats with a Rust-native reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Docx,
    Pptx,
    Xlsx,
    Odt,
    Ods,
}

/// Map a filename/ref to a Rust-readable office format. Anything else —
/// including legacy `.doc/.xls/.ppt` and `.pdf` — is `None` (markitdown
/// lane or `not_extracted`).
pub fn detect_ext(name: &str) -> Option<Format> {
    match ext(name).as_str() {
        ".docx" => Some(Format::Docx),
        ".pptx" => Some(Format::Pptx),
        ".xlsx" => Some(Format::Xlsx),
        ".odt" => Some(Format::Odt),
        ".ods" => Some(Format::Ods),
        _ => None,
    }
}

/// Detect + extract in one step for the ingest hook: `Ok(None)` when the
/// extension isn't Rust-readable, `Ok(Some(md))` on success.
pub fn extract_named(name: &str, bytes: &[u8]) -> Result<Option<String>, EngineError> {
    detect_ext(name)
        .map(|f| extract(f, bytes))
        .transpose()
}

/// Extract `bytes` (a zip container) to markdown text for `format`.
pub fn extract(format: Format, bytes: &[u8]) -> Result<String, EngineError> {
    let z = zip::ZipArchive::open(bytes)?;
    let md = match format {
        Format::Docx => ooxml::docx(&z)?,
        Format::Pptx => ooxml::pptx(&z)?,
        Format::Xlsx => xlsx::xlsx(&z)?,
        Format::Odt => odf::odf(&z)?,
        Format::Ods => odf::odf(&z)?,
    };
    Ok(strip_forbidden(&md))
}

/// Last-attribute lookup by local name (`w:val`, `r:id`, `table:name` …).
/// Element selection keeps namespaces distinct, so a local-name match is
/// precise enough for these vocabularies.
pub(crate) fn attr(e: &BytesStart, local: &str, dec: Decoder) -> Option<String> {
    for a in e.attributes().flatten() {
        if a.key.local_name().as_ref() == local.as_bytes() {
            return a.decode_and_unescape_value(dec).ok().map(|v| v.into_owned());
        }
    }
    None
}

/// XML-1.0 forbidden characters are stripped from extracted output:
/// C0/C1 controls minus tab/newline/CR, U+FFFE/U+FFFF (spec §8 — the
/// office XML itself tolerates them; our markdown must not carry them).
pub(crate) fn strip_forbidden(s: &str) -> String {
    s.chars()
        .filter(|&c| {
            !(('\u{0}'..='\u{8}').contains(&c)
                || ('\u{B}'..='\u{C}').contains(&c)
                || ('\u{E}'..='\u{1F}').contains(&c)
                || ('\u{7F}'..='\u{9F}').contains(&c)
                || c == '\u{FFFE}'
                || c == '\u{FFFF}')
        })
        .collect()
}

/// Escape one cell for a markdown pipe table.
pub(crate) fn md_cell(s: &str) -> String {
    strip_forbidden(s)
        .replace('|', "\\|")
        .replace('\n', " ")
        .replace('\r', " ")
        .trim()
        .to_string()
}

/// Render rows as a pipe table; column count is the widest row.
pub(crate) fn md_table(rows: &[Vec<String>]) -> String {
    let mut out = String::new();
    if rows.is_empty() {
        return out;
    }
    let cols = rows.iter().map(Vec::len).max().unwrap_or(0);
    if cols == 0 {
        return out;
    }
    for (i, row) in rows.iter().enumerate() {
        out.push('|');
        for c in 0..cols {
            out.push(' ');
            out.push_str(&md_cell(row.get(c).map(String::as_str).unwrap_or("")));
            out.push_str(" |");
        }
        out.push('\n');
        if i == 0 {
            out.push('|');
            for _ in 0..cols {
                out.push_str(" --- |");
            }
            out.push('\n');
        }
    }
    out
}

/// A1-style column reference → 0-based column index.
pub(crate) fn col_idx(cell_ref: &str) -> Option<usize> {
    let mut n = 0usize;
    let mut seen = false;
    for ch in cell_ref.chars() {
        if ch.is_ascii_alphabetic() {
            n = n * 26 + (ch.to_ascii_uppercase() as usize - 'A' as usize + 1);
            seen = true;
        } else {
            break;
        }
    }
    seen.then(|| n - 1)
}
