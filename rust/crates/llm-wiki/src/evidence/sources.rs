//! Source registry: identity, versioning, raw preservation, extraction stubs.
//! Port of wikicore/sources.py ingest()/_fill_normalized/pending_versions/
//! mark_compiled — secrets policy wired through `secret_gate` (sources.py:103).

use std::fs;
use std::io::Read;
use std::path::Path;

use serde_json::{json, Value};

use crate::err::EngineError;
use crate::evidence::{
    self, check_sid, is_image, secret_gate, versioned_rel,
    INJECTION_WARNING,
};
use crate::ids;
use crate::store::Wiki;
use crate::txn::{Txn, TxnOpts};
use crate::util::{now_iso, sha256_hex};

/// Options mirroring sources.ingest kwargs.
#[derive(Default, Clone)]
pub struct IngestOpts {
    pub source_id: Option<String>,
    /// Agent-produced normalized markdown (contents, not a path).
    pub normalized_content: Option<String>,
    pub parser_name: Option<String>,
    pub force: bool,
    pub root_origin: Option<String>,
}

pub type IngestReceipt = Value;

/// `origin_key` (sources.py:41-56): canonical identity for a (kind, ref) pair.
pub fn origin_key(kind: &str, rref: &str) -> Result<String, EngineError> {
    match kind {
        "file" | "directory" => {
            let abs = crate::store::absolute(Path::new(rref))?;
            Ok(abs.to_string_lossy().into_owned())
        }
        "url" => Ok(normalize_url(rref)),
        "session" => Ok(format!("session:{rref}")),
        "git_commit" | "feed_item" => {
            Ok(format!("{}:{}", kind, &sha256_hex(rref.as_bytes())[..24]))
        }
        "text" => Ok(format!("text:{}", &sha256_hex(rref.as_bytes())[..16])),
        _ => Err(EngineError::Txn(format!("unknown source kind: {kind:?}"))),
    }
}

/// urlsplit→lower scheme/netloc→sorted query→drop fragment.
fn normalize_url(rref: &str) -> String {
    let (rest, _frag) = rref.split_once('#').unwrap_or((rref, ""));
    let (scheme, rest) = rest.split_once("://").unwrap_or(("", rest));
    let (netloc, tail) = match rest.find(['/', '?']) {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let (path, query) = tail.split_once('?').unwrap_or((tail, ""));
    let mut pairs: Vec<&str> = query.split('&').filter(|s| !s.is_empty()).collect();
    pairs.sort();
    let q = pairs.join("&");
    if scheme.is_empty() && netloc.is_empty() {
        return rref.to_string();
    }
    format!(
        "{}://{}{}{}{}",
        scheme.to_lowercase(),
        netloc.to_lowercase(),
        path,
        if q.is_empty() { "" } else { "?" },
        q
    )
}

/// `resolve` (sources.py:87-89) — alias lookup by origin key.
pub fn resolve(wiki: &Wiki, kind: &str, rref: &str) -> Result<Option<String>, EngineError> {
    let aliases = wiki.load_json_opt(".state/aliases.json")?.unwrap_or(json!({}));
    let okey = origin_key(kind, rref)?;
    Ok(aliases
        .get(&okey)
        .and_then(|v| v.as_str())
        .map(str::to_string))
}

/// CLI-shaped file ingest: read bytes, kind=file.
pub fn ingest_file(
    wiki: &Wiki,
    path: &Path,
    kind: &str,
    opts: &IngestOpts,
) -> Result<IngestReceipt, EngineError> {
    let data = std::fs::read(path)
        .map_err(|e| EngineError::Txn(format!("cannot read {}: {e}", path.display())))?;
    ingest(wiki, kind, &path.to_string_lossy(), &data, opts)
}

/// CLI-shaped text ingest: inline text bytes, caller-supplied ref.
pub fn ingest_text(
    wiki: &Wiki,
    text: &str,
    text_ref: &str,
    kind: &str,
    opts: &IngestOpts,
) -> Result<IngestReceipt, EngineError> {
    ingest(wiki, kind, text_ref, text.as_bytes(), opts)
}

/// `sources.ingest` (sources.py:130-218) ported one-to-one.
pub fn ingest(
    wiki: &Wiki,
    kind: &str,
    rref: &str,
    data: &[u8],
    opts: &IngestOpts,
) -> Result<IngestReceipt, EngineError> {
    if !wiki.exists() {
        return Err(EngineError::Txn(
            "wiki not initialized; run wiki-init first".into(),
        ));
    }
    let okey = origin_key(kind, rref)?;
    let aliases = wiki.load_json_opt(".state/aliases.json")?.unwrap_or(json!({}));
    let mut sid = opts
        .source_id
        .clone()
        .or_else(|| aliases.get(&okey).and_then(|v| v.as_str()).map(str::to_string));
    if let Some(s) = &sid {
        check_sid(s)?;
    }
    let sha = sha256_hex(data);
    let mut warnings: Vec<String> = Vec::new();
    let filename = if kind == "file" || kind == "directory" {
        Path::new(rref)
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
    } else {
        None
    };
    let mut normalized_text = String::new();
    let mut pre_secret_summary = json!({"findings": [], "redacted": 0});
    if let Some(content) = &opts.normalized_content {
        let (t, s) = secret_gate(wiki, content.clone(), filename.as_deref(), &mut warnings)?;
        normalized_text = t;
        pre_secret_summary = s;
        if evidence::possible_prompt_injection(&normalized_text) {
            warnings.push(INJECTION_WARNING.into());
        }
    }
    let mut manifest;
    let version;
    if let Some(existing) = &sid {
        manifest = wiki.load_json(&format!("sources/{existing}/manifest.json"))?;
        if !opts.force {
            for v in manifest["versions"].as_array().cloned().unwrap_or_default() {
                if v["sha256"].as_str() == Some(sha.as_str()) {
                    if opts.normalized_content.is_some() {
                        return fill_normalized(
                            wiki,
                            existing,
                            &v,
                            normalized_text,
                            &opts.parser_name,
                            rref,
                            &sha,
                            warnings,
                            pre_secret_summary,
                        );
                    }
                    return Ok(json!({
                        "source_id": existing,
                        "version": v["version"],
                        "sha256": sha,
                        "deduplicated": true,
                        "warnings": [],
                        "secrets": {"findings": [], "redacted": 0},
                        "raw_path": v["raw_path"],
                    }));
                }
            }
        }
        version = manifest["versions"]
            .as_array()
            .map(|a| a.len() as i64)
            .unwrap_or(0)
            + 1;
    } else {
        sid = Some(ids::new("source"));
        manifest = json!({
            "id": sid.as_ref().unwrap(),
            "kind": kind,
            "origin": rref,
            "root_origin": opts.root_origin.clone().unwrap_or_else(|| sid.clone().unwrap()),
            "created_at": now_iso(),
            "versions": [],
            "title": filename.clone().unwrap_or_else(|| rref.chars().take(80).collect()),
        });
        version = 1;
    }
    let sid = sid.unwrap();

    let text_like = evidence::text::is_text_like(kind, rref, data);
    let mut parser = json!({"name": "wikicore-raw", "version": "1.0"});
    let mut normalized_source = Value::Null;
    let secret_summary;
    if opts.normalized_content.is_some() {
        secret_summary = pre_secret_summary.clone();
        parser = json!({"name": opts.parser_name.clone().unwrap_or_else(|| "agent".into()),
                        "version": "in-session"});
        normalized_source = json!("agent");
    } else if text_like {
        let (t, s) = secret_gate(wiki, String::from_utf8_lossy(data).into_owned(), filename.as_deref(), &mut warnings)?;
        normalized_text = t;
        secret_summary = s;
        normalized_source = json!("wikicore-raw");
    } else {
        secret_summary = json!({"findings": [], "redacted": 0});
    }
    if opts.normalized_content.is_none() && evidence::possible_prompt_injection(&normalized_text) {
        warnings.push(INJECTION_WARNING.into());
    }

    let run_id = ids::new_run();
    let mut txn = Txn::begin_with(
        wiki,
        &run_id,
        TxnOpts { command: Some("wiki-ingest".to_string()), ..Default::default() },
    )?;
    // staging paths forbid dot-components; flatten leading dots in basenames
    let raw_name = filename
        .clone()
        .unwrap_or_else(|| "content.bin".into())
        .trim_start_matches('.')
        .to_string();
    let raw_name = if raw_name.is_empty() {
        "dotfile".to_string()
    } else {
        raw_name
    };
    let raw_rel = format!("raw/{}/{}/{}", raw_subdir(kind), &sha[..16], raw_name);
    txn.stage_write(&raw_rel, data)?;
    let norm_rel = versioned_rel(&sid, version, "content", ".md");
    let norm_alias_rel = format!("sources/{sid}/content.md");
    let mut coverage = json!({
        "text": "complete", "tables": "skipped",
        "images": if is_image(rref) { "not_extracted" } else { "skipped" },
        "diagrams": "skipped", "formulas": "skipped",
    });
    let final_text: String;
    let mut secret_summary = secret_summary;
    if opts.normalized_content.is_some() {
        final_text = normalized_text.clone();
        if is_image(rref) {
            coverage["images"] = json!("complete");
        }
    } else if text_like {
        final_text = normalized_text.clone();
    } else {
        // Rust-native office readers first (spec §8 capability upgrade); the
        // markitdown lane is the fallback for legacy formats and failure.
        let mut native: Option<String> = None;
        if let Some(name) = filename.as_deref() {
            match crate::formats::extract_named(name, data) {
                Ok(Some(md)) => native = Some(md),
                Ok(None) => {}
                Err(e) => warnings.push(format!(
                    "rust-oxml extraction failed ({}); falling back to markitdown probe",
                    e
                )),
            }
        }
        match native {
            Some(md) => {
                parser = json!({"name": crate::formats::PARSER_NAME, "version": "1.0"});
                normalized_source = json!("rust-oxml");
                coverage["text"] = json!("extracted");
                final_text = md;
            }
            None => {
                let probe = evidence::text::probe_path(
                    &wiki.dot,
                    rref,
                    data,
                    Some(txn.staging_path(&raw_rel)),
                )?;
                match evidence::text::parser_for_binary(probe.path()) {
                    Some((parsed, p_name)) => {
                        // parser output is untrusted external content: gate + scan it too
                        let (parsed, s) = secret_gate(wiki, parsed, filename.as_deref(), &mut warnings)?;
                        secret_summary = s;
                        if evidence::possible_prompt_injection(&parsed) {
                            warnings.push(INJECTION_WARNING.into());
                        }
                        parser = json!({"name": p_name, "version": "?"});
                        normalized_source = json!("markitdown");
                        coverage["text"] = json!("partial");
                        final_text = parsed;
                        warnings.push(format!(
                            "binary parsed with {}; verify coverage",
                            parser["name"].as_str().unwrap_or("markitdown")
                        ));
                    }
                    None => {
                        coverage["text"] = json!("not_extracted");
                        warnings.push(format!(
                            "{} preserved as raw bytes but not semantically extracted \
                             (no parser available); use a document parser to extract",
                            filename.clone().unwrap_or_else(|| kind.to_string())
                        ));
                        final_text = String::new();
                    }
                }
            }
        }
    }
    txn.stage_write(&norm_rel, final_text.as_bytes())?;
    txn.stage_write(&norm_alias_rel, final_text.as_bytes())?;
    let versions = manifest["versions"].as_array_mut().unwrap();
    versions.push(json!({
        "version": version,
        "sha256": sha,
        "captured_at": now_iso(),
        "raw_path": raw_rel,
        "normalized_path": norm_rel,
        "assets": [],
        "adapter": format!("wikicore.{kind}"),
        "bytes": data.len(),
    }));
    stage_json(&mut txn, &format!("sources/{sid}/manifest.json"), &manifest)?;
    let report = json!({
        "source_id": sid, "source_version": version,
        "coverage": coverage, "warnings": warnings, "parser": parser,
    });
    let extraction_doc = json!({
        "extraction_report": report, "candidates": [], "source_version": version,
    });
    stage_json(
        &mut txn,
        &versioned_rel(&sid, version, "extraction", ".json"),
        &extraction_doc,
    )?;
    stage_json(
        &mut txn,
        &format!("sources/{sid}/extraction.json"),
        &extraction_doc,
    )?;
    let mut aliases_new = aliases;
    aliases_new[&okey] = json!(sid);
    txn.stage_state(".state/aliases.json", &aliases_new)?;
    // python parity (sources.py:217): receipt carries the change counts
    // that feed wiki/log.md + wiki/changes/index.md
    txn.changes
        .insert("sources_registered".to_string(), json!(i64::from(version == 1)));
    txn.changes.insert("versions_added".to_string(), json!(1));
    txn.warnings = warnings.iter().map(|w| json!(w)).collect();
    let _receipt = txn.commit()?;
    Ok(json!({
        "source_id": sid, "version": version, "sha256": sha,
        "deduplicated": false,
        "raw_path": raw_rel, "normalized_path": norm_rel,
        "coverage": coverage, "parser": parser,
        "normalized_source": normalized_source,
        "warnings": warnings, "secrets": secret_summary,
        "run_id": run_id,
    }))
}

/// `_fill_normalized` (sources.py:221-287): fill agent-supplied normalized
/// text into a deduplicated raw-only version, on THAT version's own
/// content_vN/extraction_vN artifacts.
#[allow(clippy::too_many_arguments)]
fn fill_normalized(
    wiki: &Wiki,
    sid: &str,
    version_entry: &Value,
    text: String,
    parser_name: &Option<String>,
    rref: &str,
    sha: &str,
    warnings: Vec<String>,
    secret_summary: Value,
) -> Result<IngestReceipt, EngineError> {
    let v = version_entry["version"].as_i64().unwrap_or(0);
    let extraction_rel = versioned_rel(sid, v, "extraction", ".json");
    let mut extraction: Value = if wiki.p(&extraction_rel).is_file() {
        wiki.load_json(&extraction_rel)?
    } else {
        // legacy layout: shared extraction.json trusted only when it describes v
        let alias_rel = format!("sources/{sid}/extraction.json");
        let ex = wiki.load_json_opt(&alias_rel)?.unwrap_or(json!({}));
        if ex.get("source_version").and_then(|x| x.as_i64()) != Some(v) {
            json!({"candidates": [], "source_version": v})
        } else {
            ex
        }
    };
    let cur = extraction
        .get("extraction_report")
        .cloned()
        .unwrap_or(json!({}));
    if cur
        .get("coverage")
        .and_then(|c| c.get("text"))
        .and_then(|t| t.as_str())
        .map(|t| t != "not_extracted")
        .unwrap_or(false)
    {
        let mut w = warnings;
        w.push(format!(
            "normalized_content_ignored: version {v} already text-extracted"
        ));
        return Ok(json!({
            "source_id": sid, "version": v, "sha256": sha,
            "deduplicated": true,
            "normalized_status": "already_extracted",
            "normalized_source": "agent",
            "parser": {"name": parser_name.clone().unwrap_or_else(|| "agent".into()),
                       "version": "in-session"},
            "warnings": w,
            "secrets": secret_summary,
            "raw_path": version_entry["raw_path"],
        }));
    }
    let mut manifest = wiki.load_json(&format!("sources/{sid}/manifest.json"))?;
    let is_latest = manifest["versions"]
        .as_array()
        .and_then(|vs| vs.last())
        .and_then(|e| e["version"].as_i64())
        == Some(v);
    let run_id = ids::new_run();
    let mut txn = Txn::begin_with(
        wiki,
        &run_id,
        TxnOpts { command: Some("wiki-ingest-fill".to_string()), ..Default::default() },
    )?;
    let content_rel = versioned_rel(sid, v, "content", ".md");
    txn.stage_write(&content_rel, text.as_bytes())?;
    if is_latest {
        txn.stage_write(&format!("sources/{sid}/content.md"), text.as_bytes())?;
    }
    let mut coverage = cur.get("coverage").cloned().unwrap_or(json!({}));
    coverage["text"] = json!("complete");
    if is_image(rref) {
        coverage["images"] = json!("complete");
    }
    let report = json!({
        "source_id": sid, "source_version": v,
        "coverage": coverage, "warnings": warnings,
        "parser": {"name": parser_name.clone().unwrap_or_else(|| "agent".into()),
                   "version": "in-session"},
    });
    extraction["extraction_report"] = report;
    extraction["source_version"] = json!(v);
    stage_json(&mut txn, &extraction_rel, &extraction)?;
    if is_latest {
        stage_json(&mut txn, &format!("sources/{sid}/extraction.json"), &extraction)?;
    }
    // manifest entry must point at this version's own content file
    let mut changed = false;
    if let Some(vs) = manifest["versions"].as_array_mut() {
        for entry in vs.iter_mut() {
            if entry["version"].as_i64() == Some(v)
                && entry.get("normalized_path").and_then(|p| p.as_str()) != Some(&content_rel)
            {
                entry["normalized_path"] = json!(content_rel);
                changed = true;
            }
        }
    }
    if changed {
        stage_json(&mut txn, &format!("sources/{sid}/manifest.json"), &manifest)?;
    }
    // uncompile: remove v from .state/manifest.json compiled[sid]
    let mut state = wiki.state()?;
    if let Some(done) = state
        .pointer_mut(&format!("/compiled/{sid}"))
        .and_then(|d| d.as_array_mut())
    {
        done.retain(|x| x.as_i64() != Some(v));
    }
    txn.stage_state(".state/manifest.json", &state)?;
    // python parity (sources.py:279)
    txn.changes.insert("sources_registered".to_string(), json!(0));
    txn.changes.insert("versions_added".to_string(), json!(0));
    txn.warnings = warnings.iter().map(|w| json!(w)).collect();
    let _receipt = txn.commit()?;
    Ok(json!({
        "source_id": sid, "version": v, "sha256": sha,
        "deduplicated": false,
        "normalized_status": "filled",
        "updated_normalized": true,
        "normalized_source": "agent",
        "parser": {"name": parser_name.clone().unwrap_or_else(|| "agent".into()),
                   "version": "in-session"},
        "raw_path": version_entry["raw_path"],
        "coverage": coverage,
        "warnings": warnings,
        "secrets": secret_summary,
        "run_id": run_id,
    }))
}

/// `load_content` (sources.py:293-303).
pub fn load_content(wiki: &Wiki, source_id: &str, version: i64) -> Result<String, EngineError> {
    check_sid(source_id)?;
    let manifest = wiki.load_json(&format!("sources/{source_id}/manifest.json"))?;
    for v in manifest["versions"].as_array().cloned().unwrap_or_default() {
        if v["version"].as_i64() == Some(version) {
            let rel = v["normalized_path"].as_str().unwrap_or("");
            let path = wiki.p(rel);
            if path.is_file() {
                return Ok(std::fs::read_to_string(path)?);
            }
            return Ok(String::new());
        }
    }
    Err(EngineError::Txn(format!(
        "unknown version {version} of {source_id}"
    )))
}

/// `get_manifest` (sources.py:305-310) — None when absent.
pub fn get_manifest(wiki: &Wiki, source_id: &str) -> Result<Option<Value>, EngineError> {
    check_sid(source_id)?;
    wiki.load_json_opt(&format!("sources/{source_id}/manifest.json"))
}

/// `pending_versions` (sources.py:313-333) — source versions not yet compiled.
pub fn pending_versions(wiki: &Wiki) -> Result<Vec<Value>, EngineError> {
    let state = wiki.state()?;
    let compiled = state.get("compiled").cloned().unwrap_or(json!({}));
    let mut pending = Vec::new();
    let sources_dir = wiki.p("sources");
    if !sources_dir.is_dir() {
        return Ok(pending);
    }
    let mut sids: Vec<String> = Vec::new();
    for e in std::fs::read_dir(&sources_dir)? {
        let e = e?;
        if e.file_type()?.is_dir() {
            sids.push(e.file_name().to_string_lossy().into_owned());
        }
    }
    sids.sort();
    for sid in sids {
        let mrel = format!("sources/{sid}/manifest.json");
        if !wiki.p(&mrel).is_file() {
            continue;
        }
        let manifest = wiki.load_json(&mrel)?;
        let done: Vec<i64> = compiled
            .get(&sid)
            .and_then(|d| d.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_i64()).collect())
            .unwrap_or_default();
        for v in manifest["versions"].as_array().cloned().unwrap_or_default() {
            let ver = v["version"].as_i64().unwrap_or(0);
            if !done.contains(&ver) {
                pending.push(json!({
                    "source_id": sid,
                    "version": ver,
                    "origin": manifest["origin"],
                    "title": manifest.get("title").cloned().unwrap_or(json!("")),
                }));
            }
        }
    }
    Ok(pending)
}

/// `mark_compiled` (sources.py:335-345) — record a version as compiled in
/// `.state/manifest.json` under its own txn.
pub fn mark_compiled(wiki: &Wiki, source_id: &str, version: i64) -> Result<(), EngineError> {
    check_sid(source_id)?;
    let run_id = ids::new_run();
    let mut txn = Txn::begin_with(
        wiki,
        &run_id,
        TxnOpts {
            command: Some("wiki-internal-mark-compiled".to_string()),
            ..Default::default()
        },
    )?;
    let mut state = wiki.state()?;
    {
        let root = state
            .as_object_mut()
            .ok_or_else(|| EngineError::Txn(".state/manifest.json is not an object".into()))?;
        let compiled = root
            .entry("compiled".to_string())
            .or_insert_with(|| json!({}));
        let done = compiled
            .as_object_mut()
            .ok_or_else(|| EngineError::Txn("compiled is not an object".into()))?
            .entry(source_id.to_string())
            .or_insert_with(|| json!([]));
        let arr = done
            .as_array_mut()
            .ok_or_else(|| EngineError::Txn("compiled[sid] is not an array".into()))?;
        if !arr.iter().any(|x| x.as_i64() == Some(version)) {
            arr.push(json!(version));
        }
    }
    txn.stage_state(".state/manifest.json", &state)?;
    let _receipt = txn.commit()?;
    Ok(())
}

fn stage_json(txn: &mut Txn, rel: &str, v: &Value) -> Result<(), EngineError> {
    let mut bytes = serde_json::to_string_pretty(v)?.into_bytes();
    bytes.push(b'\n');
    txn.stage_write(rel, &bytes)
}


/// sources.py LARGE_FILE_BYTES — files over 500 KB demote to priority 3.
pub const LARGE_FILE_BYTES: u64 = 500 * 1024;

/// `SESSION_NAME_RE` (sources.py:348): filename hints at a session
/// transcript. `re.search(r"session|transcript|chat", IGNORECASE)` reduces
/// to a case-insensitive substring test.
fn session_named(name: &str) -> bool {
    let n = name.to_lowercase();
    n.contains("session") || n.contains("transcript") || n.contains("chat")
}

/// `_has_role_message` (sources.py:351) — decoded JSON value carrying both
/// role and message keys, at any nesting depth.
fn has_role_message(v: &Value) -> bool {
    match v {
        Value::Object(m) => {
            (m.contains_key("role") && m.contains_key("message"))
                || m.values().any(has_role_message)
        }
        Value::Array(a) => a.iter().any(has_role_message),
        _ => false,
    }
}

/// `_session_shaped` (sources.py:362) — sniff a .jsonl/.json file for
/// conversation records over the first MiB. JSONL first (any complete line
/// that parses as a session record), then plain JSON.
fn session_shaped(path: &Path) -> bool {
    let mut f = match fs::File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut chunk = Vec::new();
    if f.by_ref().take(1 << 20).read_to_end(&mut chunk).is_err() {
        return false;
    }
    let text = String::from_utf8_lossy(&chunk);
    for line in text.split(['\n', '\r']) {
        let line = line.trim();
        if line.is_empty() || !line.starts_with('{') {
            continue;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(v) if has_role_message(&v) => return true,
            _ => continue,
        }
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(v) => has_role_message(&v),
        Err(_) => false,
    }
}

/// `sources.triage` (sources.py:387) — deterministic ingest proposals for
/// a directory's top-level files. No LLM: kind/priority come from
/// extension, filename patterns, content sniffing, and size. The manifest
/// is meant to be reviewed/edited before `ingest --triage` applies it.
pub fn triage(dirpath: &Path) -> Result<Value, EngineError> {
    let dir_abs = std::path::absolute(dirpath)?;
    let mut entries: Vec<Value> = Vec::new();
    let mut names: Vec<String> = fs::read_dir(&dir_abs)?
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    for name in names {
        let path = dir_abs.join(&name);
        // `os.path.isfile(path) or os.path.islink(path)` — regular files
        // only, symlinks excluded.
        let is_file = fs::metadata(&path).map(|m| m.is_file()).unwrap_or(false);
        let is_link = fs::symlink_metadata(&path)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        if !is_file || is_link {
            continue;
        }
        let ext = evidence::ext(&name);
        let mut reasons: Vec<String> = Vec::new();
        let mut kind: Option<&str> = None;
        if ext == ".jsonl" || ext == ".json" {
            if session_shaped(&path) {
                kind = Some("session");
                reasons.push("content carries role/message records".to_string());
            } else if session_named(&name) {
                kind = Some("session");
                reasons.push("filename suggests a session transcript".to_string());
            }
        }
        if ext == ".md" || ext == ".txt" {
            reasons.push(format!("{ext} text/markdown"));
        } else if !ext.is_empty() {
            reasons.push(format!("{ext} file"));
            if kind.is_none() {
                reasons.push("kind undetermined; review".to_string());
            }
        } else {
            reasons.push("unknown file type; review manually".to_string());
        }
        let mut priority = 2;
        let size = fs::metadata(&path).map(|m| m.len() as i64).unwrap_or(-1);
        if size > LARGE_FILE_BYTES as i64 {
            priority = 3;
            reasons.push(format!(
                "size {} bytes exceeds {} KB; review before ingest",
                size,
                LARGE_FILE_BYTES / 1024
            ));
        } else if size < 0 {
            priority = 3;
            reasons.push("unreadable; verify before ingest".to_string());
        } else if kind == Some("session") {
            priority = 1;
        }
        entries.push(json!({
            "file": path.to_string_lossy(),
            "kind": kind,
            "priority": priority,
            "rationale": reasons.join("; "),
        }));
    }
    Ok(json!({
        "dir": dir_abs.to_string_lossy(),
        "files": entries,
    }))
}

/// `"%s: %s" % (type(e).__name__, e)` — Python class name for the failed[]
/// error string.
fn py_exc_name(e: &EngineError) -> &'static str {
    match e {
        EngineError::Usage(_) => "UsageError",
        EngineError::Txn(_) => "TxnError",
        EngineError::Repair(_) => "RepairError",
        EngineError::Conflict(_) => "ConflictError",
        EngineError::Locked => "LockedError",
    }
}

fn io_exc_name(e: &std::io::Error) -> &'static str {
    match e.kind() {
        std::io::ErrorKind::NotFound => "FileNotFoundError",
        std::io::ErrorKind::PermissionDenied => "PermissionError",
        _ => "OSError",
    }
}

/// `sources.ingest_triage` (sources.py:437) — batch-ingest a triage
/// manifest. One failure logs and continues. Each entry ingests through
/// the same path as `ingest --file` — a manifest kind (e.g. "session")
/// takes the place of the `--kind` flag.
pub fn ingest_triage(wiki: &Wiki, manifest: &Value, force: bool) -> Value {
    let files = manifest
        .get("files")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let total = files.len();
    let mut ingested: Vec<Value> = Vec::new();
    let mut failed: Vec<Value> = Vec::new();
    for entry in files {
        let path = entry.get("file").cloned().unwrap_or(Value::Null);
        // `entry.get("kind") or "file"` — falsy/empty kinds fall back.
        let kind = entry
            .get("kind")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .unwrap_or("file");
        let outcome = match path.as_str() {
            None => Err("TypeError: open() missing required argument".to_string()),
            Some(p) => match fs::read(p) {
                Err(e) => Err(format!("{}: {}", io_exc_name(&e), e)),
                Ok(data) => ingest(wiki, kind, p, &data, &IngestOpts {
                    force,
                    ..Default::default()
                })
                .map_err(|e| format!("{}: {}", py_exc_name(&e), e))
                .and_then(|r| {
                    Ok(json!({
                        "file": path,
                        "source_id": r["source_id"],
                        "version": r["version"],
                        "deduplicated": r["deduplicated"],
                    }))
                }),
            },
        };
        match outcome {
            Ok(item) => ingested.push(item),
            Err(msg) => failed.push(json!({"file": path, "error": msg})),
        }
    }
    json!({"ingested": ingested, "failed": failed, "total": total})
}

/// `raw/<subdir>` bucket per spec §4.1 layout (`raw/{files,urls,repositories,
/// sessions,text}`). Python wrote `raw/{kind}` verbatim producing a stray
/// `raw/file/` the init scaffold never creates — spec layout wins.
fn raw_subdir(kind: &str) -> &'static str {
    match kind {
        "file" => "files",
        "url" => "urls",
        "session" => "sessions",
        "text" => "text",
        "directory" | "git_commit" => "repositories",
        _ => "files",
    }
}
