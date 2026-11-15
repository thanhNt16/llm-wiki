//! Stdout emission contract (spec §6.2): exactly one JSON object on
//! stdout, then `process::exit`. Errors are JSON too — stderr is for
//! logs/crashes only. `report` exists for `ok:false` domain results
//! (verify-class commands) that are data, not errors, yet still exit 3.
use crate::err::EngineError;

pub fn ok(v: serde_json::Value) -> ! {
    println!("{}", serde_json::to_string(&v).unwrap());
    std::process::exit(0);
}

/// Emit a payload with a non-zero domain exit code — `ok:false` verify-class
/// results are data, not errors (spec §6.2), so `emit::err` does not apply.
pub fn report(v: serde_json::Value, code: i32) -> ! {
    println!("{}", serde_json::to_string(&v).unwrap());
    std::process::exit(code);
}

pub fn err(e: &EngineError) -> ! {
    println!(
        "{}",
        serde_json::json!({
            "error": e.to_string(),
            "exit_code": e.exit_code(),
            "error_type": e.error_type(),
        })
    );
    std::process::exit(e.exit_code());
}
