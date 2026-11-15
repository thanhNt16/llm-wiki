//! `compile` — the 3-phase umbrella state machine (wiki.py:cmd_compile
//! 250-253 parity). `--resume` advances parked run dirs one phase; a
//! plain run mints/refreshes extract-phase pending entries. Every phase
//! payload exits 0 — `{"phase": "classify"}` is a successful result, not
//! an error (the `build` command owns the blocked exit-3 rendering).

use serde_json::Value;

use crate::compile;
use crate::err::EngineError;
use crate::store::Wiki;

#[derive(Default)]
pub struct CompileArgs {
    /// advance every staged run one phase (wiki.py --resume)
    pub resume: bool,
}

pub fn run(wiki: &Wiki, args: &CompileArgs) -> Result<Value, EngineError> {
    compile::compile_umbrella(wiki, args.resume)
}
