# ptt real-corpus dogfooding — design spec

Date: 2026-10-22
Status: approved (design); pending implementation plan
Project: llm-wiki (skill + `wiki.py` engine)

## Problem

The wiki engine has never run against a real corpus at scale. All evaluation is
synthetic (golden-corpus fixture) or single-run semantic scenarios (4/8 recorded
in `results.jsonl`, Sept 14). The §60 baseline comparison against plain
Markdown + text search is specified but has never been executed. Meanwhile the
external state of the art (Decoding AI workshop, `nvk/llm-wiki`, Karpathy
clones) demonstrates real-corpus runs — ours is untested there, and the
`topic-map-compile` work is unproven against live data.

Goal: dogfood the full pipeline on `/Users/harry/Desktop/cellock/ptt/docs`
(439 files, ~10MB, ~49k lines of Markdown, ~320 screenshots, 3 sql, 2 html) as
a *driven loop* — surface reconciliation gaps, scale issues, and eval needs
through measured evidence rather than guessed improvements.

## Decision

Approach A (approved): instrumented run. Build a permanent project wiki at
`ptt/.llm-wiki` using the `topic-map-compile` branch engine, with every phase
wrapped by a timing/receipt logger. Every anomaly becomes a triaged finding:
`fix-now` (engine commit on topic-map-compile) / `eval-case` (new gate or
scenario) / `wontfix` (logged rationale).

Rejected:

- Approach B (synthetic 1k+ corpus harness first) — front-loads a generator
  whose breaks may never occur in the real corpus; the real corpus itself is
  the test.
- Approach C (plain sequential run) — anecdotes, not data; no per-phase timing,
  no reusable eval machinery.

## Approved behaviors (user decisions)

| Question | Decision |
|---|---|
| Corpus | `/Users/harry/Desktop/cellock/ptt/docs` — 439 files, full scope |
| Wiki location | Permanent `ptt/.llm-wiki/` inside the cellock/ptt monorepo |
| Image scope | All ~320 webp/png via agent-vision lane, not skipped |
| Engine | `topic-map-compile` branch HEAD (topics registry + record-decision) |
| Approach | Instrumented run — wrapper logs phases, no engine measurement code |

## Design

### Run phases

| Phase | Command path | Instrumented metric |
|---|---|---|
| 0. Setup | `wiki.py init --name ptt` at ptt repo root | idempotent re-run, `.state` layout sanity |
| 1. Ingest | `ingest --dir docs` (md/sql/html) + per-image `ingest --file … --parser-name agent-vision --normalized-content <extraction>` | per-source ms, bytes, dedup skips, extraction-report coverage |
| 2. Compile | `compile` umbrella (extract → reconcile-prepare → reconcile-apply), resumable per phase | candidates/source, auto-verdict rate, manual review count, phase wall time |
| 3. Pages | `page-targets` → `write-page` ×N → `build-pages` | pages written, stale marks, `verify` green |
| 4. Query | `query-prepare` per golden question + `context-pack --resume` | pack tokens vs budget, citation resolution, correctness |
| 5. Report | dogfood report + `results.jsonl` append | — |

Images ingested in batches (~25/batch) so one vision-lane failure doesn't stall
the run; the 3-phase compile supports resume.

### Instrumentation — `evals/dogfood/` (new, inside llm-wiki repo)

- `run.py` — thin wrapper per `wiki.py` call: `time.monotonic` wall_ms, exit
  code, stdout/stderr → `runs/<ts>/NNN-<cmd>.log`, one JSONL row per call into
  `run.jsonl` (`{seq, cmd, wall_ms, exit, receipt_path}`). Engine receipts are
  copied verbatim, never re-derived.
- `triager.md` — decision table mapping anomaly classes → `fix-now` /
  `eval-case` / `wontfix`.
- `questions-ptt.txt` — golden questions authored during ingestion while corpus
  knowledge is fresh: real correction/scope/conflict/unanswerable cases found
  in the data + the 8 standard §57 scenarios adapted to ptt.

No `wikicore/` changes for measurement.

### Anomaly → fix flow

Any engine fix required by a `fix-now` finding lands as its own commit on
`topic-map-compile` with a regression test — never edited inside the
instrumented run. A hard failure (compile can't resume, verify stays red) is
itself the finding: reproduce minimal → fix → re-run → document.

## Success criteria

- `ptt/.llm-wiki/` builds end-to-end on all 439 files; `wiki.py verify` →
  `ok:true`; `wiki.py doctor` → no red findings.
- All 9 deterministic gates pass on engine HEAD; `unittest discover` green.
- Golden ptt question set: citation correctness 100%, unanswerable refusal,
  scope/historical handled (same rubric as §57/§59); results appended to
  `evals/semantic/results.jsonl`.
- §60 baseline comparison executed once — same question set against plain
  grep + PROJECT.md on the corpus — recorded in the run report.
- `docs/dogfood/<date>-ptt-run.md` — per-phase timings, triaged findings.

## Out of scope (YAGNI)

Ingest adapters for YouTube/Reddit; question-capture loop (`questions/`,
`notes/`, `open-questions.md`); Obsidian export; synthetic scale-corpus
generator; changes to the nine-skill facade structure.

## Risks

- **Merge collision** — ptt/.llm-wiki is created on `topic-map-compile`; if
  main's uncommitted fix-phase diff (54 files) lands first, paths may drift.
  Mitigation: run against branch worktree checkout, not installed skill.
- **Vision-lane cost** — ~320 images × extraction is token-heavy. Batching
  bounds blast radius; a single image failure logs and continues.
- **Corpus drift** — ptt/docs may change under us mid-run. Mitigation: record
  `git -C ptt rev-parse HEAD` in the run report header.
