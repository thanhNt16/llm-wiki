//! Derived-page front matter — minimal port of `wikicore/synthesize.py`'s
//! `_split_front` + `PAGE_KINDS`/`KIND_TYPE` and the `wikicore/yamlite.py`
//! parse/dump subset those callers need. Unknown fields are tolerated
//! (parsed into the map, never surfaced). Canonical data never passes
//! through here — this is for `wiki/<kind>/*.md` pages only.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};

use crate::err::EngineError;

/// synthesize.py:19 — page kinds with a `wiki/<kind>/` section.
pub const PAGE_KINDS: &[&str] = &[
    "concepts",
    "entities",
    "procedures",
    "questions",
    "sources",
    "changes",
];

/// synthesize.py:21 — plural dir name → singular type label.
pub fn kind_type(kind: &str) -> &'static str {
    match kind {
        "concepts" => "concept",
        "entities" => "entity",
        "procedures" => "procedure",
        "questions" => "question",
        "sources" => "source",
        "changes" => "change",
        _ => "concept",
    }
}

/// synthesize.py:41 `_FRONT_RE` — fences must be `---` lines.
static FRONT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?ms)\A---\s*\n(.*?)^---[ \t]*\n?").unwrap());

/// `synthesize._split_front` — return (frontmatter map, body) for a markdown
/// page. Malformed front matter raises the same TxnError yamlite would
/// (callers in query.py/graphdata.py let it propagate).
pub fn split_front(text: &str) -> Result<(Map<String, Value>, &str), EngineError> {
    match FRONT_RE.captures(text) {
        None => Ok((Map::new(), text)),
        Some(caps) => {
            let m = caps.get(0).unwrap();
            Ok((loads(&caps[1])?, &text[m.end()..]))
        }
    }
}

/// Body after the opening `---` fence block, if one is present — used by
/// `doctor --fix` to recover the body of a page whose frontmatter fails to
/// parse (`malformed_frontmatter` default-repair keeps the body verbatim).
pub fn body_after_fence(text: &str) -> &str {
    match FRONT_RE.captures(text) {
        None => text,
        Some(caps) => &text[caps.get(0).unwrap().end()..],
    }
}

/// Python truthiness for a front-matter scalar (`front.get("stale")` etc.).
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n
            .as_i64()
            .map(|i| i != 0)
            .unwrap_or_else(|| n.as_f64().map(|f| f != 0.0).unwrap_or(true)),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

// --- yamlite.loads port -------------------------------------------------

static INT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^-?\d+$").unwrap());
static FLOAT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^-?\d+\.\d+$").unwrap());

/// `_parse_scalar` (yamlite.py:81) — null/bool/int/float/quoted-string only.
fn parse_scalar(text: &str) -> Value {
    if text.is_empty() || text == "null" {
        return Value::Null;
    }
    if text == "true" {
        return Value::Bool(true);
    }
    if text == "false" {
        return Value::Bool(false);
    }
    if text.len() >= 2 && text.starts_with('\'') && text.ends_with('\'') {
        return Value::String(unescape(&text[1..text.len() - 1]));
    }
    if INT_RE.is_match(text) {
        if let Ok(i) = text.parse::<i64>() {
            return Value::from(i);
        }
        return Value::String(text.to_string());
    }
    if FLOAT_RE.is_match(text) {
        if let Ok(f) = text.parse::<f64>() {
            return Value::from(f);
        }
        return Value::String(text.to_string());
    }
    if text == "[]" || text == "['']" {
        return if text == "[]" {
            Value::Array(vec![])
        } else {
            Value::Array(vec![Value::String(String::new())])
        };
    }
    Value::String(text.to_string())
}

/// `_unescape` (yamlite.py:30) — `''` → `'`, `\n`/`\r`/`\\` escapes; an
/// unknown escape keeps backslash + char verbatim.
fn unescape(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        let ch = chars[i];
        if ch == '\'' && chars.get(i + 1) == Some(&'\'') {
            out.push('\'');
            i += 2;
        } else if ch == '\\' && i + 1 < chars.len() {
            let n = chars[i + 1];
            match n {
                'n' => out.push('\n'),
                'r' => out.push('\r'),
                '\\' => out.push('\\'),
                _ => {
                    out.push('\\');
                    out.push(n);
                }
            }
            i += 2;
        } else {
            out.push(ch);
            i += 1;
        }
    }
    out
}

/// `yamlite.loads` — restricted flat front-matter reader: scalars, scalar
/// lists (`  - item`), and one-level nested mappings (indented `k: v` under
/// a bare `key:`). Body is the already-extracted fence interior.
fn loads(body: &str) -> Result<Map<String, Value>, EngineError> {
    let mut result = Map::new();
    let mut current_key: Option<String> = None;
    let mut pending_block: Option<String> = None;
    for line in body.lines() {
        let stripped = line.trim();
        if stripped.is_empty() || stripped.starts_with('#') {
            continue;
        }
        if let Some(item) = line.strip_prefix("  - ") {
            let key = current_key.clone().ok_or_else(|| {
                EngineError::Txn(format!("yamlite: list item without a key: {line:?}"))
            })?;
            match result.get_mut(&key) {
                Some(Value::Array(a)) => {
                    a.push(parse_scalar(item.trim()));
                    pending_block = None;
                    continue;
                }
                _ => {
                    return Err(EngineError::Txn(format!(
                        "yamlite: list item under scalar key {key:?}: {line:?}"
                    )))
                }
            }
        }
        if line.starts_with(' ') && pending_block.is_some() && line.contains(':') {
            // one-level nested mapping (e.g. OKF `generated: {by, at}`)
            let key = pending_block.clone().unwrap();
            let (k, raw) = stripped.split_once(':').unwrap();
            let entry = result
                .entry(key.clone())
                .or_insert_with(|| Value::Object(Map::new()));
            if !entry.is_object() {
                *entry = Value::Object(Map::new());
            }
            entry
                .as_object_mut()
                .unwrap()
                .insert(k.trim().to_string(), parse_scalar(raw.trim()));
            continue;
        }
        if !line.contains(':') {
            return Err(EngineError::Txn(format!("yamlite: cannot parse line: {line:?}")));
        }
        let (key, raw) = line.split_once(':').unwrap();
        let key = key.trim().to_string();
        let raw = raw.trim();
        current_key = Some(key.clone());
        pending_block = if raw.is_empty() { Some(key.clone()) } else { None };
        if raw.is_empty() || raw == "[]" {
            result.insert(key, Value::Array(vec![]));
        } else {
            let v = parse_scalar(raw);
            let is_list = v.is_array();
            result.insert(key, v);
            if is_list {
                current_key = None;
            }
        }
    }
    Ok(result)
}

// --- yamlite._dump_scalar port (used by okf.rs) -------------------------

/// `_NONSTRING_SCALAR_RE` — scalars that would re-parse as non-strings get
/// quoted so the string value survives the round trip.
static NONSTRING_SCALAR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:null|true|false|-?\d+|-?\d+\.\d+|\[\]|\[''\])$").unwrap()
});

/// `_escape` — backslash, quote doubling, newline/carriage-return.
fn escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('\'', "''")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
}

/// `_quote` — emit bare unless the scalar needs single-quoting.
fn quote(value: &str) -> String {
    let needs = value.is_empty()
        || value.starts_with(' ')
        || value.starts_with('\'')
        || value.starts_with('#')
        || value.ends_with(' ')
        || value.contains(": ")
        || value.contains(" #")
        || value.contains('\'')
        || value.contains('\\')
        || value.contains('\n')
        || value.contains('\r')
        || NONSTRING_SCALAR_RE.is_match(value);
    if needs {
        format!("'{}'", escape(value))
    } else {
        value.to_string()
    }
}

/// `_dump_scalar` restricted to the shapes okf emits (str here; other
/// scalar kinds render via `py_str` at the call site).
pub fn dump_scalar_str(value: &str) -> String {
    quote(value)
}

// --- yamlite.dumps port (write-page / build-pages) ----------------------

/// `_dump_scalar` — null/bool/int/float/str emission; floats keep Python
/// `repr` shape via `py_str`.
fn dump_scalar(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        Value::Number(_) => crate::util::py_str(v),
        other => quote(&crate::util::py_str(other)),
    }
}

/// `yamlite.dumps` — flat front-matter writer: scalars + scalar lists.
/// Nested dicts/lists raise the yamlite ValueError (a Txn here).
pub fn dumps(data: &Map<String, Value>) -> Result<String, EngineError> {
    let mut lines = Vec::new();
    for (key, value) in data {
        match value {
            Value::Object(_) => {
                return Err(EngineError::Txn(format!(
                    "yamlite: nested dicts not supported (key {key:?})"
                )))
            }
            Value::Array(items) => {
                if items.iter().any(|i| i.is_object() || i.is_array()) {
                    return Err(EngineError::Txn(format!(
                        "yamlite: nested lists not supported (key {key:?})"
                    )));
                }
                if items.is_empty() {
                    lines.push(format!("{key}: []"));
                } else {
                    lines.push(format!("{key}:"));
                    for item in items {
                        lines.push(format!("  - {}", dump_scalar(item)));
                    }
                }
            }
            other => lines.push(format!("{key}: {}", dump_scalar(other))),
        }
    }
    Ok(lines.join("\n"))
}

/// `_page` / write_page render — `"---\n<yamlite>\n---\n\n<body.strip()>\n"`.
pub fn page_text(front: &Map<String, Value>, body: &str) -> Result<String, EngineError> {
    Ok(format!(
        "---\n{}\n---\n\n{}\n",
        dumps(front)?,
        body.trim()
    ))
}
