//! Shorthand candidate parsing for the compile umbrella — port of
//! wikicore/shorthand.py verbatim. Agents write one JSON object per line
//! with only semantic fields; this module expands locator/authority
//! shorthand and stamps engine-owned fields.

use serde_json::{json, Value};

use crate::err::EngineError;
use crate::util;

fn py_type(v: &Value) -> &'static str {
    match v {
        Value::Null => "NoneType",
        Value::Bool(_) => "bool",
        Value::Number(n) if n.is_f64() => "float",
        Value::Number(_) => "int",
        Value::String(_) => "str",
        Value::Array(_) => "list",
        Value::Object(_) => "dict",
    }
}

fn authority_type(key: &str) -> Option<&'static str> {
    Some(match key {
        "manual" | "doc" | "config" => "authoritative_source",
        "decision" | "adr" => "explicit_project_decision",
        "code" | "verified" => "implementation_verification",
        "inferred" => "agent_inference",
        _ => return None,
    })
}

fn authority_source(key: &str) -> &'static str {
    // shorthand.py:_AUTHORITY_SOURCE — identity save "inferred"->"agent".
    match key {
        "manual" => "manual",
        "doc" => "doc",
        "config" => "config",
        "decision" => "decision",
        "adr" => "adr",
        "code" => "code",
        "verified" => "verified",
        _ => "agent",
    }
}

/// `_ALLOWED_FIELDS` (shorthand.py:30-34) — exactly the keys
/// normalize_candidate reads/defaults; unknown keys are rejected at
/// staging instead of silently persisting.
pub const ALLOWED_FIELDS: &[&str] = &[
    "authority",
    "evidence",
    "id",
    "locator",
    "predicate",
    "proposed_by",
    "recorded_at",
    "root_origin",
    "scope",
    "status",
    "subject",
    "supersedes",
    "unit",
    "valid_from",
    "valid_to",
    "value",
];

/// `expand_locator` (shorthand.py:36-46) — "h:<heading>" | "l:<lines>" |
/// "s:<text>" shorthand into `{type, value}`; dicts pass through.
pub fn expand_locator(raw: &Value) -> Result<Value, EngineError> {
    if raw.is_object() {
        return Ok(raw.clone());
    }
    let Some(s) = raw.as_str() else {
        return Err(EngineError::Txn(format!(
            "locator must be a string shorthand or object, got {}",
            py_type(raw)
        )));
    };
    // re.match(r"^([hls]):(.+)$", raw, re.S) — one shorthand char, a
    // colon, then a non-empty value.
    let (k, val) = match s.as_bytes() {
        [c, b':', rest @ ..] if !rest.is_empty() => (*c as char, s[2..].trim()),
        _ => {
            return Err(EngineError::Txn(format!(
                "bad locator shorthand {s:?} — use h:<heading>, l:<lines>, s:<text>"
            )))
        }
    };
    let t = match k {
        'h' => "heading",
        'l' => "line_range",
        's' => "text",
        _ => {
            return Err(EngineError::Txn(format!(
                "bad locator shorthand {s:?} — use h:<heading>, l:<lines>, s:<text>"
            )))
        }
    };
    Ok(json!({"type": t, "value": val}))
}

/// `expand_authority` (shorthand.py:49-58) — "manual|doc|config|decision|
/// adr|code|verified|inferred[:<src>]" into `{type, source}`; dicts pass
/// through.
pub fn expand_authority(raw: &Value) -> Result<Value, EngineError> {
    if raw.is_object() {
        return Ok(raw.clone());
    }
    let Some(s) = raw.as_str() else {
        return Err(EngineError::Txn(format!(
            "authority must be a shorthand string or object, got {}",
            py_type(raw)
        )));
    };
    let (key0, src) = s.split_once(':').unwrap_or((s, ""));
    let key = key0.trim().to_lowercase();
    let Some(atype) = authority_type(&key) else {
        return Err(EngineError::Txn(format!(
            "bad authority shorthand {s:?} — one of [\"adr\", \"code\", \"config\", \"decision\", \"doc\", \"inferred\", \"manual\", \"verified\"]"
        )));
    };
    let src_t = src.trim();
    Ok(json!({
        "type": atype,
        "source": if src_t.is_empty() { authority_source(&key).to_string() } else { src_t.to_string() },
    }))
}

/// `normalize_candidate` (shorthand.py:61-98) — shorthand dict -> full
/// candidate. Engine stamps id later (staging); recorded_at/status/
/// proposed_by/evidence/root_origin default here. Idempotent over full
/// candidates (setdefault parity).
pub fn normalize_candidate(
    c: &Value,
    source_id: &str,
    source_version: i64,
    root_origin: &str,
) -> Result<Value, EngineError> {
    let Some(obj) = c.as_object() else {
        return Err(EngineError::Txn("candidate must be an object".into()));
    };
    for field in ["subject", "predicate"] {
        if !truthy(&c[field]) {
            return Err(EngineError::Txn(format!("candidate missing '{field}'")));
        }
    }
    let mut bad: Vec<&String> = obj
        .keys()
        .filter(|k| !ALLOWED_FIELDS.contains(&k.as_str()))
        .collect();
    if !bad.is_empty() {
        bad.sort();
        return Err(EngineError::Txn(format!(
            "unknown candidate field(s): {} — valid: {}",
            bad.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", "),
            ALLOWED_FIELDS.join(", ")
        )));
    }
    let mut out = obj.clone();
    out.insert(
        "subject".to_string(),
        json!(crate::schema::py_str(&c["subject"]).trim().to_string()),
    );
    out.insert(
        "predicate".to_string(),
        json!(crate::schema::py_str(&c["predicate"]).trim().to_string()),
    );
    if !out.contains_key("evidence") {
        let loc = expand_locator(
            &out.remove("locator").unwrap_or(json!("s:whole-document")),
        )?;
        out.insert(
            "evidence".to_string(),
            json!([{
                "source_id": source_id,
                "source_version": source_version,
                "locator": loc,
            }]),
        );
    } else {
        let default_loc = expand_locator(
            &out.remove("locator").unwrap_or(json!("s:whole-document")),
        )?;
        if let Some(evs) = out.get_mut("evidence").and_then(Value::as_array_mut) {
            for ev in evs.iter_mut() {
                if let Some(eo) = ev.as_object_mut() {
                    eo.entry("locator".to_string())
                        .or_insert_with(|| default_loc.clone());
                }
            }
        }
    }
    let auth = out
        .remove("authority")
        .unwrap_or(json!({"type": "authoritative_source", "source": "document"}));
    out.insert("authority".to_string(), expand_authority(&auth)?);
    for (k, v) in [
        ("scope", json!({})),
        ("valid_from", Value::Null),
        ("valid_to", Value::Null),
        ("supersedes", json!([])),
        ("recorded_at", json!(util::now_iso())),
        ("status", json!("candidate")),
        ("proposed_by", json!("agent")),
        ("root_origin", json!(root_origin)),
    ] {
        out.entry(k.to_string()).or_insert(v);
    }
    Ok(Value::Object(out))
}

/// Python truthiness (`if not c.get(field)`): Null/false/0/""/[]/{} are falsy.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// `parse_candidates_file` (shorthand.py:101-125) — `.jsonl` yields one
/// shorthand object per line; `.json` an array or `{candidates: [...]}`.
/// Returns raw dicts (normalization happens at staging).
pub fn parse_candidates_file(path: &std::path::Path) -> Result<Vec<Value>, EngineError> {
    let name = path.to_string_lossy();
    if name.ends_with(".jsonl") {
        let text = std::fs::read_to_string(path)?;
        let mut out = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let obj: Value = serde_json::from_str(line).map_err(|e| {
                EngineError::Txn(format!("candidates file {name} line {}: {e}", n + 1))
            })?;
            if !obj.is_object() {
                return Err(EngineError::Txn(format!(
                    "candidates file {name} line {}: not an object",
                    n + 1
                )));
            }
            out.push(obj);
        }
        return Ok(out);
    }
    let text = std::fs::read_to_string(path)?;
    let data: Value = serde_json::from_str(&text)?;
    let data = match data {
        Value::Object(o) => o.get("candidates").cloned().unwrap_or(json!([])),
        other => other,
    };
    let Value::Array(rows) = data else {
        return Err(EngineError::Txn(format!(
            "candidates file {name}: expected array"
        )));
    };
    Ok(rows)
}
