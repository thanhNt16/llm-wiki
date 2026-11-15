//! `doctor` — static + semantic diagnostics plus the §9 fix path.
//!
//! Read path payload `{ok, findings[], counts, fts, extractors,
//! repair_plan}`; `ok:false → exit 3` (spec §6.2 — a successful domain
//! result, not an error). `--scope` is a substring filter over the
//! serialized finding (`scope in json.dumps(finding)`, wiki.py:617-623)
//! with `ok`/`counts` recounted over the survivors.
//!
//! `doctor --fix` (§9): applicable fixes run in ONE transaction, the
//! index rebuild is post-commit, and the payload is
//! `{ok: true, fixed: [{code,artifact,action}], remaining: [findings]}`
//! — exit 0 always (`remaining` is information, not failure; real errors
//! still exit 3). `--dry-run` applies the identical plan to a scratch
//! copy of `.llm-wiki` (msft assumeDeleted-style: simulate each fix, then
//! re-lint the post-fix world) and mutates nothing. `--code` is a
//! repeatable finding-code filter; `--scope` also limits the fix set —
//! scoped-out findings are neither fixed nor reported in `remaining[]`.
//!
//! Legacy flags (§9.3): `--fix-indexes` = `--code unlisted_page
//! --code section_index_unlinked`; `--rebuild-index`/`--rebuild-fts` =
//! `--code index_stale --code fts_stale`. Output identical to `--fix`
//! plus `"deprecated_flag": "<name>"`.

use serde_json::{json, Value};

use crate::doctor;
use crate::err::EngineError;
use crate::store::Wiki;
use crate::util;

#[derive(Default)]
pub struct DoctorArgs {
    /// `--scope S` — substring filter over `json.dumps(finding)`; with
    /// `--fix` it limits the fix set BEFORE fix selection (§9.2).
    pub scope: Option<String>,
    /// `--fix` — run the §9 fix path.
    pub fix: bool,
    /// `--fix-indexes` legacy alias (§9.3).
    pub fix_indexes: bool,
    /// `--rebuild-index` legacy alias (§9.3).
    pub rebuild_index: bool,
    /// `--rebuild-fts` legacy alias (§9.3).
    pub rebuild_fts: bool,
    /// `--dry-run` (`--fix` modifier) — simulate, never commit.
    pub dry_run: bool,
    /// `--code X` — repeatable finding-code filter over the fix set.
    pub codes: Vec<String>,
}

/// Process exit code for a doctor payload: `ok:false` → 3, else 0
/// (`return 0 if v["ok"] else 3`, wiki.py:626 parity). Fix payloads pin
/// `ok:true` so the exit-0-always contract (§9.2) falls out of here.
pub fn exit_code(payload: &Value) -> i32 {
    if payload["ok"].as_bool().unwrap_or(false) {
        0
    } else {
        3
    }
}

/// The §9.3 legacy flag that triggered the fix run, in reporting
/// precedence order. `--fix` itself is the real flag — never deprecated.
fn deprecated_flag(args: &DoctorArgs) -> Option<&'static str> {
    if args.fix_indexes {
        Some("--fix-indexes")
    } else if args.rebuild_index {
        Some("--rebuild-index")
    } else if args.rebuild_fts {
        Some("--rebuild-fts")
    } else {
        None
    }
}

/// True when any fix-family flag was passed — the wiki.py
/// `takes_commit_lock` membership for `doctor` (§6.3; `--fix --dry-run`
/// is excluded since dry-run mutates nothing). A bare `--code`/`--dry-run`
/// requests the fix path too (the modifier implies its head flag).
pub fn wants_lock(args: &DoctorArgs) -> bool {
    (args.fix
        || args.fix_indexes
        || args.rebuild_index
        || args.rebuild_fts
        || !args.codes.is_empty())
        && !args.dry_run
}

/// Legacy flags map to their §9.3 code sets; explicit `--code` on a
/// legacy flag run is overridden by the defined set.
fn effective_codes(args: &DoctorArgs) -> Vec<String> {
    if args.fix_indexes {
        vec![
            "unlisted_page".to_string(),
            "section_index_unlinked".to_string(),
        ]
    } else if args.rebuild_index || args.rebuild_fts {
        vec!["index_stale".to_string(), "fts_stale".to_string()]
    } else {
        args.codes.clone()
    }
}

/// Scope-filter a findings array — same `scope in json.dumps(finding)`
/// rule as `Report::scoped`, applied to already-serialized findings.
fn scoped_findings(findings: &[Value], scope: &Option<String>) -> Vec<Value> {
    match scope {
        None => findings.to_vec(),
        Some(s) => findings
            .iter()
            .filter(|f| util::py_json_dumps(f).contains(s.as_str()))
            .cloned()
            .collect(),
    }
}

/// The §9.2 fix run: plan from the (scope-filtered) findings, apply in
/// one txn + post-commit index rebuild, re-lint for `remaining[]`.
/// `--dry-run` runs the identical machinery against a scratch copy of
/// `.llm-wiki` — the simulated post-fix world is re-linted for real, so
/// would-be-fixed and real-remaining come from the same detectors.
fn fix_payload(wiki: &Wiki, args: &DoctorArgs) -> Result<Value, EngineError> {
    let report = doctor::run(wiki)?;
    let report = match &args.scope {
        Some(scope) => report.scoped(scope),
        None => report,
    };
    let codes = effective_codes(args);

    let (fixed, after) = if args.dry_run {
        // assumeFixed simulation: copy .llm-wiki wholesale so file writes,
        // dep-graph edits, §5.4 repair, and the index rebuild all observe
        // a faithful post-fix world. A shimmed in-memory overlay would
        // diverge — the copy keeps `doctor::run` (unmodified) truthful.
        let tmp = std::env::temp_dir();
        let scratch = util::make_tmp_dir("llm-wiki-doctor-dry-", &tmp)
            .map_err(|e| EngineError::Txn(format!("dry-run scratch dir: {e}")))?;
        let _guard = util::TmpDir(scratch.clone());
        util::copy_tree(&wiki.dot, &scratch.join(".llm-wiki"))
            .map_err(|e| EngineError::Txn(format!("dry-run copy: {e}")))?;
        let replica = Wiki::open(&scratch)?;
        let replica = Wiki {
            allow_repair: wiki.allow_repair,
            ..replica
        };
        doctor::fix_wiki(wiki, &replica, &report.findings, &codes)?
    } else {
        doctor::fix_wiki(wiki, wiki, &report.findings, &codes)?
    };

    // remaining[] = post-fix findings, re-scoped (scoped-out findings are
    // neither fixed nor reported).
    let after_findings = after.to_value()["findings"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let remaining = scoped_findings(&after_findings, &args.scope);

    let mut payload = json!({
        "ok": true,
        "fixed": fixed,
        "remaining": remaining,
    });
    if args.dry_run {
        payload["dry_run"] = json!(true);
    }
    // Legacy-flag parity keys (wiki.py:595-599): `--rebuild-index` /
    // `--rebuild-fts` unconditionally rebuild and report the receipt.
    // Python's engines share the legacy JSON index; Rust's single
    // index.sqlite serves both, so both flags report `index_rebuilt`
    // (additive `fts_rebuilt` alias keeps the python key present too).
    // Never in dry-run: the rebuild would mutate the real `.llm-wiki`.
    if (args.rebuild_index || args.rebuild_fts) && !args.dry_run {
        let rb = crate::index::Index::rebuild_under_index_lock(
            wiki,
            wiki.revision()?,
        )?;
        payload["index_rebuilt"] = rb;
        if args.rebuild_fts {
            payload["fts_rebuilt"] = payload["index_rebuilt"].clone();
        }
    }
    if let Some(flag) = deprecated_flag(args) {
         payload["deprecated_flag"] = json!(flag);
     }
     Ok(payload)
}

pub fn run(wiki: &Wiki, args: &DoctorArgs) -> Result<Value, EngineError> {
    let fix_requested = args.fix
        || args.dry_run
        || args.fix_indexes
        || args.rebuild_index
        || args.rebuild_fts
        || !args.codes.is_empty();
    if fix_requested {
        return fix_payload(wiki, args);
    }
    let report = doctor::run(wiki)?;
    let report = match &args.scope {
        Some(scope) => report.scoped(scope),
        None => report,
    };
    Ok(report.to_value())
}
