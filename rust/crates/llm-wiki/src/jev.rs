//! Jev (TypeSafe System One) — advisory decision client
//! (`skill/llm-wiki/scripts/wikicore/jev.py` port, spec §3).
//!
//! Env-gated: `DECISION_TOKEN` + `DECISION_MODEL` enable the client,
//! `DECISION_ENDPOINT` overrides the default URL. `ask` never raises —
//! transport, parse, and cache failures all degrade to `{ok:false}` (or an
//! absent advisory field at the call sites). Successful answers are cached
//! in `.state/index.sqlite` `jev_answers` (spec §10.1), keyed by
//! `sha256(endpoint + model + state + questions)` — the token is
//! deliberately excluded so token rotation reuses cached verdicts.
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::store::Wiki;
use crate::index::Index;
use crate::util;

pub const TOKEN_ENV: &str = "DECISION_TOKEN";
pub const MODEL_ENV: &str = "DECISION_MODEL";
pub const ENDPOINT_ENV: &str = "DECISION_ENDPOINT";
pub const DEFAULT_ENDPOINT: &str = "http://localhost:20128/v1/systemone";
pub const TIMEOUT_S: u64 = 10;

/// `jev.is_ready` — true when both DECISION_TOKEN and DECISION_MODEL are set.
pub fn is_ready() -> bool {
    env_nonempty(TOKEN_ENV) && env_nonempty(MODEL_ENV)
}

fn env_nonempty(name: &str) -> bool {
    std::env::var(name).map(|v| !v.is_empty()).unwrap_or(false)
}

/// `jev.endpoint` — DECISION_ENDPOINT or the localhost default.
pub fn endpoint() -> String {
    std::env::var(ENDPOINT_ENV)
        .ok()
        .filter(|v| !v.is_empty())
        .unwrap_or_else(|| DEFAULT_ENDPOINT.to_string())
}

/// `jev.missing_env` — unset required vars in canonical order (`--check`).
pub fn missing_env() -> Vec<&'static str> {
    [TOKEN_ENV, MODEL_ENV]
        .into_iter()
        .filter(|name| !env_nonempty(name))
        .collect()
}

/// `jev._fail` — the uniform not-ok shape.
fn fail(error: Option<String>, disabled: bool) -> Value {
    json!({
        "ok": false,
        "disabled": disabled,
        "answers": {},
        "usage": {},
        "error": error,
    })
}

fn jstr(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Bool(b) => if *b { "True" } else { "False" }.to_string(),
        Value::Null => String::new(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// `truthy` in the Python sense for the `or`-chains this file ports:
/// null/false/0/empty-string/empty-map/empty-array are all falsy.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().map(|f| f != 0.0).unwrap_or(true),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// Python `a or b` over JSON values (truthiness, not just null).
fn py_or<'a>(a: &'a Value, b: &'a Value) -> &'a Value {
    if truthy(a) {
        a
    } else {
        b
    }
}

/// Python `float(v)`: numbers and numeric strings; bool -> 1.0/0.0.
fn py_float(v: &Value) -> Option<f64> {
    match v {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse::<f64>().ok(),
        Value::Bool(b) => Some(if *b { 1.0 } else { 0.0 }),
        _ => None,
    }
}

/// `int(round(x))` with Python's round-half-to-even.
fn py_round_int(v: f64) -> Option<i64> {
    if !v.is_finite() || v.abs() >= 9.0e18 {
        return None;
    }
    Some(v.round_ties_even() as i64)
}

/// `jev._to_wire` — caller-friendly question shapes onto the System One
/// wire: a choice `options` list becomes `criteria` {opt: opt}; score
/// `levels: N` becomes `["level 0".."level N-1"]`. Everything else passes
/// through. (Python raises on a non-int `levels`; here it degrades —
/// `ask` must never raise.)
fn to_wire(questions: &Value) -> Value {
    let Some(obj) = questions.as_object() else {
        return questions.clone();
    };
    let mut out = Map::new();
    for (name, q) in obj {
        let Some(w) = q.as_object() else {
            out.insert(name.clone(), q.clone());
            continue;
        };
        let mut w = w.clone();
        let opts = w.remove("options");
        let levels = w.remove("levels");
        if !w.contains_key("criteria") {
            if let Some(opts) = opts.as_ref().and_then(Value::as_array) {
                let criteria: Map<String, Value> = opts
                    .iter()
                    .map(|o| (jstr(o), json!(jstr(o))))
                    .collect();
                w.insert("criteria".to_string(), Value::Object(criteria));
            } else if let Some(n) = levels.as_ref().and_then(py_float) {
                let criteria: Vec<Value> = (0..n.max(0.0) as i64)
                    .map(|i| json!(format!("level {i}")))
                    .collect();
                w.insert("criteria".to_string(), Value::Array(criteria));
            }
        }
        out.insert(name.clone(), Value::Object(w));
    }
    Value::Object(out)
}

/// `jev._normalize` — wire answer -> {"type": t, "value": v}, verbatim.
/// Unknown/malformed shapes keep their declared type with `value: null`
/// rather than raising.
fn normalize(raw: &Value) -> Value {
    let Some(obj) = raw.as_object() else {
        return json!({"type": "unknown", "value": null});
    };
    let t = obj.get("type").and_then(Value::as_str).unwrap_or("");
    match t {
        "noul" => {
            let v = obj
                .get("noul")
                .or_else(|| obj.get("value"))
                .and_then(py_float);
            match v {
                Some(f) => json!({"type": "noul", "value": f}),
                None => json!({"type": "noul", "value": null}),
            }
        }
        "choice" => {
            let null = Value::Null;
            let ch = obj.get("choice").unwrap_or(&null);
            let (dist, pick): (&Value, Option<Value>) = if ch.is_object() {
                let d = py_or(
                    ch.get("probs").unwrap_or(&null),
                    ch.get("probabilities").unwrap_or(&null),
                );
                let d = py_or(d, ch.get("distribution").unwrap_or(&null));
                let p = py_or(
                    ch.get("pick").unwrap_or(&null),
                    ch.get("answer").unwrap_or(&null),
                );
                let p = py_or(p, ch.get("value").unwrap_or(&null));
                (d, truthy(p).then(|| p.clone()))
            } else {
                let d = py_or(obj.get("probabilities").unwrap_or(&null),
                              obj.get("probs").unwrap_or(&null));
                let d = py_or(d, obj.get("distribution").unwrap_or(&null));
                let p = if ch.is_string() {
                    Some(ch.clone())
                } else {
                    obj.get("pick")
                        .filter(|v| truthy(v))
                        .cloned()
                };
                (d, p)
            };
            let pick = pick.or_else(|| argmax(dist));
            let mut value = json!({"pick": pick, "distribution": dist});
            if let Some(c) = obj.get("confidence") {
                if !c.is_null() {
                    value["confidence"] = c.clone();
                }
            }
            json!({"type": "choice", "value": value})
        }
        "score" => {
            let null = Value::Null;
            let probs = obj.get("probabilities").unwrap_or(&null);
            let sc = obj.get("score").unwrap_or(&null);
            let (level_v, expected_v): (&Value, Option<f64>) = if sc.is_object() {
                (sc.get("level").unwrap_or(&null),
                 sc.get("expected").and_then(py_float))
            } else {
                let expected = probs
                    .as_object()
                    .filter(|m| !m.is_empty())
                    .and_then(|m| {
                        let mut sum = 0.0f64;
                        for (k, p) in m {
                            sum += k.trim().parse::<f64>().ok()? * py_float(p)?;
                        }
                        Some(sum)
                    })
                    .or_else(|| (!sc.is_null()).then(|| sc).and_then(py_float));
                (sc, expected)
            };
            let level = level_v
                .as_str()
                .and_then(|s| s.trim().parse::<f64>().ok())
                .or_else(|| py_float(level_v))
                .and_then(py_round_int);
            json!({"type": "score",
                   "value": {"level": level, "expected": expected_v}})
        }
        _ => {
            if t.is_empty() {
                json!({"type": "unknown", "value": null})
            } else {
                json!({"type": t, "value": null})
            }
        }
    }
}

/// `max(dist, key=dist.get)` — pick the highest-probability option. All
/// values must coerce to f64 (Python raises otherwise; we skip -> None).
fn argmax(dist: &Value) -> Option<Value> {
    let obj = dist.as_object()?;
    let mut best: Option<(&String, f64)> = None;
    for (k, v) in obj {
        let f = py_float(v)?;
        match best {
            Some((_, bf)) if f <= bf => {}
            _ => best = Some((k, f)),
        }
    }
    best.map(|(k, _)| json!(k))
}

/// One POST to the configured endpoint (`jev._post`); returns the parsed
/// JSON body or a never-raising error string. 4xx/5xx report
/// `HTTP <code> <body[:400] | reason>` like urllib's HTTPError lane.
fn post(payload: &Value, token: &str) -> Result<Value, String> {
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(TIMEOUT_S)))
            .http_status_as_error(false)
            .redirect_auth_headers(ureq::config::RedirectAuthHeaders::SameHost)
            .build(),
    );
    let body = serde_json::to_string(payload).map_err(|e| e.to_string())?;
    let mut resp = agent
        .post(&endpoint())
        .header("Authorization", &format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .send(body.as_bytes())
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    let mut text = String::new();
    resp.body_mut()
        .as_reader()
        .take(4 * 1024 * 1024)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if !(200..300).contains(&status.as_u16()) {
        let detail = if text.is_empty() {
            status.canonical_reason().unwrap_or("").to_string()
        } else {
            text.chars().take(400).collect::<String>()
        };
        return Err(format!("HTTP {} {detail}", status.as_u16()));
    }
    serde_json::from_str(&text).map_err(|e| e.to_string())
}

/// `sha256(endpoint + model + state + questions)` — exact concatenation,
/// token excluded (spec §3/§10.1). `questions` is the serialized *wire*
/// payload so caller-friendly shapes that compile to the same wire form
/// share one cache entry.
fn cache_key(state: &str, wire_questions: &Value) -> String {
    let endpoint = endpoint();
    let model = std::env::var(MODEL_ENV).unwrap_or_default();
    let q = serde_json::to_string(wire_questions).unwrap_or_default();
    let digest = Sha256::digest(format!("{endpoint}{model}{state}{q}").as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// Open the wiki's index when `root` hosts one — else None (cache is
/// advisory; a non-wiki root or broken index silently disables it).
fn open_index(root: &Path) -> Option<Index> {
    let wiki = Wiki::open(root).ok()?;
    if !wiki.exists() {
        return None;
    }
    Index::open_or_rebuild(&wiki).ok()
}

fn cache_get(root: Option<&Path>, key: &str) -> Option<Value> {
    let root = root?;
    let idx = open_index(root)?;
    let text: String = idx
        .conn()
        .query_row(
            "SELECT answers_json FROM jev_answers WHERE key = ?1",
            [key],
            |r| r.get(0),
        )
        .ok()?;
    let stored: Value = serde_json::from_str(&text).ok()?;
    if stored.get("ok").and_then(Value::as_bool) == Some(true) {
        Some(stored)
    } else {
        None
    }
}

fn cache_put(root: Option<&Path>, key: &str, answers: &Value) {
    let Some(root) = root else { return };
    let Some(idx) = open_index(root) else { return };
    let text = match serde_json::to_string(answers) {
        Ok(t) => t,
        Err(_) => return,
    };
    let _ = idx.conn().execute(
        "INSERT OR REPLACE INTO jev_answers (key, answers_json, created_at)
         VALUES (?1, ?2, ?3)",
        rusqlite::params![key, text, util::now_iso()],
    );
}

/// `jev.ask` — ask Jev typed questions about `state`. Never raises.
///
/// Returns `{"ok", "answers": {q: {"type", "value"}}, "usage",
/// "disabled", "error"}`. Env unset -> `ok:false` + `disabled:true`.
/// Cache lookups are skipped: use [`ask_with`] when a wiki root is known.
pub fn ask(state: &str, questions: &Value) -> Value {
    ask_with(None, state, questions)
}

/// `ask` with a wiki root for the sqlite answer cache (spec §3):
/// `jev_answers[key]` hits replay the stored payload with no network;
/// misses POST once and store the successful result. Every cache failure
/// degrades to a plain live ask.
pub fn ask_with(root: Option<&Path>, state: &str, questions: &Value) -> Value {
    if !is_ready() {
        return fail(None, true);
    }
    let wire = to_wire(questions);
    let key = cache_key(state, &wire);
    if let Some(hit) = cache_get(root, &key) {
        return hit;
    }
    let payload = json!({
        "model": std::env::var(MODEL_ENV).unwrap_or_default(),
        "state": state,
        "questions": wire,
    });
    let token = std::env::var(TOKEN_ENV).unwrap_or_default();
    let body = match post(&payload, &token) {
        Ok(b) => b,
        Err(e) => return fail(Some(e), false),
    };
    if body.get("success").and_then(Value::as_bool) != Some(true) {
        let detail = body.get("error").map(jstr).unwrap_or_default();
        return fail(
            Some(if detail.is_empty() {
                "endpoint returned success != true".to_string()
            } else {
                detail
            }),
            false,
        );
    }
    let null = Value::Null;
    let empty = json!({});
    let result = py_or(body.get("result").unwrap_or(&null), &empty);
    let raw_answers = py_or(result.get("answers").unwrap_or(&null), &empty);
    let mut answers = Map::new();
    if let Some(raws) = raw_answers.as_object() {
        for (q, raw) in raws {
            answers.insert(q.clone(), normalize(raw));
        }
    }
    let out = json!({
        "ok": true,
        "disabled": false,
        "answers": Value::Object(answers),
        "usage": py_or(result.get("usage").unwrap_or(&null), &json!({})),
        "error": null,
    });
    cache_put(root, &key, &out);
    out
}
