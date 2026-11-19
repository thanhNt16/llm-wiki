//! `llm-wiki-core` — evidence-store core, ported from
//! `skill/llm-wiki/scripts/wikicore/store.py` ([`store`]) and
//! `transaction.py` ([`txn`]).
//!
//! - [`store`]: project-root / `.llm-wiki` layout, `wiki.json` sentinel,
//!   config + manifest io, JSONL readback, idempotent `init_wiki`.
//! - [`txn`]: PLAN → STAGE → VALIDATE → COMMIT under the `commit.lock`
//!   flock with optimistic `base_revision` checking, journal/backup
//!   breadcrumbs and crash rollback.
//! - [`err`]: error taxonomy → CLI exit codes.
//!
//! The derived-index hook ([`txn::set_commit_hook`]) lets an embedding
//! binary (the `llm-wiki` CLI) keep its sqlite projection fresh inside the
//! commit critical section; the core itself has no index dependency.

pub mod err;
pub mod store;
pub mod txn;
pub mod util;
