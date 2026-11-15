//! `tools/list` + `tools/call` — the 13-tool contract (spec §7.5).
//!
//! Every tool maps onto an existing `cmd::*` entry point and takes an
//! optional `root` argument that overrides the session binding for that
//! call only. `wiki_open`/`wiki_init` are the unbound-exempt tools; every
//! other tool returns `isError:true` "no wiki open — call wiki_open"
//! while the session is unbound (§7.3).

use std::path::PathBuf;

use serde_json::{json, Value};

use super::{RpcError, Server, NO_WIKI_MSG};
use crate::cmd::build as cmd_build;
use crate::cmd::compile as cmd_compile;
use crate::cmd::decide as cmd_decide;
use crate::cmd::doctor as cmd_doctor;
use crate::cmd::ingest::{self, IngestArgs, DEFAULT_MAX_BYTES};
use crate::cmd::page_show as cmd_page_show;
use crate::cmd::query_prepare as cmd_query_prepare;
use crate::cmd::reconcile_apply as cmd_reconcile_apply;
use crate::cmd::reconcile_prepare as cmd_reconcile_prepare;
use crate::cmd::review as cmd_review;
use crate::cmd::status;
use crate::cmd::write_page as cmd_write_page;
use crate::err::EngineError;
use crate::store;

/// tools/call entry: params → CallToolResult (§7.4) or RpcError.
/// `tools/call` invocations are serialized through the single-threaded
/// dispatch loop — the §7.1 dispatch mutex falls out structurally.
pub(crate) fn call(srv: &mut Server, params: &Value) -> Result<Value, RpcError> {
    let Some(obj) = params.as_object() else {
        return Err(RpcError::invalid_params("tools/call params must be an object"));
    };
    let Some(name) = obj.get("name").and_then(Value::as_str) else {
        return Err(RpcError::invalid_params("tools/call requires a string `name`"));
    };
    let args = match obj.get("arguments") {
        None | Some(Value::Null) => json!({}),
        Some(v) if v.is_object() => v.clone(),
        Some(_) => {
            return Err(RpcError::invalid_params("tools/call `arguments` must be an object"));
        }
    };
    if !TOOL_NAMES.contains(&name) {
        // Unknown tool → isError:true content (§7.1 msft convention),
        // never a JSON-RPC error.
        return Ok(srv.tool_err(json!({
            "error": format!("unknown tool: {name}"),
            "exit_code": 2,
            "error_type": "usage",
        })));
    }
    // Shared per-call `root` override (§7.5).
    let call_root = match s_opt(&args, "root") {
        Ok(r) => r.map(|r| srv.resolve_path_arg(&r)),
        Err(e) => return Err(e),
    };
    match name {
        "wiki_open" => wiki_open(srv, &args, call_root),
        "wiki_init" => wiki_init(srv, &args, call_root),
        _ => dispatch_bound(srv, name, &args, call_root),
    }
}

/// Bound-tool path: resolve the session root (roots/list rung included)
/// or take the per-call `root` override, then run the cmd fn under a
/// fresh `Wiki` handle (§7.3 freshness).
fn dispatch_bound(
    srv: &mut Server,
    name: &str,
    args: &Value,
    call_root: Option<PathBuf>,
) -> Result<Value, RpcError> {
    let root = match call_root {
        Some(r) => Some(r),
        None => {
            srv.auto_bind(true);
            srv.bound.clone()
        }
    };
    let Some(root) = root else {
        return Ok(srv.no_wiki_err());
    };
    let wiki = match srv.open_wiki_at(&root) {
        Ok(w) => w,
        Err(e) => return Ok(srv.domain_err(&e)),
    };
    match run_bound(&wiki, name, args) {
        Ok(payload) => Ok(srv.tool_ok(payload)),
        Err(e) => Ok(srv.domain_err(&e)),
    }
}

fn run_bound(wiki: &crate::store::Wiki, name: &str, a: &Value) -> Result<Value, EngineError> {
    match name {
        "wiki_status" => status::run(wiki),
        "wiki_query" => cmd_query_prepare::run(
            wiki,
            &cmd_query_prepare::QueryPrepareArgs {
                question: req_str(a, "question")?,
                as_of: opt_str(a, "as_of")?,
            },
        ),
        "wiki_search_claims" => {
            let prefix = opt_str(a, "prefix")?;
            let subject = opt_str(a, "subject")?;
            let status_f = opt_str(a, "status")?;
            let subjects = crate::subjects::subjects(wiki, prefix.as_deref())?;
            let mut claims = Vec::new();
            for c in crate::claims::list(wiki)? {
                let row = crate::claims::index_row_from(&c);
                if let Some(s) = &subject {
                    if &row.subject != s {
                        continue;
                    }
                }
                if let Some(st) = &status_f {
                    if &row.status != st {
                        continue;
                    }
                }
                claims.push(json!({
                    "id": row.id,
                    "subject": row.subject,
                    "predicate": row.predicate,
                    "status": row.status,
                    "version": row.version,
                }));
            }
            Ok(json!({"subjects": subjects, "claims": claims}))
        }
        "wiki_read_page" => cmd_page_show::run(
            wiki,
            &cmd_page_show::PageShowArgs {
                artifact: req_str(a, "artifact")?,
            },
        ),
        "wiki_doctor" => {
            // Read path only — `--fix` lanes are never reachable via MCP
            // (§7.5 row 5).
            cmd_doctor::run(
                wiki,
                &cmd_doctor::DoctorArgs {
                    scope: opt_str(a, "scope")?,
                    fix: false,
                    fix_indexes: false,
                    rebuild_index: false,
                    rebuild_fts: false,
                    dry_run: false,
                    codes: Vec::new(),
                },
            )
        }
        "wiki_review" => {
            let op = req_str(a, "op")?;
            match op.as_str() {
                "list" | "show" | "triage" => {}
                "act" | "apply" => {
                    return Err(EngineError::Usage(format!(
                        "wiki_review op {op:?} is CLI-only in v1 (spec §7.5)"
                    )))
                }
                other => {
                    return Err(EngineError::Usage(format!(
                        "wiki_review op must be list|show|triage, got {other:?}"
                    )))
                }
            }
            cmd_review::run(
                wiki,
                &cmd_review::ReviewArgs {
                    review_cmd: Some(op),
                    item: opt_str(a, "item")?,
                    action: None,
                    params: None,
                    file: None,
                    group_by: opt_str(a, "group_by")?,
                    kind: opt_str(a, "kind")?,
                    status: opt_str(a, "status")?,
                    all: opt_bool(a, "all")?.unwrap_or(false),
                    base_revision: opt_i64(a, "base_revision")?,
                },
            )
        }
        "wiki_ingest" => ingest::run(
            wiki,
            &IngestArgs {
                file: opt_path(a, "file")?,
                url: opt_str(a, "url")?,
                dir: opt_path(a, "dir")?,
                text: opt_str(a, "text")?,
                text_ref: opt_str(a, "text_ref")?,
                kind: opt_str(a, "kind")?,
                source_id: opt_str(a, "source_id")?,
                normalized_content: opt_path(a, "normalized_content")?,
                parser_name: opt_str(a, "parser_name")?,
                include_hidden: opt_bool(a, "include_hidden")?.unwrap_or(false),
                max_bytes: opt_u64(a, "max_bytes")?.unwrap_or(DEFAULT_MAX_BYTES),
                triage: opt_path(a, "triage")?,
                force: opt_bool(a, "force")?.unwrap_or(false),
                connector: opt_str(a, "connector")?,
                ref_: opt_str(a, "ref")?,
                limit: opt_u64(a, "limit")?.map(|v| v as usize).unwrap_or(50),
                list_connectors: opt_bool(a, "list_connectors")?.unwrap_or(false),
            },
        ),
        "wiki_compile" => {
            // `resume:true` advances every parked run dir one phase, then
            // continues the build umbrella (§7.5 row 10).
            if opt_bool(a, "resume")?.unwrap_or(false) {
                cmd_compile::run(wiki, &cmd_compile::CompileArgs { resume: true })?;
            }
            cmd_build::run(wiki)
        }
        "wiki_reconcile" => {
            let op = req_str(a, "op")?;
            let run = req_str(a, "run")?;
            match op.as_str() {
                "prepare" => cmd_reconcile_prepare::run(
                    wiki,
                    &cmd_reconcile_prepare::ReconcilePrepareArgs { run },
                ),
                "apply" => cmd_reconcile_apply::run(
                    wiki,
                    &cmd_reconcile_apply::ReconcileApplyArgs {
                        run,
                        classifications: req_path(a, "classifications")?,
                        base_revision: opt_i64(a, "base_revision")?,
                        use_prefill: opt_bool(a, "use_prefill")?.unwrap_or(false),
                    },
                ),
                other => Err(EngineError::Usage(format!(
                    "wiki_reconcile op must be prepare|apply, got {other:?}"
                ))),
            }
        }
        "wiki_write_page" => {
            let file = opt_str(a, "file")?;
            if file.as_deref() == Some("-") {
                return Err(EngineError::Usage(
                    "wiki_write_page file \"-\" (stdin) is unavailable over \
                     MCP — pass `text` or a file path"
                        .into(),
                ));
            }
            cmd_write_page::run(
                wiki,
                &cmd_write_page::WritePageArgs {
                    file: file.map(PathBuf::from),
                    text: opt_str(a, "text")?,
                    artifact: req_str(a, "artifact")?,
                    deps_file: opt_path(a, "deps_file")?,
                    deps: opt_str(a, "deps")?,
                    allow_new: opt_bool(a, "allow_new")?.unwrap_or(false),
                    base_revision: opt_i64(a, "base_revision")?,
                    decide: opt_bool(a, "decide")?.unwrap_or(false),
                },
            )
        }
        "wiki_decide" => cmd_decide::run(
            wiki,
            &cmd_decide::DecideArgs {
                title: req_str(a, "title")?,
                claims: opt_str(a, "claims")?,
                evidence: opt_str_list(a, "evidence")?,
                authority: opt_str(a, "authority")?,
                status: opt_str(a, "status")?.unwrap_or_else(|| "accepted".into()),
                decided_on: opt_str(a, "decided_on")?,
                body: opt_str(a, "body")?,
                base_revision: opt_i64(a, "base_revision")?,
            },
        ),
        other => Err(EngineError::Usage(format!("unknown tool: {other}"))),
    }
}

/// `wiki_open` — §7.5 row 7. `path` (or the shared `root` arg) binds
/// explicitly; absent, discovery re-runs the whole §7.3 chain.
fn wiki_open(
    srv: &mut Server,
    a: &Value,
    call_root: Option<PathBuf>,
) -> Result<Value, RpcError> {
    if let Err(e) = opt_str(a, "path") {
        return Ok(srv.domain_err(&e));
    }
    let explicit = call_root.or_else(|| {
        opt_str(a, "path")
            .ok()
            .flatten()
            .map(|p| srv.resolve_path_arg(&p))
    });
    if let Some(path) = explicit {
        let wiki = match srv.open_wiki_at(&path) {
            Ok(w) => w,
            Err(e) => return Ok(srv.domain_err(&e)),
        };
        let root = wiki.root.clone();
        let initialized = wiki.exists();
        srv.bound = Some(root.clone());
        srv.bound_source = Some("explicit");
        return Ok(srv.tool_ok(json!({
            "root": root,
            "initialized": initialized,
            "source": "explicit",
        })));
    }
    // Re-run discovery: clear binding + roots cache and walk the chain.
    srv.bound = None;
    srv.bound_source = None;
    srv.roots_tried = false;
    srv.client_root_files.clear();
    srv.auto_bind(true);
    let Some(root) = srv.bound.clone() else {
        return Ok(srv.tool_err(json!({
            "error": format!("{NO_WIKI_MSG} with an explicit path — \
                              no wiki root discovered"),
            "exit_code": 2,
            "error_type": "usage",
        })));
    };
    let wiki = match srv.open_wiki_at(&root) {
        Ok(w) => w,
        Err(e) => return Ok(srv.domain_err(&e)),
    };
    Ok(srv.tool_ok(json!({
        "root": wiki.root,
        "initialized": wiki.exists(),
        "source": srv.bound_source.unwrap_or("explicit"),
    })))
}

/// `wiki_init` — §7.5 row 8. `root` (shared arg) > `path` > session
/// binding > auto-discovery; on success the session binds to the path.
fn wiki_init(
    srv: &mut Server,
    a: &Value,
    call_root: Option<PathBuf>,
) -> Result<Value, RpcError> {
    let target = match call_root {
        Some(r) => Some(r),
        None => match opt_str(a, "path").map_err(|e| RpcError::invalid_params(e.to_string()))? {
            Some(p) => Some(srv.resolve_path_arg(&p)),
            None => {
                srv.auto_bind(true);
                srv.bound.clone()
            }
        },
    };
    let Some(root) = target else {
        return Ok(srv.tool_err(json!({
            "error": "wiki_init needs a target: pass `path` (or `root`), \
                      or bind a session root first",
            "exit_code": 2,
            "error_type": "usage",
        })));
    };
    let wiki = match srv.open_wiki_at(&root) {
        Ok(w) => w,
        Err(e) => return Ok(srv.domain_err(&e)),
    };
    let name = match opt_str(a, "name").map_err(|e| RpcError::invalid_params(e.to_string()))? {
        Some(n) => n,
        None => wiki
            .root
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "root".to_string()),
    };
    match store::init_wiki(&wiki, &name) {
        Ok(payload) => {
            srv.bound = Some(wiki.root.clone());
            srv.bound_source = Some("explicit");
            Ok(srv.tool_ok(payload))
        }
        Err(e) => Ok(srv.domain_err(&e)),
    }
}

// ---------------------------------------------------------------- arg parse

fn req_str(a: &Value, k: &str) -> Result<String, EngineError> {
    match a.get(k) {
        Some(Value::String(s)) => Ok(s.clone()),
        Some(_) => Err(EngineError::Usage(format!("`{k}` must be a string"))),
        None => Err(EngineError::Usage(format!("missing required `{k}`"))),
    }
}

fn opt_str(a: &Value, k: &str) -> Result<Option<String>, EngineError> {
    match a.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(EngineError::Usage(format!("`{k}` must be a string"))),
    }
}

fn opt_bool(a: &Value, k: &str) -> Result<Option<bool>, EngineError> {
    match a.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(EngineError::Usage(format!("`{k}` must be a boolean"))),
    }
}

fn opt_i64(a: &Value, k: &str) -> Result<Option<i64>, EngineError> {
    match a.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_i64()
            .map(Some)
            .ok_or_else(|| EngineError::Usage(format!("`{k}` must be an integer"))),
    }
}

fn opt_u64(a: &Value, k: &str) -> Result<Option<u64>, EngineError> {
    match a.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => v
            .as_u64()
            .map(Some)
            .ok_or_else(|| EngineError::Usage(format!("`{k}` must be an integer"))),
    }
}

fn opt_path(a: &Value, k: &str) -> Result<Option<PathBuf>, EngineError> {
    Ok(opt_str(a, k)?.map(PathBuf::from))
}

fn req_path(a: &Value, k: &str) -> Result<PathBuf, EngineError> {
    req_str(a, k).map(PathBuf::from)
}

fn opt_str_list(a: &Value, k: &str) -> Result<Vec<String>, EngineError> {
    match a.get(k) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(items)) => {
            let mut out = Vec::with_capacity(items.len());
            for it in items {
                match it.as_str() {
                    Some(s) => out.push(s.to_string()),
                    None => {
                        return Err(EngineError::Usage(format!(
                            "`{k}` must be an array of strings"
                        )))
                    }
                }
            }
            Ok(out)
        }
        Some(_) => Err(EngineError::Usage(format!("`{k}` must be an array of strings"))),
    }
}

/// `opt_str` wrapper that maps arg-shape errors into `-32602` (they are
/// params-level mistakes for the shared `root` key, surfaced before
/// dispatch).
fn s_opt(a: &Value, k: &str) -> Result<Option<String>, RpcError> {
    opt_str(a, k).map_err(|e| RpcError::invalid_params(e.to_string()))
}

// ---------------------------------------------------------------- tools/list

const TOOL_NAMES: &[&str] = &[
    "wiki_status",
    "wiki_query",
    "wiki_search_claims",
    "wiki_read_page",
    "wiki_doctor",
    "wiki_review",
    "wiki_open",
    "wiki_init",
    "wiki_ingest",
    "wiki_compile",
    "wiki_reconcile",
    "wiki_write_page",
    "wiki_decide",
];

fn prop(t: &str, desc: &str) -> Value {
    json!({"type": t, "description": desc})
}

fn schema(props: Value, required: &[&str]) -> Value {
    let mut p = props.as_object().unwrap().clone();
    p.insert(
        "root".into(),
        prop("string", "project root override for this call only (abs or server-cwd-relative)"),
    );
    json!({
        "type": "object",
        "properties": p,
        "required": required,
        "additionalProperties": true,
    })
}


fn tool(name: &str, desc: &str, input: Value, annotations: Value, structured: bool) -> Value {
    let mut t = json!({
        "name": name,
        "description": desc,
        "inputSchema": input,
        "annotations": annotations,
    });
    if structured {
        t["outputSchema"] = json!({"type": "object"});
    }
    t
}

fn read_only() -> Value {
    json!({
        "readOnlyHint": true,
        "destructiveHint": false,
        "idempotentHint": true,
        "openWorldHint": false,
    })
}

/// `tools/list` — §7.5 rows 1-13; `outputSchema` rides only on
/// ≥2025-06-18 negotiated sessions (§7.2).
pub(crate) fn list(structured: bool) -> Value {
    let ro = read_only();
    let tools = json!([
        tool("wiki_status",
            "Wiki health snapshot — initialized/counts/pending/stale (read-only).",
            schema(json!({}), &[]), ro.clone(), structured),
        tool("wiki_query",
            "Read-only query preparation: matches + superseded + decisions + \
             conflicts + gaps for the agent to synthesize an answer.",
            schema(json!({
                "question": prop("string", "the question to prepare evidence for"),
                "as_of": prop("string", "historical query: answer as of this ISO date/datetime"),
            }), &["question"]), ro.clone(), structured),
        tool("wiki_search_claims",
            "Subject rollup + canonical claim summaries, filterable by \
             prefix/subject/status (read-only).",
            schema(json!({
                "prefix": prop("string", "only subjects starting with this prefix"),
                "subject": prop("string", "only claims with this subject"),
                "status": prop("string", "only claims with this status"),
            }), &[]), ro.clone(), structured),
        tool("wiki_read_page",
            "Frontmatter, body, claim details, staleness and registered \
             deps for one derived page (read-only).",
            schema(json!({
                "artifact": prop("string", "wiki/<kind>/<slug>.md"),
            }), &["artifact"]), ro.clone(), structured),
        tool("wiki_doctor",
            "Static + semantic diagnostics over the wiki (read path only, \
             never --fix). ok:false is a successful domain result.",
            schema(json!({
                "scope": prop("string", "substring filter over the serialized finding"),
            }), &[]), ro.clone(), structured),
        tool("wiki_review",
            "Human review queue — read ops only: list|show|triage \
             (act/apply are CLI-only in v1).",
            schema(json!({
                "op": {"type": "string", "enum": ["list", "show", "triage"],
                       "description": "review sub-operation"},
                "item": prop("string", "queue item id (show)"),
                "status": prop("string", "list: status filter"),
                "kind": prop("string", "list: kind filter"),
                "all": prop("boolean", "list: include deferred/resolved items"),
                "group_by": prop("string", "triage group key (default: subject)"),
                "base_revision": prop("integer", "reserved; mutating ops are CLI-only"),
            }), &["op"]), ro.clone(), structured),
        tool("wiki_open",
            "Bind the session wiki root: explicit `path`, or re-run \
             discovery (flag > env > cwd-walk > client roots).",
            schema(json!({
                "path": prop("string", "absolute or server-cwd-relative root; ~ is not expanded"),
            }), &[]), json!({
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": true,
            }), structured),
        tool("wiki_init",
            "Idempotent scaffold of the .llm-wiki layout at `path` (or the \
             shared `root`); binds the session on success.",
            schema(json!({
                "name": prop("string", "project name (default: root basename)"),
                "path": prop("string", "where the wiki is created"),
            }), &[]), json!({
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false,
            }), structured),
        tool("wiki_ingest",
            "Ingest evidence — file/url/dir/text/triage-manifest/connector; \
             exactly one source lane per call.",
            schema(json!({
                "file": prop("string", "source file path"),
                "url": prop("string", "source URL"),
                "dir": prop("string", "directory to ingest"),
                "text": prop("string", "inline text payload"),
                "text_ref": prop("string", "identity label for --text"),
                "kind": prop("string", "source kind ('session' = episodic)"),
                "source_id": prop("string", "explicit source id"),
                "normalized_content": prop("string", "path to agent-extraction markdown (per-file)"),
                "parser_name": prop("string", "parser label for normalized_content"),
                "include_hidden": prop("boolean", "dir lane: include dotfiles"),
                "max_bytes": prop("integer", "per-part byte cap"),
                "triage": prop("string", "triage manifest path (exclusive lane)"),
                "force": prop("boolean", "bypass sha256 dedup"),
                "connector": prop("string", "preset connector name"),
                "ref": prop("string", "connector endpoint ref"),
                "limit": prop("integer", "max items for connector lane"),
                "list_connectors": prop("boolean", "list connector preset names"),
            }), &[]), json!({
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": false,
                "openWorldHint": true,
            }), structured),
        tool("wiki_compile",
            "Build umbrella: compile → build-pages → verify. `resume:true` \
             advances every parked run dir one phase first.",
            schema(json!({
                "resume": prop("boolean", "advance staged run dirs one phase before building"),
            }), &[]), json!({
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false,
            }), structured),
        tool("wiki_reconcile",
            "Reconcile a staged run: `prepare` emits tier-matched \
             comparisons; `apply` commits the classifications file.",
            schema(json!({
                "op": {"type": "string", "enum": ["prepare", "apply"],
                       "description": "reconcile sub-operation"},
                "run": prop("string", "run id minted by compile or stage-candidates"),
                "classifications": prop("string", "classifications JSON file (apply)"),
                "use_prefill": prop("boolean", "apply: consume auto_prefill blocks"),
                "base_revision": prop("integer", "apply: optimistic-concurrency base"),
            }), &["op", "run"]), json!({
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": false,
                "openWorldHint": false,
            }), structured),
        tool("wiki_write_page",
            "Validate + commit an agent-authored derived page with its \
             dependency registration.",
            schema(json!({
                "artifact": prop("string", "wiki/<kind>/<slug>.md"),
                "text": prop("string", "markdown page body+frontmatter"),
                "file": prop("string", "markdown page file path"),
                "deps": prop("string", "comma-separated dep list"),
                "deps_file": prop("string", "dep list JSON file"),
                "allow_new": prop("boolean", "allow near-duplicate page creation"),
                "decide": prop("boolean", "advisory Jev verdict (env-gated)"),
                "base_revision": prop("integer", "optimistic-concurrency base"),
            }), &["artifact"]), json!({
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": true,
                "openWorldHint": false,
            }), structured),
        tool("wiki_decide",
            "Record a project decision into decisions/ under a commit \
             transaction.",
            schema(json!({
                "title": prop("string", "decision title"),
                "claims": prop("string", "comma-separated claim_<id>s"),
                "evidence": {"type": "array", "items": {"type": "string"},
                             "description": "source_<id>@<version> refs"},
                "authority": prop("string", "authority label"),
                "status": {"type": "string",
                           "enum": ["proposed", "accepted", "superseded", "retracted"]},
                "decided_on": prop("string", "YYYY-MM-DD (default: today, UTC)"),
                "body": prop("string", "rationale text"),
                "base_revision": prop("integer", "optimistic-concurrency base"),
            }), &["title"]), json!({
                "readOnlyHint": false,
                "destructiveHint": false,
                "idempotentHint": false,
                "openWorldHint": false,
            }), structured),
    ]);
    json!({"tools": tools})
}
