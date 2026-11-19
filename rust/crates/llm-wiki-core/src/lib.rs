//! `llm-wiki-core` — evidence-store core, ported from
//! `skill/llm-wiki/scripts/wikicore/store.py` ([`store`]),
//! `transaction.py` ([`txn`]), `ids.py` ([`ids`]), `dirs.py` ([`dirs`])
//! and `deps.py` ([`deps`]).
//!
//! - [`store`]: project-root / `.llm-wiki` layout, `wiki.json` sentinel,
//!   config + manifest io, JSONL readback, idempotent `init_wiki`.
//! - [`txn`]: PLAN → STAGE → VALIDATE → COMMIT under the `commit.lock`
//!   flock with optimistic `base_revision` checking, journal/backup
//!   breadcrumbs and crash rollback.
//! - [`ids`]: ULID-style identifiers (48-bit ms timestamp + 80-bit
//!   randomness, Crockford base32) — `ids.new` / `ids.validate`.
//! - [`dirs`]: recursive directory ingest — deterministic sorted walk,
//!   contractual skip classification, per-file ingest seam, batch receipt
//!   under `.state/ingest-batches/`.
//! - [`deps`]: dependency/invalidation graph over
//!   `.state/dependencies.json` — edge registration, transitive stale
//!   marking (`<id>@<version>` deps match any version of a changed id).
//! - [`err`]: error taxonomy → CLI exit codes.
//!
//! The derived-index hook ([`txn::set_commit_hook`]) lets an embedding
//! binary (the `llm-wiki` CLI) keep its sqlite projection fresh inside the
//! commit critical section; the core itself has no index dependency.

pub mod deps;
pub mod dirs;
pub mod err;
pub mod ids;
pub mod store;
pub mod txn;
pub mod util;
