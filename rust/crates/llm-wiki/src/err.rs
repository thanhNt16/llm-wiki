//! Error taxonomy → exit codes (spec §6.2).
//!
//! One variant per CLI exit class; `error_type` is the stable string
//! agents parse out of the stdout error payload. `Usage` is reserved for
//! CLI-shape mistakes (clap failures + lane-selection misuse); genuine
//! runtime rejections — including any `io`/`serde_json`/`rusqlite`
//! failure — collapse to `Txn`/`Repair` (exit 3), matching the Python
//! catch-all that never emits a traceback.
use thiserror::Error;

#[derive(Error, Debug)]
pub enum EngineError {
    #[error("{0}")]
    Usage(String), // exit 2
    #[error("{0}")]
    Txn(String), // exit 3
    #[error("repair_required: {0}")]
    Repair(String), // exit 3, error_type=repair_required
    #[error("conflict: {0}")]
    Conflict(String), // exit 4
    #[error("locked")]
    Locked, // exit 5
}

impl EngineError {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Usage(_) => 2,
            Self::Txn(_) | Self::Repair(_) => 3,
            Self::Conflict(_) => 4,
            Self::Locked => 5,
        }
    }

    pub fn error_type(&self) -> &'static str {
        match self {
            Self::Usage(_) => "usage",
            Self::Txn(_) => "txn",
            Self::Repair(_) => "repair_required",
            Self::Conflict(_) => "conflict",
            Self::Locked => "locked",
        }
    }
}

impl From<std::io::Error> for EngineError {
    fn from(e: std::io::Error) -> Self {
        Self::Txn(e.to_string())
    }
}

impl From<serde_json::Error> for EngineError {
    fn from(e: serde_json::Error) -> Self {
        Self::Txn(e.to_string())
    }
}

impl From<rusqlite::Error> for EngineError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Txn(e.to_string())
    }
}
