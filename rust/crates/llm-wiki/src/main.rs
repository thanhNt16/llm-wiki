//! `llm-wiki` CLI entry point — clap surface over the `llm-wiki` crate.
//!
//! Contract (spec §6): every invocation prints exactly one JSON object on
//! stdout and exits 0/2/3/4/5. Clap parse failures — including
//! subparser-level missing/invalid args — are re-emitted as
//! `{"error","exit_code":2,"error_type":"usage"}` JSON, never clap's
//! human rendering. `--root` > `LLM_WIKI_ROOT` > cwd (no ancestor walk —
//! MCP-only). `--allow-repair` bypasses the commit repair gate on every
//! txn-owning command. `--wait-lock`/`--no-wait` are rejected outside the
//! commit-taking command whitelist (`takes_commit_lock`).
use std::path::{Path, PathBuf};
use std::time::Duration;

use clap::{Parser, Subcommand};
use llm_wiki::cmd::check_evidence::{self, CheckEvidenceArgs};
use llm_wiki::cmd::doctor as cmd_doctor;
use llm_wiki::cmd::dream as cmd_dream;
use llm_wiki::cmd::ingest::{self, IngestArgs, DEFAULT_MAX_BYTES};
use llm_wiki::cmd::ingest_feed::{self, IngestFeedArgs};
use llm_wiki::cmd::ingest_git::{self, IngestGitArgs};
use llm_wiki::cmd::triage::{self, TriageArgs};
use llm_wiki::cmd::verify as cmd_verify;
use llm_wiki::cmd::subjects as cmd_subjects;
use llm_wiki::cmd::graph_data as cmd_graph_data;
use llm_wiki::cmd::export_okf as cmd_export_okf;
use llm_wiki::cmd::graph_export as cmd_graph_export;
use llm_wiki::cmd::viz_serve as cmd_viz_serve;
use llm_wiki::cmd::decide as cmd_decide;
use llm_wiki::cmd::record_decision as cmd_record_decision;
use llm_wiki::cmd::review as cmd_review;
use llm_wiki::cmd::{status, topics as cmd_topics};
use llm_wiki::cmd::page_targets as cmd_page_targets;
use llm_wiki::cmd::write_page as cmd_write_page;
use llm_wiki::cmd::page_show as cmd_page_show;
use llm_wiki::cmd::build_pages as cmd_build_pages;
use llm_wiki::cmd::build as cmd_build;
use llm_wiki::emit;
use llm_wiki::cmd::compile as cmd_compile;
use llm_wiki::cmd::compile_plan as cmd_compile_plan;
use llm_wiki::cmd::stage_candidates as cmd_stage_candidates;
use llm_wiki::cmd::reconcile_prepare as cmd_reconcile_prepare;
use llm_wiki::cmd::reconcile_apply as cmd_reconcile_apply;
use llm_wiki::cmd::context_pack as cmd_context_pack;
use llm_wiki::cmd::jev as cmd_jev;
use llm_wiki::cmd::query_prepare as cmd_query_prepare;
use llm_wiki::cmd::mcp as cmd_mcp;
use llm_wiki::cmd::mcp_register as cmd_mcp_register;
use llm_wiki::mcp_register::McpClient;
use llm_wiki::skills_embed::{self, Preset};
use llm_wiki::err::EngineError;
use llm_wiki::store::{self, Wiki};
use llm_wiki::txn::lock::Lock;

#[derive(Parser)]
#[command(name = "llm-wiki", version, about = "llm-wiki deterministic core")]
struct Cli {
    /// project root (default: cwd; also LLM_WIKI_ROOT)
    #[arg(long, global = true)]
    root: Option<PathBuf>,
    /// wait for an in-flight commit to release commit.lock instead of
    /// exiting 5 on contention; `=SECS` bounds the wait (default: block).
    /// Only valid on commands that run a commit transaction.
    #[arg(long, global = true, num_args = 0..=1, require_equals = true,
        value_name = "SECS")]
    wait_lock: Option<Option<f64>>,
    /// explicitly request the default: fail immediately on a held lock.
    /// Overrides --wait-lock (wiki.py store_const parity).
    #[arg(long, global = true)]
    no_wait: bool,
    /// bypass the leftover-journal repair gate on commit (else exit 3).
    /// Applies to every command that runs a commit transaction.
    #[arg(long, global = true)]
    allow_repair: bool,
    /// install the embedded skill set (binary variant, no scripts/) into
    /// DIR (default ~/.omp/agent/skills). Top-level flag, not a subcommand;
    /// cannot be combined with a subcommand (spec §12).
    #[arg(long, value_name = "DIR", num_args = 0..=1)]
    install_skills: Option<Option<PathBuf>>,
    /// --install-skills preset: target ~/.agents/skills.
    #[arg(long, requires = "install_skills", conflicts_with = "cellockai")]
    agents: bool,
    /// --install-skills preset: target ~/.cellockai/skills.
    #[arg(long, requires = "install_skills")]
    cellockai: bool,
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Idempotent, non-destructive scaffold of the .llm-wiki layout.
    Init {
        /// project root to initialize (overrides --root; default: cwd)
        path: Option<PathBuf>,
        /// project name (default: root directory basename)
        #[arg(long)]
        name: Option<String>,
    },
    /// Ingest evidence — files, inline text, or URLs become versioned sources.
    Ingest {
        #[arg(long)]
        file: Option<PathBuf>,
        #[arg(long)]
        url: Option<String>,
        #[arg(long)]
        dir: Option<PathBuf>,
        #[arg(long)]
        include_hidden: bool,
        #[arg(long, default_value_t = DEFAULT_MAX_BYTES)]
        max_bytes: u64,
        #[arg(long)]
        text: Option<String>,
        /// identity label for --text input
        #[arg(long)]
        text_ref: Option<String>,
        /// source kind for --text/--file: 'session' marks episodic session evidence
        #[arg(long, value_parser = ["session"])]
        kind: Option<String>,
        #[arg(long)]
        source_id: Option<String>,
        /// agent-produced extraction markdown file
        #[arg(long)]
        normalized_content: Option<PathBuf>,
        /// parser label for --normalized-content, e.g. agent-vision
        #[arg(long)]
        parser_name: Option<String>,
        /// bypass sha256 dedup and append a new version
        #[arg(long)]
        force: bool,
        /// batch-ingest a manifest produced by `llm-wiki triage`
        #[arg(long, value_name = "MANIFEST")]
        triage: Option<PathBuf>,
        /// preset connector name (see --list-connectors)
        #[arg(long)]
        connector: Option<String>,
        /// endpoint ref for --connector (URL or repo path)
        #[arg(long = "ref")]
        ref_: Option<String>,
        /// max items for --connector
        #[arg(long, default_value_t = 50)]
        limit: usize,
        /// list connector preset names and exit (read-only)
        #[arg(long)]
        list_connectors: bool,
    },
    /// Ingest git commit history as evidence sources.
    IngestGit {
        /// path to a git repository (same as --repo)
        repo: Option<PathBuf>,
        /// path to a git repository (wiki.py --repo parity; default: cwd)
        #[arg(long = "repo", value_name = "REPO", conflicts_with = "repo")]
        repo_flag: Option<PathBuf>,
        /// max commits (oldest-first)
        #[arg(long, default_value_t = 200)]
        max: usize,
        #[arg(long)]
        force: bool,
    },
    /// Ingest an RSS/Atom feed (URL or file) as evidence sources.
    IngestRss {
        /// feed URL (http/https); wins over --file when both given
        #[arg(long)]
        url: Option<String>,
        /// local feed file
        #[arg(long)]
        file: Option<PathBuf>,
        /// max items
        #[arg(long, default_value_t = 50)]
        limit: usize,
        #[arg(long, default_value_t = DEFAULT_MAX_BYTES)]
        max_bytes: u64,
        #[arg(long)]
        force: bool,
    },
    /// Wiki health snapshot — initialized/counts/pending/stale (read-only).
    Status {},
    /// Topic registry listing; --backfill rebuilds it from claims.
    Topics {
        /// rebuild the registry from the claims scan (idempotent)
        #[arg(long)]
        backfill: bool,
    },
    /// Evidence-lane audit — manifest-pinned sources/raw payloads vs disk.
    CheckEvidence {
        /// emit the JSON report (default output; kept for the flag contract)
        #[arg(long)]
        json: bool,
        /// re-hash manifest sha256 values against raw/ payload bytes
        #[arg(long)]
        verify: bool,
        /// max entries in the bounded issues[] detail list
        #[arg(long, default_value_t = 100)]
        details: usize,
    },
    /// Deterministic ingest proposals for a directory's top-level files
    /// (no LLM); adds incremental changed/new/removed cursor buckets once
    /// a wiki exists (spec §8).
    Triage {
        /// directory of files to scan (top level)
        dir: Option<PathBuf>,
        /// same as the positional dir (wiki.py --dir parity)
        #[arg(long = "dir", value_name = "DIR", conflicts_with = "dir")]
        dir_flag: Option<PathBuf>,
        /// also write the manifest JSON to this path (stdout always)
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Synthesis targets over eligible claims: kind routing, staleness and
    /// corroboration reasons per subject (read-only).
    PageTargets {
        /// restrict to one page kind
        #[arg(long, value_parser = ["concepts", "entities", "procedures",
            "questions", "sources", "changes"])]
        kind: Option<String>,
        /// only targets whose artifact is stale
        #[arg(long)]
        stale_only: bool,
        #[arg(long, default_value_t = 2)]
        min_claims: i64,
        #[arg(long, default_value_t = 2)]
        min_sources: i64,
        /// attach advisory Jev kind verdicts to heuristic-routed targets
        /// (no-op when Jev env is unset)
        #[arg(long)]
        decide: bool,
    },
    /// Validate + commit an agent-authored wiki page with its dependency
    /// registration (commit.lock).
    WritePage {
        /// markdown page file, '-' for stdin
        #[arg(long)]
        file: Option<PathBuf>,
        /// markdown page as a string argument
        #[arg(long)]
        text: Option<String>,
        /// target wiki path, e.g. wiki/concepts/foo.md
        #[arg(long, required = true)]
        artifact: String,
        /// JSON file: dep list, or a page-targets target dict
        #[arg(long = "deps-file")]
        deps_file: Option<PathBuf>,
        /// comma-separated dep list (overrides deps: frontmatter)
        #[arg(long)]
        deps: Option<String>,
        /// allow creating a page that near-duplicates an existing one
        #[arg(long)]
        allow_new: bool,
        /// optimistic-concurrency base revision
        #[arg(long = "base-revision")]
        base_revision: Option<i64>,
        /// attach an advisory Jev same-page verdict to a near-duplicate
        /// refusal (no-op when Jev env is unset)
        #[arg(long)]
        decide: bool,
    },
    /// Frontmatter, body, claim details, staleness and registered deps for
    /// one derived page (read-only).
    PageShow {
        /// wiki/<kind>/<slug>.md
        #[arg(long, required = true)]
        artifact: String,
    },
    /// (Re)build deterministic derived views (indexes, overview, review,
    /// log, changes) and register their deps (commit.lock).
    BuildPages {
        /// optimistic-concurrency base revision
        #[arg(long = "base-revision")]
        base_revision: Option<i64>,
    },
    /// Compile pipeline umbrella: extract -> classify -> apply over
    /// staged run dirs (wiki.py compile; --resume advances one phase).
    Compile {
        /// advance every staged run one phase
        #[arg(long)]
        resume: bool,
    },
    /// Read-only pending-evidence plan (wiki.py compile-plan).
    CompilePlan {},
    /// One command: compile (until agent input is required) → build-pages
    /// → verify. Exits 3 with the exact next action when a human/agent
    /// step (candidate extraction or needs_review classification) is
    /// pending; never invents content (wiki.py cmd_build).
    Build {},
    /// Stage agent-extracted candidates into a run dir
    /// (wiki.py stage-candidates).
    StageCandidates {
        /// candidates JSON or JSONL file
        #[arg(long, required = true)]
        file: PathBuf,
        #[arg(long = "source-id", required = true)]
        source_id: String,
        #[arg(long = "source-version", required = true)]
        source_version: i64,
        /// extraction report JSON file
        #[arg(long)]
        report: Option<PathBuf>,
        /// stage into an existing pending run dir instead of minting one
        #[arg(long)]
        run: Option<String>,
    },
    /// Tier-matched comparisons for a staged run
    /// (wiki.py reconcile-prepare; bounded matches[] per spec §6.4).
    ReconcilePrepare {
        /// run id minted by compile or stage-candidates
        #[arg(long, required = true)]
        run: String,
    },
    /// Commit a prepared run's classifications
    /// (wiki.py reconcile-apply + spec §6.4 --use-prefill).
    ReconcileApply {
        /// run id minted by compile or stage-candidates
        #[arg(long, required = true)]
        run: String,
        /// classifications JSON file
        #[arg(long, required = true)]
        classifications: PathBuf,
        /// optimistic-concurrency base revision (default: current)
        #[arg(long = "base-revision")]
        base_revision: Option<i64>,
        /// consume ≥0.7-confidence auto_prefill blocks (spec §6.4)
        #[arg(long = "use-prefill")]
        use_prefill: bool,
    },
    /// Citation-resolution gate: derived-page deps resolve and nothing
    /// stale stays current; audit checks ride under `checks`.
    Verify {},
    /// Static + semantic diagnostics over the wiki — findings payload,
    /// `ok:false -> exit 3` (spec §9.4 read-path; --fix lands in P3).
    Doctor {
        /// substring filter over the serialized finding (wiki.py:617-623)
        #[arg(long)]
        scope: Option<String>,
        /// repair findings (spec §9 — one commit + post-commit index rebuild)
        #[arg(long)]
        fix: bool,
        /// legacy alias: repair section indexes (spec §9.3)
        #[arg(long)]
        fix_indexes: bool,
        /// legacy alias: rebuild index.sqlite (spec §9.3)
        #[arg(long)]
        rebuild_index: bool,
        /// legacy alias: rebuild the FTS index (spec §9.3)
        #[arg(long)]
        rebuild_fts: bool,
        /// simulate fixes without mutating (--fix modifier)
        #[arg(long)]
        dry_run: bool,
        /// restrict fixes to a finding code; repeatable
        #[arg(long = "code", value_name = "CODE")]
        codes: Vec<String>,
    },
    /// Corpus reconciliation + GC: archive superseded/expired/dup/orphan
    /// claims, drop dead dep edges, delete junk and all-deps-dead pages.
    Dream {
        /// execute the plan (default: dry-run report)
        #[arg(long)]
        apply: bool,
        /// run only this phase (repeatable)
        #[arg(long, value_parser = llm_wiki::dream::PHASES)]
        phase: Vec<String>,
        /// delete _archive entries older than --retention-days and unreferenced
        #[arg(long)]
        purge: bool,
        /// with --purge: plan only, delete nothing
        #[arg(long)]
        dry_run: bool,
        #[arg(long, default_value_t = 90)]
        retention_days: i64,
    },
    /// Read-only subject rollup: predicates, status counts, distinct
    /// evidence sources, stale pages per subject (wiki.py subjects).
    Subjects {
        /// only subjects starting with this prefix
        #[arg(long)]
        prefix: Option<String>,
    },
    /// Knowledge-graph JSON payload for the wiki-visualize app (read-only).
    GraphData {
        /// write JSON to file instead of stdout
        #[arg(long)]
        output: Option<PathBuf>,
        /// embed full claim dicts in entity nodes (default emits claim_ids)
        #[arg(long)]
        full: bool,
    },
    /// Write deterministic OKF v0.2 bundle (openwiki-consumable).
    ExportOkf {
        /// destination directory (replaced atomically)
        output: PathBuf,
    },
    /// Write self-contained static visualizer bundle.
    GraphExport {
        /// destination directory (replaced atomically)
        output: PathBuf,
        /// prebuilt UI dist dir (default: auto-detect; WIKI_UI_DIST env)
        #[arg(long)]
        ui_dist: Option<PathBuf>,
    },
    /// Serve the visualizer on 127.0.0.1 with SSE live reload.
    VizServe {
        /// first candidate port (scan covers port..=port+20)
        #[arg(long, default_value_t = 4333)]
        port: u16,
        /// do not open the browser
        #[arg(long)]
        no_open: bool,
        /// prebuilt UI dist dir (default: auto-detect; WIKI_UI_DIST env)
        #[arg(long)]
        ui_dist: Option<PathBuf>,
    },
    /// Human review queue: list|show|triage|apply|act
    /// (wiki.py review; act/apply take commit.lock).
    Review {
        /// review sub-command: list|show|triage|apply|act
        review_cmd: Option<String>,
        /// queue item id (show/act)
        #[arg(long)]
        item: Option<String>,
        /// action for act: accept|reject|merge|mark_duplicate|
        /// mark_authoritative|mark_superseded|set_scope|set_validity|
        /// defer|reopen|set_topic_kind
        #[arg(long)]
        action: Option<String>,
        /// params JSON file (act)
        #[arg(long)]
        params: Option<PathBuf>,
        /// review apply: JSON list of {"id","action","params"}
        /// (or {"actions": [...]})
        #[arg(long)]
        file: Option<PathBuf>,
        /// review triage: group key (default: linked claim subject;
        /// any other value names an item field)
        #[arg(long = "group-by")]
        group_by: Option<String>,
        /// list: kind filter
        #[arg(long)]
        kind: Option<String>,
        /// list: status filter
        #[arg(long)]
        status: Option<String>,
        /// list: include deferred items too
        #[arg(long)]
        all: bool,
        /// optimistic-concurrency base revision (act/apply)
        #[arg(long = "base-revision")]
        base_revision: Option<i64>,
    },
    /// Record a project decision into decisions/ (flag-driven;
    /// see also record-decision).
    Decide {
        /// decision title (required)
        #[arg(long)]
        title: String,
        /// comma-separated claim_<id>s the decision resolves
        #[arg(long)]
        claims: Option<String>,
        /// evidence ref source_<id>@<version>; repeatable
        #[arg(long = "evidence", value_name = "SOURCE@VER")]
        evidence: Vec<String>,
        /// authority label, e.g. explicit_project_decision or human_review
        #[arg(long)]
        authority: Option<String>,
        /// decision status
        #[arg(long, default_value = "accepted",
            value_parser = ["proposed", "accepted", "superseded", "retracted"])]
        status: String,
        /// YYYY-MM-DD (default: today, UTC)
        #[arg(long = "decided-on")]
        decided_on: Option<String>,
        /// rationale text
        #[arg(long)]
        body: Option<String>,
        /// optimistic-concurrency base revision
        #[arg(long = "base-revision")]
        base_revision: Option<i64>,
    },
    /// Commit a decision record from a JSON file (engine stamps
    /// id/recorded_at/version).
    RecordDecision {
        /// decision input JSON file
        #[arg(long, required = true)]
        file: PathBuf,
    },
    /// Advisory System One client — env-gated one-shot typed queries and
    /// readiness check (wiki.py jev; answers cache in index.sqlite).
    Jev {
        /// report Jev readiness (exit 0 either way)
        #[arg(long)]
        check: bool,
        /// state text for a one-shot query
        #[arg(long)]
        state: Option<String>,
        /// questions JSON object, e.g. '{"q":{"type":"noul","instructions":"..."}}'
        #[arg(long)]
        question: Option<String>,
    },
    /// Smallest useful context for a task under a hard token budget;
    /// commits pack + receipt under commit.lock (wiki.py context-pack).
    ContextPack {
        /// task description driving relevance filtering
        #[arg(long, default_value = "")]
        task: String,
        /// hard token budget (default: context_budget_default)
        #[arg(long)]
        budget: Option<i64>,
        /// change briefing since the latest pack receipt
        #[arg(long)]
        resume: bool,
        /// diff base: a context id or ISO-8601 timestamp
        #[arg(long = "changes-since")]
        changes_since: Option<String>,
        /// print the pack file itself instead of the JSON payload
        #[arg(long = "print-pack")]
        print_pack: bool,
    },

    /// Read-only query preparation: matches + superseded + decisions +
    /// conflicts + gaps for the agent to synthesize an answer (wiki.py
    /// query-prepare; always exits 0 — gaps are a domain answer).
    QueryPrepare {
        /// the question to prepare evidence for
        #[arg(long, required = true)]
        question: String,
        /// historical query: answer as of this ISO date/datetime
        #[arg(long = "as-of")]
        as_of: Option<String>,
    },
    /// MCP stdio server — newline-delimited JSON-RPC 2.0 (spec §7);
    /// exits 0 on stdin EOF. Root binding: --root > LLM_WIKI_ROOT >
    /// cwd-ancestor walk > client roots/list > unbound.
    Mcp {},
    /// Register the bare `llm-wiki mcp` server in an agent client's
    /// config and install the CLAUDE.md/AGENTS.md managed memory block
    /// (spec §7.8). Global/user scope: client config under HOME, doc
    /// block under the resolved project root (cwd by default).
    McpRegister {
        /// agent client: claude-code | cursor | claude-desktop | codex
        #[arg(long, required = true, value_enum)]
        client: McpClient,
        /// remove the registration and the managed doc block
        #[arg(long)]
        uninstall: bool,
    },
}

fn main() {
    // Keep the sqlite projection fresh inside every commit's critical
    // section (core `Txn` is index-free; the hook seam is §10.2).
    llm_wiki::install_commit_hook();
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            // clap Error::use_stderr() == false only for --help/--version,
            // which print to stdout and exit 0. Everything else is a usage
            // failure → JSON on stdout, exit 2 (spec §6.2).
            if e.use_stderr() {
                emit::err(&EngineError::Usage(e.to_string()));
            }
            println!("{}", e);
            std::process::exit(0);
        }
    };
    match run(cli) {
        Ok(v) => emit::ok(v),
        Err(e) => emit::err(&e),
    }
}

fn run(cli: Cli) -> Result<serde_json::Value, EngineError> {
    // --install-skills is a top-level flag, not a subcommand (spec §12):
    // it short-circuits before any wiki resolution — the binary variant of
    // the skill tree is embedded, so no checkout or .llm-wiki is needed.
    if let Some(dir) = &cli.install_skills {
        if cli.command.is_some() {
            return Err(EngineError::Usage(
                "--install-skills cannot be combined with a subcommand".to_string(),
            ));
        }
        if cli.wait_lock.is_some() || cli.no_wait {
            return Err(EngineError::Usage(
                "--wait-lock/--no-wait apply only to commands that run a commit \
                 transaction".to_string(),
            ));
        }
        let home = std::env::var_os("HOME")
            .or_else(|| std::env::var_os("USERPROFILE"))
            .map(PathBuf::from);
        let target = if cli.agents {
            home.as_ref().map(|h| Preset::Agents.dir(h))
        } else if cli.cellockai {
            home.as_ref().map(|h| Preset::Cellockai.dir(h))
        } else {
            match dir {
                Some(d) => Some(d.clone()),
                None => home.as_ref().map(|h| Preset::Omp.dir(h)),
            }
        }
        .ok_or_else(|| EngineError::Usage("cannot resolve home directory (HOME unset)".to_string()))?;
        if (cli.agents || cli.cellockai) && dir.is_some() {
            return Err(EngineError::Usage(
                "--agents/--cellockai are mutually exclusive with --install-skills DIR"
                    .to_string(),
            ));
        }
        return skills_embed::install(&target);
    }
    let cmd = match cli.command {
        Some(c) => c,
        None => {
            return Err(EngineError::Usage(
                "missing subcommand (see --help)".to_string(),
            ))
        }
    };
    let lock_flag = cli.wait_lock.is_some() || cli.no_wait;
    if lock_flag && !takes_commit_lock(&cmd) {
        return Err(EngineError::Usage(format!(
            "--wait-lock/--no-wait apply only to commands that run a commit \
             transaction (see --help): {}",
            command_name(&cmd)
        )));
    }
    let root = resolve_root(cli.root.clone())?;
    // --wait-lock rides out an in-flight commit: best-effort acquire+release
    // on .state/locks/commit.lock before dispatch (TOCTOU — the real commit
    // still try_locks and can exit 5 on a fresh interloper). Skipped when the
    // wiki isn't initialized so a failed command doesn't create .state/locks/
    // in a non-wiki directory (wiki.py:1024 parity). --no-wait overrides.
    if !cli.no_wait {
        if let Some(secs) = cli.wait_lock {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            if wiki.exists() {
                let timeout = secs.map(|s| {
                    if s.is_finite() {
                        Duration::from_secs_f64(s.max(0.0))
                    } else {
                        Duration::MAX
                    }
                });
                Lock::wait_until_free(&wiki, timeout)?;
            }
        }
    }
    match cmd {
        Commands::Init { path, name } => {
            let root = match path {
                Some(p) => p,
                None => root,
            };
            let wiki = open_wiki(&root, cli.allow_repair)?;
            let name = match name {
                Some(n) => n,
                None => wiki
                    .root
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "root".to_string()),
            };
            store::init_wiki(&wiki, &name)
        }
        Commands::Ingest {
            file,
            url,
            dir,
            include_hidden,
            max_bytes,
            text,
            text_ref,
            kind,
            source_id,
            normalized_content,
            parser_name,
            force,
            triage,
            connector,
            ref_,
            limit,
            list_connectors,
        } => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            ingest::run(
                &wiki,
                &IngestArgs {
                    file,
                    url,
                    dir,
                    include_hidden,
                    max_bytes,
                    text,
                    text_ref,
                    kind,
                    source_id,
                    normalized_content,
                    parser_name,
                    force,
                    triage,
                    connector,
                    ref_,
                    limit,
                    list_connectors,
                },
            )
        }
        Commands::Status {} => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            status::run(&wiki)
        }
        Commands::Topics { backfill } => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            cmd_topics::run(&wiki, &cmd_topics::TopicsArgs { backfill })
        }
        Commands::CheckEvidence {
            json,
            verify,
            details,
        } => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            check_evidence::run(
                &wiki,
                &CheckEvidenceArgs {
                    json,
                    verify,
                    details,
                },
            )
        }
        Commands::IngestGit {
            repo,
            repo_flag,
            max,
            force,
        } => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            let repo = repo_flag.or(repo).unwrap_or_else(|| PathBuf::from("."));
            ingest_git::run(&wiki, &IngestGitArgs { repo, max, force })
        }
        Commands::IngestRss {
            url,
            file,
            limit,
            max_bytes,
            force,
        } => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            ingest_feed::run(
                &wiki,
                &IngestFeedArgs {
                    url,
                    file,
                    limit,
                    max_bytes,
                    force,
                },
            )
        }
        Commands::Triage { dir, dir_flag, out } => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            let dir = match dir.or(dir_flag) {
                Some(d) => d,
                None => {
                    return Err(EngineError::Usage(
                        "triage requires a directory argument".into(),
                    ))
                }
            };
            triage::run(&wiki, &TriageArgs { dir, out })
        }
        Commands::PageTargets {
            kind,
            stale_only,
            min_claims,
            min_sources,
            decide,
        } => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            cmd_page_targets::run(
                &wiki,
                &cmd_page_targets::PageTargetsArgs {
                    kind,
                    stale_only,
                    min_claims,
                    min_sources,
                    decide,
                },
            )
        }
        Commands::WritePage {
            file,
            text,
            artifact,
            deps_file,
            deps,
            allow_new,
            base_revision,
            decide,
        } => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            cmd_write_page::run(
                &wiki,
                &cmd_write_page::WritePageArgs {
                    file,
                    text,
                    artifact,
                    deps_file,
                    deps,
                    allow_new,
                    base_revision,
                    decide,
                },
            )
        }
        Commands::PageShow { artifact } => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            cmd_page_show::run(&wiki, &cmd_page_show::PageShowArgs { artifact })
        }
        Commands::BuildPages { base_revision } => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            cmd_build_pages::run(
                &wiki,
                &cmd_build_pages::BuildPagesArgs { base_revision },
            )
        }
        Commands::Compile { resume } => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            cmd_compile::run(&wiki, &cmd_compile::CompileArgs { resume })
        }
        Commands::CompilePlan {} => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            cmd_compile_plan::run(&wiki)
        }
        Commands::Build {} => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            let payload = cmd_build::run(&wiki)?;
            // `blocked` and `ok:false` verify are domain results
            // (spec §6.2): emit the payload with its exit code.
            let code = cmd_build::exit_code(&payload);
            emit::report(payload, code);
        }
        Commands::StageCandidates {
            file,
            source_id,
            source_version,
            report,
            run,
        } => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            cmd_stage_candidates::run(
                &wiki,
                &cmd_stage_candidates::StageCandidatesArgs {
                    file,
                    source_id,
                    source_version,
                    report,
                    run,
                },
            )
        }
        Commands::ReconcilePrepare { run } => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            cmd_reconcile_prepare::run(
                &wiki,
                &cmd_reconcile_prepare::ReconcilePrepareArgs { run },
            )
        }
        Commands::ReconcileApply {
            run,
            classifications,
            base_revision,
            use_prefill,
        } => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            cmd_reconcile_apply::run(
                &wiki,
                &cmd_reconcile_apply::ReconcileApplyArgs {
                    run,
                    classifications,
                    base_revision,
                    use_prefill,
                },
            )
        }
        Commands::Verify {} => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            let payload = cmd_verify::run(&wiki)?;
            // `ok:false` is a successful domain result, not an error
            // (spec §6.2): emit the report payload with exit 3.
            let code = cmd_verify::exit_code(&payload);
            emit::report(payload, code);
        }
        Commands::Doctor {
            scope,
            fix,
            fix_indexes,
            rebuild_index,
            rebuild_fts,
            dry_run,
            codes,
        } => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            let payload = cmd_doctor::run(
                &wiki,
                &cmd_doctor::DoctorArgs {
                    scope,
                    fix,
                    fix_indexes,
                    rebuild_index,
                    rebuild_fts,
                    dry_run,
                    codes,
                },
            )?;
            let code = cmd_doctor::exit_code(&payload);
            emit::report(payload, code);
        }
        Commands::Dream {
            apply,
            phase,
            purge,
            dry_run,
            retention_days,
        } => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            cmd_dream::run(
                &wiki,
                &cmd_dream::DreamArgs {
                    apply,
                    phases: phase,
                    purge,
                    dry_run,
                    retention_days,
                },
            )
        }
        Commands::Subjects { prefix } => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            cmd_subjects::run(&wiki, &cmd_subjects::SubjectsArgs { prefix })
        }
        Commands::GraphData { output, full } => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            cmd_graph_data::run(&wiki, &cmd_graph_data::GraphDataArgs { output, full })
        }
        Commands::ExportOkf { output } => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            cmd_export_okf::run(&wiki, &output)
        }
        Commands::GraphExport { output, ui_dist } => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            cmd_graph_export::run(&wiki, &output, ui_dist.as_ref())
        }
        Commands::VizServe {
            port,
            no_open,
            ui_dist,
        } => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            cmd_viz_serve::run(&wiki, &cmd_viz_serve::VizServeArgs { port, no_open, ui_dist })
        }
        Commands::Review {
            review_cmd,
            item,
            action,
            params,
            file,
            group_by,
            kind,
            status,
            all,
            base_revision,
        } => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            cmd_review::run(
                &wiki,
                &cmd_review::ReviewArgs {
                    review_cmd,
                    item,
                    action,
                    params,
                    file,
                    group_by,
                    kind,
                    status,
                    all,
                    base_revision,
                },
            )
        }
        Commands::Decide {
            title,
            claims,
            evidence,
            authority,
            status,
            decided_on,
            body,
            base_revision,
        } => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            cmd_decide::run(
                &wiki,
                &cmd_decide::DecideArgs {
                    title,
                    claims,
                    evidence,
                    authority,
                    status,
                    decided_on,
                    body,
                    base_revision,
                },
            )
        }
        Commands::ContextPack {
            task,
            budget,
            resume,
            changes_since,
            print_pack,
        } => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            let out = cmd_context_pack::run(
                &wiki,
                &cmd_context_pack::ContextPackArgs {
                    task: Some(task),
                    budget,
                    resume,
                    changes_since,
                    print_pack,
                },
            )?;
            if print_pack {
                // wiki.py cmd_context_pack: --print-pack prints the pack
                // file verbatim (print adds a trailing newline), not JSON.
                let text = std::fs::read_to_string(
                    wiki.p(out["pack_path"].as_str().unwrap_or("")),
                )?;
                println!("{text}");
                std::process::exit(0);
            }
            Ok(out)
        }
        Commands::RecordDecision { file } => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            cmd_record_decision::run(&wiki, &file)
        }
        Commands::Jev {
            check,
            state,
            question,
        } => {
            // jev is env-gated, not wiki-gated: --check works outside a
            // wiki; the root only locates the answer cache when one exists.
            let wiki = open_wiki(&root, cli.allow_repair)?;
            cmd_jev::run(
                &wiki.root,
                &cmd_jev::JevArgs {
                    check,
                    state,
                    question,
                },
            )
        }
        Commands::QueryPrepare { question, as_of } => {
            let wiki = open_wiki(&root, cli.allow_repair)?;
            cmd_query_prepare::run(
                &wiki,
                &cmd_query_prepare::QueryPrepareArgs { question, as_of },
            )
        }
        // `mcp` bypasses resolve_root: the server session owns the §7.3
        // chain (raw --root flag rung first), so the CLI's arg>env>cwd
        // shortcut must not pre-bind a root here.
        Commands::Mcp {} => cmd_mcp::run(cli.root.clone()),
        Commands::McpRegister { client, uninstall } => {
            cmd_mcp_register::run(client, uninstall, &root)
        }
    }
}

/// The `Wiki::open` seam: `--allow-repair` lands on the Wiki so
/// `Txn::begin_with` applies it on every txn-owning path — current and
/// any commands added later (spec §6.1 global).
fn open_wiki(root: &Path, allow_repair: bool) -> Result<Wiki, EngineError> {
    let mut wiki = Wiki::open(root)?;
    wiki.allow_repair = allow_repair;
    Ok(wiki)
}

/// `--root` > `LLM_WIKI_ROOT` > cwd (spec §6.1).
fn resolve_root(root: Option<PathBuf>) -> Result<PathBuf, EngineError> {
    if let Some(p) = root {
        return Ok(p);
    }
    if let Ok(p) = std::env::var("LLM_WIKI_ROOT") {
        if !p.is_empty() {
            return Ok(PathBuf::from(p));
        }
    }
    Ok(std::env::current_dir()?)
}

/// Commands that run a commit transaction — the `--wait-lock`/`--no-wait`
/// whitelist (wiki.py `_MUTATING_COMMANDS`, restricted to the implemented
/// surface). `init` scaffolds rather than transacts; `status`, plain
/// `topics`, and `ingest --list-connectors` are read-only. New mutating
/// commands (e.g. `ingest-git`, `ingest-rss`, `review act|apply`,
/// `doctor --rebuild-*`) MUST be added here when they land.
fn takes_commit_lock(cmd: &Commands) -> bool {
    match cmd {
        Commands::Ingest {
            list_connectors, ..
        } => !*list_connectors,
        Commands::Topics { backfill } => *backfill,
        // `triage` commits the dir-cursor map (spec §8) once a wiki exists.
        // `doctor --fix*` takes commit.lock (§6.3 membership); --dry-run is
        // excluded — it mutates nothing (§6.3). A bare --code implies the
        // fix path (same membership rule as cmd::doctor::wants_lock).
        Commands::Doctor {
            fix,
            fix_indexes,
            rebuild_index,
            rebuild_fts,
            dry_run,
            codes,
            ..
        } => (*fix || *fix_indexes || *rebuild_index || *rebuild_fts || !codes.is_empty())
            && !*dry_run,
        // `dream` mutates with --apply; a plain run is a dry report.
        // --purge mutates only when it will actually delete
        // (dry_run or no --apply → plan-only per cmd_dream).
        Commands::Dream {
            apply,
            purge,
            dry_run,
            ..
        } => *apply && !(*purge && *dry_run),
        // wiki.py `_needs_lock_wait`: review act/apply only; `decide` is a
        // _MUTATING_COMMANDS member; `record-decision` is not (no wait).
        Commands::Review { review_cmd, .. } => {
            matches!(review_cmd.as_deref(), Some("act") | Some("apply"))
        }
        Commands::Decide { .. } => true,
        Commands::Triage { .. }
        | Commands::IngestGit { .. }
        | Commands::IngestRss { .. } => true,
        // build-pages, write-page and context-pack are _MUTATING_COMMANDS
        // members (context-pack commits the pack artifact + receipt).
        Commands::BuildPages { .. } | Commands::WritePage { .. } => true,
        // wiki.py _MUTATING_COMMANDS: compile stages+commits; compile-plan
        // stays read-only (excluded, like `status`).
        Commands::Compile { .. }
        | Commands::Build {}
        | Commands::StageCandidates { .. }
        | Commands::ReconcilePrepare { .. }
        | Commands::ReconcileApply { .. }
        | Commands::ContextPack { .. } => true,
        Commands::Init { .. }
        | Commands::Status {}
        | Commands::CheckEvidence { .. }
        | Commands::Verify {}
        | Commands::Subjects { .. }
        | Commands::GraphData { .. }
        | Commands::ExportOkf { .. }
        | Commands::RecordDecision { .. }
        | Commands::PageTargets { .. }
        | Commands::PageShow { .. }
        | Commands::CompilePlan {}
        | Commands::QueryPrepare { .. }
        | Commands::Jev { .. }
        | Commands::GraphExport { .. }
        | Commands::Mcp {}
        | Commands::McpRegister { .. }
        | Commands::VizServe { .. } => false,
    }
}

fn command_name(cmd: &Commands) -> &'static str {
    match cmd {
        Commands::Init { .. } => "init",
        Commands::Ingest { .. } => "ingest",
        Commands::Status {} => "status",
        Commands::Topics { .. } => "topics",
        Commands::CheckEvidence { .. } => "check-evidence",
        Commands::Triage { .. } => "triage",
        Commands::IngestGit { .. } => "ingest-git",
        Commands::IngestRss { .. } => "ingest-rss",
        Commands::Doctor { .. } => "doctor",
        Commands::Dream { .. } => "dream",
        Commands::Subjects { .. } => "subjects",
        Commands::GraphData { .. } => "graph-data",
        Commands::ExportOkf { .. } => "export-okf",
        Commands::GraphExport { .. } => "graph-export",
        Commands::VizServe { .. } => "viz-serve",
        Commands::Mcp {} => "mcp",
        Commands::McpRegister { .. } => "mcp-register",
        Commands::Verify {} => "verify",
        Commands::PageTargets { .. } => "page-targets",
        Commands::WritePage { .. } => "write-page",
        Commands::PageShow { .. } => "page-show",
        Commands::BuildPages { .. } => "build-pages",
        Commands::Compile { .. } => "compile",
        Commands::CompilePlan {} => "compile-plan",
        Commands::Build {} => "build",
        Commands::StageCandidates { .. } => "stage-candidates",
        Commands::ReconcilePrepare { .. } => "reconcile-prepare",
        Commands::ReconcileApply { .. } => "reconcile-apply",
        Commands::ContextPack { .. } => "context-pack",
        Commands::Review { .. } => "review",
        Commands::Decide { .. } => "decide",
        Commands::RecordDecision { .. } => "record-decision",
        Commands::Jev { .. } => "jev",
        Commands::QueryPrepare { .. } => "query-prepare",
    }
}
