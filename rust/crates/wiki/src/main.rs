//! `wiki` — Rust drop-in for the `wiki.py` calls the skills make
//! (issue #15): `init`, `ingest`, `compile`, `query-prepare` (alias
//! `query`). The clap surface mirrors wiki.py's argparse for exactly
//! that subset — same flags, same wiki.py `--wait-lock`/`--no-wait`
//! contract (default 30s, exit 5 on timeout) — and every subcommand
//! dispatches into the `llm-wiki` engine crate, whose store/txn layers
//! are `llm-wiki-core` (the #11 port). Output contract: one JSON object
//! on stdout, success and errors alike; exit codes 0/2/3/4/5.

use std::path::PathBuf;
use std::time::Duration;

use clap::{Parser, Subcommand};
use llm_wiki::cmd::compile::{self, CompileArgs};
use llm_wiki::cmd::ingest::{self, IngestArgs, DEFAULT_MAX_BYTES};
use llm_wiki::cmd::query_prepare::{self, QueryPrepareArgs};
use llm_wiki::emit;
use llm_wiki::err::EngineError;
use llm_wiki::store::{self, Wiki};
use llm_wiki::txn::lock::Lock;

/// wiki.py `--wait-lock` default (seconds before giving up with exit 5).
const DEFAULT_WAIT_LOCK: f64 = 30.0;

#[derive(Parser)]
#[command(name = "wiki", version, about = "llm-wiki deterministic core")]
struct Cli {
    /// project root (default: cwd; also LLM_WIKI_ROOT)
    #[arg(long, global = true)]
    root: Option<PathBuf>,
    /// mutating commands: wait up to SECONDS for the commit lock before
    /// running (default 30; exit 5 on timeout)
    #[arg(long, global = true, value_name = "SECONDS",
        default_value_t = DEFAULT_WAIT_LOCK)]
    wait_lock: f64,
    /// fail immediately when the commit lock is held (wiki.py store_const 0.0)
    #[arg(long, global = true, conflicts_with = "wait_lock")]
    no_wait: bool,
    #[command(subcommand)]
    command: Command,
}

// wiki.py parity keeps `ingest` a flat flag bag, so its variant dwarfs the
// others; boxing Options would obscure the dispatch for no real win.
#[allow(clippy::large_enum_variant)]
#[derive(Subcommand)]
enum Command {
    /// Idempotent, non-destructive scaffold of the .llm-wiki layout.
    Init {
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
        /// batch-ingest a manifest produced by `wiki.py triage`
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
    /// Turn evidence into claims (3-phase umbrella; --resume advances
    /// staged runs one phase).
    Compile {
        /// advance every staged run one phase (wiki.py --resume)
        #[arg(long)]
        resume: bool,
    },
    /// Read-only query preparation: matches + superseded + decisions +
    /// conflicts + gaps for the agent to synthesize an answer (wiki.py
    /// query-prepare; gaps are a domain answer, exit 0 either way).
    #[command(alias = "query")]
    QueryPrepare {
        /// the question to prepare evidence for (required)
        #[arg(long, required = true)]
        question: String,
        /// historical query: answer as of this ISO date/datetime
        #[arg(long = "as-of")]
        as_of: Option<String>,
    },
}

impl Command {
    /// wiki.py `_MUTATING_COMMANDS` ∩ this surface; `ingest
    /// --list-connectors` stays read-only despite the command name.
    fn takes_commit_lock(&self) -> bool {
        match self {
            Command::Init { .. } | Command::Compile { .. } => true,
            Command::Ingest { list_connectors, .. } => !list_connectors,
            Command::QueryPrepare { .. } => false,
        }
    }
}

/// `--root` > `LLM_WIKI_ROOT` > cwd (wiki.py `--root` / spec §6.1).
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

fn run(cli: Cli) -> Result<serde_json::Value, EngineError> {
    let cmd = cli.command;
    let root = resolve_root(cli.root.clone())?;
    if cmd.takes_commit_lock() {
        // Best-effort pre-check only — the commit still takes the lock
        // itself (TOCTOU), skipped on non-wikis so a failed command doesn't
        // create .state/locks/ in a plain directory (wiki.py:1019 parity).
        let wiki = Wiki::open(&root)?;
        if wiki.exists() {
            let secs = if cli.no_wait { 0.0 } else { cli.wait_lock };
            Lock::wait_until_free(&wiki, Some(Duration::from_secs_f64(secs.max(0.0))))?;
        }
    }
    let wiki = Wiki::open(&root)?;
    match cmd {
        Command::Init { name } => {
            let name = name.unwrap_or_else(|| {
                wiki.root
                    .file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_else(|| "root".to_string())
            });
            store::init_wiki(&wiki, &name)
        }
        Command::Ingest {
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
        } => ingest::run(
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
        ),
        Command::Compile { resume } => compile::run(&wiki, &CompileArgs { resume }),
        Command::QueryPrepare { question, as_of } => {
            query_prepare::run(&wiki, &QueryPrepareArgs { question, as_of })
        }
    }
}

fn main() {
    // Keep the sqlite projection fresh inside every commit's critical
    // section (core `Txn` is index-free; the hook seam is spec §10.2).
    llm_wiki::install_commit_hook();
    let cli = match Cli::try_parse() {
        Ok(c) => c,
        Err(e) => {
            // clap Error::use_stderr() == false only for --help/--version,
            // which print to stdout and exit 0. Everything else is a usage
            // failure → JSON on stdout, exit 2.
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
