# llm-wiki (Rust engine)

Single-binary deterministic core for the `llm-wiki` skill — a Rust port of the
Python engine (`skill/llm-wiki/scripts/wiki.py` + `wikicore/*`). Canonical
state stays plain files under `.llm-wiki/`; `.state/index.sqlite` is a
disposable derived projection. Every mutation goes through a write-ahead
transaction (stage → flock → repair gate → revision/stamp check → journal →
backup → apply → receipt → clear breadcrumbs).

The contract with the Python engine is **behavior-free**: command names,
stdout JSON key sets, exit codes, and on-disk layouts are contractual; values
may differ where improved. All output — including every error — is a single
JSON object on **stdout**.

Spec: `docs/superpowers/specs/2026-11-14-rust-engine-design.md`
Plan: `docs/superpowers/plans/2026-11-14-rust-engine.md`

## Build

```sh
cargo build --release -p llm-wiki    # from rust/
# binary: rust/target/release/llm-wiki
```

## Test

```sh
cargo test          # from rust/ (or rust/crates/llm-wiki)
cargo doc --no-deps # rustdoc; must build warning-free
```

Suites live in `rust/crates/llm-wiki/tests/` — one file per lane
(`cli_*` end-to-end via `CARGO_BIN_EXE`, `txn_*` library-level,
`edge_cases` hardening, `dir_cursors`, `index_contract`, `deps_edges`,
`claims_index`); the `wiki` front-end's surface tests live in
`rust/crates/wiki/tests/`.

## `wiki` — wiki.py-shaped front-end (issue #15)

`cargo build -p wiki` also produces `wiki` (`rust/target/release/wiki`): a
drop-in for the `wiki.py` calls the skills make — `init`, `ingest`,
`compile`, `query-prepare` (alias `query`) — dispatching into the same
engine. Flags mirror wiki.py's argparse for that subset, including the
wiki.py lock contract: global `--root`, `--wait-lock SECONDS`
(**default 30**, exit 5 on timeout — unlike `llm-wiki`'s block-forever
default), `--no-wait`; `init`/`ingest` (unless `--list-connectors`)/`compile`
are the mutating set that waits. Output contract is identical: one JSON
object on stdout (errors included), exit codes 0/2/3/4/5.


## Commands (Phase 1)

| Command | Notes |
|---|---|
| `llm-wiki init [PATH] [--name N]` | Idempotent, non-destructive `.llm-wiki` scaffold. `wiki.json` is the sentinel and is written last. Scaffolding is serialized under `commit.lock`, so concurrent inits (or init racing a commit) can't corrupt each other. |
| `llm-wiki ingest --file P` | File → versioned source: raw bytes preserved under `raw/files/`, manifest under `sources/<id>/`. UTF-8-invalid content takes the binary lane (no normalized text) — it is **not** an error. |
| `llm-wiki ingest --text T [--text-ref R] [--kind session]` | Inline text ingest. |
| `llm-wiki ingest --url U` | http/https fetch, response capped at `--max-bytes` (default 50 MiB). `--file`/`--text` are *not* capped (parity with `_read_bytes`). |
| `llm-wiki ingest --normalized-content F --parser-name P` | Attach agent-produced extraction markdown. Per-file only. |
| `llm-wiki check-evidence [--verify] [--details N] [--json]` | Read-only evidence-lane audit: manifest-pinned paths vs disk, orphans, `issues[]` bounded by `--details`; `--verify` re-hashes raw payloads. The audit itself completes and exits 0 — `needs_repair`/`ok` is data in the payload. |
| `llm-wiki topics [--backfill]` | Topic registry listing; `--backfill` rebuilds `topics.json` from the claims scan (takes the commit lock). |
| `llm-wiki verify` | Repo-wide consistency: index watermark vs manifest revision, dep edges resolvable, claims parse. `ok:false` → exit 3 (a successful domain result, not an error payload). |

Global flags: `--root` > `LLM_WIKI_ROOT` > cwd (CLI is cwd-only — ancestor
walk is MCP-only, spec §6.1). `--wait-lock[=SECS]` / `--no-wait` are accepted
only on commands that run a commit transaction (`ingest` lanes,
`topics --backfill`); passing them to read-only commands or `init` is a usage
error. `--wait-lock` is a best-effort pre-check: it rides out a known
in-flight commit, then the real commit still `try_lock`s (TOCTOU).

A root that exists but is not a directory → usage error (exit 2).
`ingest --file <directory>` → usage error (exit 2). Other runtime failures —
unreadable files, corrupt `.state/manifest.json`, permission errors — are
`txn` errors (exit 3), never panics.

## Exit codes (spec §6.2)

| Code | Meaning | Emitted by |
|---|---|---|
| 0 | ok | success; also `{"initialized": false}` and `check-evidence` reports |
| 2 | usage | clap parse failures (all levels), `--file <dir>`, non-directory `--root`, `--wait-lock` on a non-commit command |
| 3 | txn | `TxnError` — I/O/state rejections, `repair_required` (leftover journal/backup), needs-agent statuses, `verify ok:false` |
| 4 | conflict | stale `base_revision`, expect-stamp mismatch (file rewritten/deleted/appeared mid-commit) |
| 5 | locked | `commit.lock` held and `--no-wait`/no flag; or `--wait-lock` deadline expired |

Error payload shape: `{"error": "...", "exit_code": N, "error_type": "usage|txn|repair_required|conflict|locked"}`.

## Phase-1 scope vs Python parity

Implemented per plan tasks 1–14. Deliberately deferred (spec §13 phases 2+);
flagged lanes return explicit `{"error_type": "txn"}` "not yet implemented"
errors — never silent no-ops:

| Area | Phase 1 | Deferred to |
|---|---|---|
| `ingest --dir` (directory walk/triage) | not implemented | P2 |
| `ingest --triage` (manifest batch) | not implemented | P2 |
| `ingest --list-connectors`, connectors | not implemented | P2 |
| `ingest-git`, `ingest-rss` subcommands | not implemented | P2 |
| `subjects`, `graph-data`, `export-okf`, `graph-export` | not implemented | P2 |
| `compile`, `compile-plan`, `stage-candidates`, `reconcile-*`, `build`, `build-pages`, `page-*`, `write-page`, `decide`, `review`, `record-decision` | not implemented | P3 |
| `doctor` (incl. `--fix`, repair of `repair_required`) | not implemented | P3 |
| `dream`, `query-prepare`, `context-pack`, `jev` | not implemented | P3 |
| `mcp` / `mcp-register` server | not implemented | P2 |
| `secrets` policy enforcement | scan-warnings recorded in receipts only | P2 |
| topics declared during extraction | `--backfill` rebuild exists; live declaration | P3 (compile) |
| `--install-skills` | not implemented | P2+ |

Also: `--include-hidden` parses but has no effect on the Phase-1 lanes (it
governs the P2 `--dir` walk); `takes_commit_lock` whitelists only the
implemented commit commands, so `init` rejects `--wait-lock` even though
Python's `_MUTATING_COMMANDS` lists it.
