//! Minimal JSON Schema (draft-07 subset) validator — port of
//! `wikicore/schema.py`, stdlib-free but dependency-light (serde_json +
//! regex). `validate` returns human-readable error strings (empty == valid).
//!
//! Supported keywords (parity list): type, required, properties,
//! patternProperties, additionalProperties, propertyNames, minProperties,
//! maxProperties, enum, const, items, pattern, minLength, maxLength,
//! minimum, maximum, minItems, uniqueItems. Schemas are embedded via
//! `include_str!` — the binary cannot reach the Python skill tree.

use serde_json::{Map, Value};

/// Embedded `claim.schema.json` (skill/llm-wiki/schemas/ copy).
pub const CLAIM_SCHEMA: &str = include_str!("../schemas/claim.schema.json");
/// Embedded `decision.schema.json`.
pub const DECISION_SCHEMA: &str = include_str!("../schemas/decision.schema.json");
/// Embedded `review-item.schema.json`.
pub const REVIEW_ITEM_SCHEMA: &str = include_str!("../schemas/review-item.schema.json");

fn type_ok(value: &Value, expected: &Value) -> bool {
    if let Some(list) = expected.as_array() {
        return list.iter().any(|t| type_ok(value, t));
    }
    match expected.as_str().unwrap_or("") {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        // JSON-schema integer: integral number, never boolean.
        "integer" => value.is_i64() || value.is_u64(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "null" => value.is_null(),
        _ => true,
    }
}

/// Python `==` parity for JSON values: numeric equality across int/float
/// (`5 == 5.0`), everything else structural.
fn json_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Number(x), Value::Number(y)) => {
            match (x.as_f64(), y.as_f64()) {
                (Some(x), Some(y)) => x == y,
                _ => x == y,
            }
        }
        _ => a == b,
    }
}

fn json_key(item: &Value) -> String {
    // schema.py json_key — sort_keys dump; serde_json Map sorts keys.
    serde_json::to_string(item).unwrap_or_default()
}

/// Python `repr` for the values error strings embed (`%r` parity).
pub fn py_repr(v: &Value) -> String {
    match v {
        Value::String(s) => py_repr_str(s),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Null => "None".to_string(),
        Value::Number(n) => n.to_string(),
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(py_repr).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(m) => {
            let mut items: Vec<String> = Vec::new();
            for (k, v) in m {
                items.push(format!("{}: {}", py_repr_str(k), py_repr(v)));
            }
            format!("{{{}}}", items.join(", "))
        }
    }
}

/// Python `repr` of a string: single quotes, switch to double when the
/// value contains `'` but no `"`.
pub fn py_repr_str(s: &str) -> String {
    let has_single = s.contains('\'');
    let (q, escaped) = if has_single && !s.contains('"') {
        ('"', s.to_string())
    } else {
        ('\'', s.replace('\\', "\\\\").replace('\'', "\\'"))
    };
    format!("{q}{escaped}{q}")
}

/// Python `str(v)` for JSON values — used for `str(dep).strip()` parity.
pub fn py_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Null => "None".to_string(),
        Value::Number(n) => n.to_string(),
        other => py_repr(other),
    }
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64()
}

fn check(value: &Value, prop_schema: &Map<String, Value>, path: &str, errors: &mut Vec<String>) {
    let where_ = if path.is_empty() { "root" } else { path };

    if let Some(c) = prop_schema.get("const") {
        if !json_eq(value, c) {
            errors.push(format!("{where_}: must equal {}", py_repr(c)));
            return;
        }
    }

    if let Some(e) = prop_schema.get("enum") {
        let ok = e.as_array().map(|a| a.iter().any(|x| json_eq(value, x))).unwrap_or(false);
        if !ok {
            errors.push(format!(
                "{where_}: {} not in enum {}",
                py_repr(value),
                py_repr(e)
            ));
            return;
        }
    }

    if let Some(t) = prop_schema.get("type") {
        if !type_ok(value, t) {
            errors.push(format!("{where_}: expected type {}", py_repr(t)));
            return;
        }
    }

    if let Value::String(s) = value {
        if let Some(n) = prop_schema.get("minLength").and_then(num) {
            if (s.chars().count() as f64) < n {
                errors.push(format!("{where_}: shorter than minLength {}", n as i64));
            }
        }
        if let Some(n) = prop_schema.get("maxLength").and_then(num) {
            if (s.chars().count() as f64) > n {
                errors.push(format!("{where_}: longer than maxLength {}", n as i64));
            }
        }
        if let Some(pat) = prop_schema.get("pattern").and_then(Value::as_str) {
            let ok = regex::Regex::new(pat).map(|re| re.is_match(s)).unwrap_or(false);
            if !ok {
                errors.push(format!("{where_}: {} fails pattern {}", py_repr_str(s), py_repr_str(pat)));
            }
        }
    }

    if value.is_number() {
        let v = value.as_f64().unwrap_or(f64::NAN);
        if let Some(n) = prop_schema.get("minimum").and_then(num) {
            if v < n {
                errors.push(format!("{where_}: {} < minimum {}", py_repr(value), py_repr(prop_schema.get("minimum").unwrap())));
            }
        }
        if let Some(n) = prop_schema.get("maximum").and_then(num) {
            if v > n {
                errors.push(format!("{where_}: {} > maximum {}", py_repr(value), py_repr(prop_schema.get("maximum").unwrap())));
            }
        }
    }

    if let Value::Array(items) = value {
        if let Some(n) = prop_schema.get("minItems").and_then(num) {
            if (items.len() as f64) < n {
                errors.push(format!("{where_}: needs >= {} items", n as i64));
            }
        }
        if prop_schema.get("uniqueItems").and_then(Value::as_bool).unwrap_or(false) {
            let seen: std::collections::HashSet<String> =
                items.iter().map(json_key).collect();
            if seen.len() != items.len() {
                errors.push(format!("{where_}: items not unique"));
            }
        }
        if let Some(Value::Object(item_schema)) = prop_schema.get("items") {
            for (i, item) in items.iter().enumerate() {
                check(item, item_schema, &format!("{where_}[{i}]"), errors);
            }
        }
    }

    if let Value::Object(obj) = value {
        if let Some(reqs) = prop_schema.get("required").and_then(Value::as_array) {
            for req in reqs {
                if let Some(r) = req.as_str() {
                    if !obj.contains_key(r) {
                        errors.push(format!("{where_}: missing required property {}", py_repr_str(r)));
                    }
                }
            }
        }
        if let Some(n) = prop_schema.get("minProperties").and_then(num) {
            if (obj.len() as f64) < n {
                errors.push(format!("{where_}: fewer than minProperties {}", n as i64));
            }
        }
        if let Some(n) = prop_schema.get("maxProperties").and_then(num) {
            if (obj.len() as f64) > n {
                errors.push(format!("{where_}: more than maxProperties {}", n as i64));
            }
        }
        let props = prop_schema.get("properties").and_then(Value::as_object);
        let patterns = prop_schema.get("patternProperties").and_then(Value::as_object);
        let name_schema = prop_schema.get("propertyNames").and_then(Value::as_object);
        let addl = prop_schema.get("additionalProperties");
        for (key, sub) in obj {
            if let Some(ns) = name_schema {
                check(&Value::String(key.clone()), ns, &format!("{where_}.<key {}>", py_repr_str(key)), errors);
            }
            let mut matched = false;
            if let Some(p) = props.and_then(|p| p.get(key)).and_then(Value::as_object) {
                matched = true;
                check(sub, p, &format!("{where_}.{key}"), errors);
            }
            if let Some(pats) = patterns {
                for (pat, psub) in pats {
                    let hits = regex::Regex::new(pat).map(|re| re.is_match(key)).unwrap_or(false);
                    if hits {
                        if let Some(ps) = psub.as_object() {
                            matched = true;
                            check(sub, ps, &format!("{where_}.{key}"), errors);
                        }
                    }
                }
            }
            if matched {
                continue;
            }
            match addl {
                Some(Value::Bool(false)) => {
                    errors.push(format!("{where_}: unexpected property {}", py_repr_str(key)));
                }
                Some(a @ Value::Object(_)) => {
                    check(sub, a.as_object().unwrap(), &format!("{where_}.{key}"), errors);
                }
                _ => {}
            }
        }
    }
}

/// `schema.validate` — empty list == valid.
pub fn validate(obj: &Value, schema: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    if let Value::Object(s) = schema {
        check(obj, s, "", &mut errors);
    }
    errors
}
