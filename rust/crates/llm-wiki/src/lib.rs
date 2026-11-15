//! `llm-wiki` — deterministic evidence-wiki engine (Phase 1 of the Rust
//! port; spec `docs/superpowers/specs/2026-11-14-rust-engine-design.md`).
//!
//! Single-binary replacement for the Python engine
//! (`skill/llm-wiki/scripts/wiki.py` + `wikicore/*`). The contract is
//! behavior-free: command names, stdout JSON key sets, exit codes, and on-disk
//! file layouts are contractual; values may differ where improved (spec §1).
//!
//! # Core invariants
//!
//! - **Canonical = files.** `sources/`, `claims/`, `decisions/`,
//!   `topics.json`, and `.state/*.json` are the source of truth.
//!   `.state/index.sqlite` is a derived projection — disposable, rebuilt on
//!   schema/corruption mismatch, never authoritative (spec §10.1).
//! - **Mutations = transactions.** Every write goes through
//!   [`txn::Txn`] — stage bytes under `.state/staging/<run>/`, then under
//!   the `commit.lock` flock: repair gate → `base_revision` check →
//!   expect-stamp re-check → journal → backup → apply → index hook →
//!   receipt/operations/manifest bump → breadcrumb clear (spec §5.1).
//! - **Optimistic concurrency.** Reads register an expect-stamp
//!   `(ino, mtime_ns, ctime_ns, size)`; any mid-txn external change
//!   (rewrite, delete, create-over-expect-absent) fails the commit with
//!   `Conflict` (exit 4). Leftover journals/backups from a crashed attempt
//!   block new commits with `Repair` (exit 3) (spec §5.2/§5.4).
//! - **JSON on stdout, always.** Success payloads and errors alike are a
//!   single JSON object on stdout — see [`emit`]. Exit codes live in
//!   [`err::EngineError`]: 0 ok, 2 usage, 3 txn/needs-agent/`ok:false`,
//!   4 conflict, 5 locked (spec §6.2).
//! - **Symlink hostility.** Write paths are lstat-walked per component
//!   below the project root; symlinks and `..` escapes are rejected before
//!   any bytes move (spec §5.2).
//!
//! # Phase-1 surface
//!
//! Implemented commands: `init`, `ingest` (`--file/--text/--url` lanes +
//! `--normalized-content`, `--dir`, `--triage`, `--connector/--ref`,
//! `--list-connectors`), `ingest-git`, `ingest-rss`, `status`,
//! `topics` (+`--backfill`), `check-evidence`, `verify`, and the global
//! `--wait-lock`/`--no-wait` contract. Deliberately deferred to P2+
//! (explicit `Txn` "not yet implemented" errors, never silent no-ops):
//! the compile/query/`mcp` pipeline (spec §13).

pub mod frontmatter;
pub mod graphdata;
pub mod okf;
pub mod subjects;
pub mod vizexport;
pub mod vizserve;

pub mod claims;
pub mod cmd;
pub mod compile;
pub mod contextpack;
pub mod decisions;
pub mod deps;
pub mod doctor;
pub mod dream;
pub mod emit;
pub mod err;
pub mod formats;
pub mod evidence;
pub mod ids;
pub mod index;
pub mod mcp;
pub mod mcp_register;
pub mod jev;
pub mod pages;
pub mod query;
pub mod review;
pub mod secrets;
pub mod schema;
pub mod shorthand;
pub mod store;
pub mod synthesize;
pub mod skills_embed;
pub mod txn;
pub mod topics;
pub mod util;
