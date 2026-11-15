//! `check-evidence` — read-only evidence-lane audit (plan Task 14).
//!
//! Emits the `evidence::audit::check_evidence` report on stdout. The audit
//! always completes: `needs_repair`/`ok` is data in the payload, and the
//! command exits 0 (wiki.py cmd_check_evidence parity). `--verify` enables
//! the raw-payload re-hash lane; `--json` is accepted for the spec §6.1
//! flag contract (payload is always JSON).

use serde_json::Value;

use crate::err::EngineError;
use crate::evidence::audit::{self, CheckEvidenceOpts};
use crate::store::Wiki;

pub struct CheckEvidenceArgs {
    /// Spec §6.1 flag contract — output is already JSON; accepted as a no-op.
    #[allow(dead_code)]
    pub json: bool,
    /// Re-hash manifest-pinned `sha256` values against `raw/` payloads.
    pub verify: bool,
    /// Cap on the bounded `issues[]` detail list (wiki.py `--details`).
    pub details: usize,
}

pub fn run(wiki: &Wiki, args: &CheckEvidenceArgs) -> Result<Value, EngineError> {
    audit::check_evidence(
        wiki,
        &CheckEvidenceOpts {
            verify: args.verify,
            details_limit: args.details,
        },
    )
}
