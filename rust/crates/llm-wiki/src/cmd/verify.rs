//! `verify` — the citation_resolution gate (pages.verify, spec §11):
//! every dep edge's artifact exists, is frontmatter-parseable, and cites
//! only resolvable/current deps; stale derived artifacts must not
//! silently remain current. Payload `{ok, errors, stale}` — Python's
//! `cmd_verify` exits `0 if result["ok"] else 3` (wiki.py:413-416).
//!
//! The earlier Rust surface (audit::verify's named checks) is kept under
//! the additive `checks` key: `ok` still reflects every check passing so
//! audit failures keep gating exit 3, and the check detail stays
//! machine-readable next to the contract fields.

use serde_json::Value;

use crate::err::EngineError;
use crate::evidence::audit;
use crate::pages;
use crate::store::Wiki;

/// Process exit code for a verify payload: `ok:false` → 3, else 0
/// (`return 0 if v["ok"] else 3`, wiki.py:418 parity). An uninitialized
/// wiki reports `{"initialized": false}` and exits 0 (status parity).
pub fn exit_code(payload: &Value) -> i32 {
    if payload["initialized"].as_bool() == Some(false) {
        return 0;
    }
    if payload["ok"].as_bool().unwrap_or(false) {
        0
    } else {
        3
    }
}

pub fn run(wiki: &Wiki) -> Result<Value, EngineError> {
    if !wiki.exists() {
        return Ok(serde_json::json!({"initialized": false}));
    }
    let mut payload = pages::verify(wiki)?;
    let audit_payload = audit::verify(wiki)?;
    let checks = audit_payload
        .get("checks")
        .cloned()
        .unwrap_or_else(|| serde_json::json!([]));
    let audit_ok = audit_payload
        .get("ok")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let pages_ok = payload["ok"].as_bool().unwrap_or(false);
    if let Some(obj) = payload.as_object_mut() {
        obj.insert("checks".to_string(), checks);
        obj.insert("ok".to_string(), Value::Bool(pages_ok && audit_ok));
    }
    Ok(payload)
}
