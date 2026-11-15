//! Evidence-lane audit — canonical `sources/` + `raw/` vs on-disk reality.
//!
//! `check_evidence` reconciles every `sources/<sid>/manifest.json`
//! (`versions[].{raw_path,normalized_path,assets}`) plus the versioned
//! `content_vN.md`/`extraction_vN.json` artifacts ingest emits against the
//! files actually on disk, and scans `sources/`/`raw/` for files no manifest
//! explains. With `verify` it additionally re-hashes each manifest-pinned
//! `sha256` against the raw payload bytes.
//!
//! Report keys: `{checked, missing, orphans, hash_mismatches, issues,
//! manifest_revision, needs_repair, verified, ok}` — `needs_repair` is data,
//! never a command failure; the check itself always completes (wiki.py's
//! `cmd_check_evidence` returns 0 unconditionally).
//!
//! `verify` is the repo-wide consistency pass: manifest revision vs index
//! watermark, dep edges resolvable to live ids, claims parse (plan Task 14).

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use crate::deps;
use crate::err::EngineError;
use crate::index;
use crate::store::Wiki;
use crate::util::sha256_hex;

#[derive(Default, Clone)]
pub struct CheckEvidenceOpts {
    /// Re-hash `versions[].sha256` against `raw_path` payload bytes.
    pub verify: bool,
    /// Cap on the bounded `issues[]` detail list (wiki.py `--details` slot).
    pub details_limit: usize,
}

/// Recursively collect file paths under `dir` relative to `base` as
/// slash-separated strings.
fn rel_files(base: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for e in entries.flatten() {
        let p = e.path();
        if p.is_dir() {
            rel_files(base, &p, out);
        } else if p.is_file() {
            if let Ok(rel) = p.strip_prefix(base) {
                out.push(rel.to_string_lossy().replace('\\', "/"));
            }
        }
    }
}

/// A manifest-pinned file is expected on disk; check existence, record the
/// expectation, and re-hash on demand.
fn check_referenced(
    wiki: &Wiki,
    rel: &str,
    sha: Option<&str>,
    opts: &CheckEvidenceOpts,
    expected: &mut HashSet<String>,
    missing: &mut Vec<Value>,
    mismatches: &mut Vec<Value>,
) {
    if rel.is_empty() || rel.contains("..") {
        return;
    }
    expected.insert(rel.to_string());
    let path: PathBuf = wiki.p(rel);
    if !path.is_file() {
        missing.push(json!({
            "type": "missing_file",
            "path": rel,
            "expected_sha256": sha,
        }));
        return;
    }
    if opts.verify {
        if let Some(want) = sha {
            let got = std::fs::read(&path).map(|b| sha256_hex(&b)).unwrap_or_default();
            if got != want {
                mismatches.push(json!({
                    "type": "hash_mismatch",
                    "error_type": "hash_mismatch",
                    "exit_code": 3,
                    "path": rel,
                    "expected_sha256": want,
                    "actual_sha256": got,
                }));
            }
        }
    }
}

/// Basenames a `sources/<sid>/` dir legitimately holds: manifest, the
/// versioned `content_vN`/`extraction_vN` artifacts ingest writes, the
/// `content.md`/`extraction.json` aliases, and manifest-pinned paths.
fn source_dir_expected(manifest: &Value) -> HashSet<String> {
    let mut out: HashSet<String> = HashSet::new();
    out.insert("manifest.json".to_string());
    out.insert("content.md".to_string());
    out.insert("extraction.json".to_string());
    for v in manifest["versions"].as_array().cloned().unwrap_or_default() {
        let ver = v["version"].as_i64().unwrap_or(0);
        out.insert(format!("content_v{ver}.md"));
        out.insert(format!("extraction_v{ver}.json"));
        if let Some(rel) = v["normalized_path"].as_str() {
            if let Some(base) = rel.rsplit('/').next() {
                out.insert(base.to_string());
            }
        }
        for a in v["assets"].as_array().cloned().unwrap_or_default() {
            if let Some(rel) = a.as_str().or_else(|| a["path"].as_str()) {
                if let Some(base) = rel.rsplit('/').next() {
                    out.insert(base.to_string());
                }
            }
        }
    }
    out
}

/// `check_evidence` — manifest-vs-disk integrity audit (read-only).
///
/// `missing`: manifest-pinned paths absent on disk (plus missing/corrupt
/// manifests). `orphans`: files under `sources/`/`raw/` no manifest
/// explains. `hash_mismatches` (`--verify`): raw payloads whose bytes no
/// longer match the ingest-time `sha256`. Any non-empty list flips
/// `needs_repair`/`ok`; `verified` is true exactly when no hash mismatch was
/// detected.
pub fn check_evidence(wiki: &Wiki, opts: &CheckEvidenceOpts) -> Result<Value, EngineError> {
    if !wiki.exists() {
        return Ok(json!({"initialized": false}));
    }
    let mut checked = 0i64;
    let mut missing: Vec<Value> = Vec::new();
    let mut orphans: Vec<Value> = Vec::new();
    let mut mismatches: Vec<Value> = Vec::new();
    let mut expected: HashSet<String> = HashSet::new();

    // manifest_revision: `.state/manifest.json` is canonical too — an
    // unreadable state file is itself a repair issue, reported as data.
    let state = wiki.state().ok();
    let manifest_revision = state
        .as_ref()
        .and_then(|s| s["revision"].as_i64())
        .map_or(Value::Null, Value::from);
    if state.is_none() {
        missing.push(json!({
            "type": "manifest_corrupt",
            "path": ".state/manifest.json",
        }));
    }

    let sources_dir = wiki.p("sources");
    let mut sids: Vec<String> = Vec::new();
    let mut stray_source_files: Vec<String> = Vec::new();
    if sources_dir.is_dir() {
        for e in std::fs::read_dir(&sources_dir)?.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if p.is_dir() {
                sids.push(name);
            } else {
                stray_source_files.push(format!("sources/{name}"));
            }
        }
    }
    sids.sort();

    for sid in &sids {
        let mrel = format!("sources/{sid}/manifest.json");
        expected.insert(mrel.clone());
        let manifest_path = wiki.p(&mrel);
        if !manifest_path.is_file() {
            missing.push(json!({"type": "missing_manifest", "path": mrel}));
            continue;
        }
        checked += 1;
        let manifest: Value = match std::fs::read_to_string(&manifest_path)
            .ok()
            .and_then(|t| serde_json::from_str(&t).ok())
        {
            Some(m) => m,
            None => {
                missing.push(json!({"type": "manifest_corrupt", "path": mrel}));
                continue;
            }
        };
        // Every version's pinned payload + normalized artifact, the
        // versioned extraction doc, the unversioned aliases, and assets.
        for v in manifest["versions"].as_array().cloned().unwrap_or_default() {
            let ver = v["version"].as_i64().unwrap_or(0);
            let sha = v["sha256"].as_str();
            if let Some(rel) = v["raw_path"].as_str() {
                check_referenced(
                    wiki,
                    rel,
                    sha,
                    opts,
                    &mut expected,
                    &mut missing,
                    &mut mismatches,
                );
            }
            if let Some(rel) = v["normalized_path"].as_str() {
                check_referenced(
                    wiki,
                    rel,
                    None,
                    opts,
                    &mut expected,
                    &mut missing,
                    &mut mismatches,
                );
            }
            for rel in [
                format!("sources/{sid}/content_v{ver}.md"),
                format!("sources/{sid}/extraction_v{ver}.json"),
            ] {
                check_referenced(
                    wiki,
                    &rel,
                    None,
                    opts,
                    &mut expected,
                    &mut missing,
                    &mut mismatches,
                );
            }
            for a in v["assets"].as_array().cloned().unwrap_or_default() {
                if let Some(rel) = a.as_str().or_else(|| a["path"].as_str()) {
                    check_referenced(
                        wiki,
                        rel,
                        a["sha256"].as_str(),
                        opts,
                        &mut expected,
                        &mut missing,
                        &mut mismatches,
                    );
                }
            }
        }
        for rel in [
            format!("sources/{sid}/content.md"),
            format!("sources/{sid}/extraction.json"),
        ] {
            check_referenced(
                wiki,
                &rel,
                None,
                opts,
                &mut expected,
                &mut missing,
                &mut mismatches,
            );
        }
        // Anything else in the source dir is unexplained.
        let dir_expected = source_dir_expected(&manifest);
        let sdir = wiki.p(&format!("sources/{sid}"));
        if sdir.is_dir() {
            for e in std::fs::read_dir(&sdir)?.flatten() {
                let p = e.path();
                let name = e.file_name().to_string_lossy().into_owned();
                if p.is_file() && !dir_expected.contains(&name) {
                    orphans.push(json!({
                        "type": "orphan",
                        "path": format!("sources/{sid}/{name}"),
                    }));
                }
            }
        }
    }

    for rel in stray_source_files {
        orphans.push(json!({"type": "orphan", "path": rel}));
    }

    // raw/ payloads no manifest pins are orphans.
    let raw_dir = wiki.p("raw");
    let mut raw_files = Vec::new();
    rel_files(&wiki.dot, &raw_dir, &mut raw_files);
    raw_files.sort();
    for rel in raw_files {
        if !expected.contains(&rel) {
            orphans.push(json!({"type": "orphan", "path": rel}));
        }
    }

    let needs_repair = !missing.is_empty() || !orphans.is_empty() || !mismatches.is_empty();
    // `issues` is bounded like wiki.py's `details` list.
    let mut issues: Vec<Value> = Vec::new();
    issues.extend(missing.iter().cloned());
    issues.extend(mismatches.iter().cloned());
    issues.extend(orphans.iter().cloned());
    issues.truncate(opts.details_limit);

    Ok(json!({
        "checked": checked,
        "missing": missing,
        "orphans": orphans,
        "hash_mismatches": mismatches,
        "issues": issues,
        "manifest_revision": manifest_revision,
        "needs_repair": needs_repair,
        "verified": mismatches.is_empty(),
        "ok": !needs_repair,
    }))
}

/// `verify` — repo-wide consistency pass (plan Task 14, spec §6.2):
/// manifest revision vs index watermark, dep edges resolvable to live ids,
/// claims parse. Each check reports `{name, status, details?}` with
/// `status` in {"pass", "fail"}; `ok:false` on any failure — the caller
/// maps it to exit 3.
pub fn verify(wiki: &Wiki) -> Result<Value, EngineError> {
    if !wiki.exists() {
        return Ok(json!({"initialized": false}));
    }
    let mut checks: Vec<Value> = Vec::new();

    // manifest_revision — index watermark keeps pace with `.state` revision.
    let rev = wiki.revision()?;
    match index::staleness_probe(wiki)? {
        None => checks.push(json!({
            "name": "manifest_revision", "status": "pass",
            "manifest_revision": rev,
        })),
        Some(built) if built == rev => checks.push(json!({
            "name": "manifest_revision", "status": "pass",
            "manifest_revision": rev, "built_at_revision": built,
        })),
        Some(built) => checks.push(json!({
            "name": "manifest_revision", "status": "fail",
            "detail": "index watermark behind/corrupt",
            "manifest_revision": rev, "built_at_revision": built,
        })),
    }

    // dep_edges — every registered dep resolves to a live id.
    let g = deps::graph(wiki)?;
    match deps::live_ids(wiki) {
        Ok(live) => {
            let mut dead: Vec<String> = Vec::new();
            if let Some(edges) = g["edges"].as_object() {
                for (artifact, deps_list) in edges {
                    for d in deps_list.as_array().cloned().unwrap_or_default() {
                        if let Some(dep) = d.as_str() {
                            if !deps::is_live_dep(dep, &live) {
                                dead.push(format!("{artifact} -> {dep}"));
                            }
                        }
                    }
                }
            }
            dead.sort();
            checks.push(if dead.is_empty() {
                json!({"name": "dep_edges", "status": "pass"})
            } else {
                json!({"name": "dep_edges", "status": "fail", "unresolved": dead})
            });
        }
        Err(e) => checks.push(json!({
            "name": "dep_edges", "status": "fail",
            "detail": format!("live id scan failed: {e}"),
        })),
    }

    // claims_parse — every canonical claim file parses.
    let mut bad: Vec<String> = Vec::new();
    let cdir = wiki.p("claims");
    if cdir.is_dir() {
        for e in std::fs::read_dir(&cdir)?.flatten() {
            let p = e.path();
            let name = e.file_name().to_string_lossy().into_owned();
            if !p.is_file() || !name.starts_with("claim_") || !name.ends_with(".json") {
                continue;
            }
            match std::fs::read_to_string(&p).map(|t| serde_json::from_str::<Value>(&t)) {
                Ok(Ok(_)) => {}
                Ok(Err(err)) => bad.push(format!("claims/{name}: {err}")),
                Err(err) => bad.push(format!("claims/{name}: {err}")),
            }
        }
    }
    bad.sort();
    checks.push(if bad.is_empty() {
        json!({"name": "claims_parse", "status": "pass"})
    } else {
        json!({"name": "claims_parse", "status": "fail", "unparseable": bad})
    });

    let ok = checks
        .iter()
        .all(|c| c["status"].as_str() == Some("pass"));
    Ok(json!({"ok": ok, "checks": checks}))
}
