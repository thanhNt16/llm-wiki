//! `dream` command — thin layer over [`crate::dream`]
//! (wiki.py `cmd_dream`): `--purge` routes to the retention sweep (dry
//! unless `--apply`/`--dry-run` decide), everything else is plan/apply.
use crate::dream;
use crate::err::EngineError;
use crate::store::Wiki;

pub struct DreamArgs {
    pub apply: bool,
    pub phases: Vec<String>,
    pub purge: bool,
    pub dry_run: bool,
    pub retention_days: i64,
}

/// wiki.py:702-709 — purge uses `dry_run or not apply`; run uses `apply`
/// and the (possibly empty) repeatable `--phase` subset.
pub fn run(wiki: &Wiki, args: &DreamArgs) -> Result<serde_json::Value, EngineError> {
    if args.purge {
        return dream::purge(
            wiki,
            args.dry_run || !args.apply,
            None,
            args.retention_days,
        );
    }
    let phases: Vec<&str> = args.phases.iter().map(String::as_str).collect();
    let phases = if phases.is_empty() { None } else { Some(phases.as_slice()) };
    dream::run(wiki, args.apply, phases, None)
}
