//! `export-okf` — deterministic OKF v0.2 bundle (wiki.py:cmd_export_okf
//! 658-662 parity). Emits okf.export's `{"pages","links","bytes"}` receipt.

use std::path::PathBuf;

use serde_json::Value;

use crate::err::EngineError;
use crate::okf;
use crate::store::Wiki;

pub fn run(wiki: &Wiki, output: &PathBuf) -> Result<Value, EngineError> {
    okf::export(wiki, output)
}
