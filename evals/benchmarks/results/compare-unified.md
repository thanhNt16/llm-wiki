## Run metadata

- **Date:** 2026-10-25
- **Base commit:** `bb7a51f` (pre-change; extracted via `git archive bb7a51f skill/llm-wiki` to /tmp)
- **Head commit:** `12e580e9daa151068af49d661b4cd77be7439ae4` (unified; engine: `skill/llm-wiki/scripts/wiki.py` in this worktree)
- **Harness:** `evals/benchmarks/bench.py` + `compare.py` at head; deterministic planted fixture, same golden corpus for both runs (`bench-base2.json` / `bench-unified.json` under `evals/matrix/results/`).
- **Takeaway:** the extend-dont-create guard blocked the near-duplicate write (exit 0->3, `near_dup_pairs` 1->0) and the tier-1/2 lint surfaced the planted stray page (`orphan_page`+`unlisted_page`+`dead_end_page` 0->2 each), while retrieval stayed identical (recall 1.0, MRR 0.7) and context packing improved (compression 6.52->6.81, avg pack tokens 275.2->263.8).

| metric | before | after | delta | verdict |
|---|---|---|---|---|
| `build_steps` | [{'err': '', 'exit': 0, 'step': 'init'}, {'err': '', 'exit': 0, 'step': 'ingest docs/adr-007-retries.md'}, {'err': '', 'exit': 0, 'step': 'ingest docs/adr-019-attribution.md'}, {'err': '', 'exit': 0, 'step': 'ingest notes/memo-2026-09-01-window-change.md'}, {'err': '', 'exit': 0, 'step': 'ingest config/analytics.prod.json'}, {'err': '', 'exit': 0, 'step': 'ingest config/analytics.sandbox.json'}, {'err': '', 'exit': 0, 'step': 'ingest notes/meeting-2026-09-10.md'}, {'err': '', 'exit': 0, 'step': 'ingest docs/adr-001-postgres.md'}, {'err': '', 'exit': 0, 'step': 'stage docs/adr-019-attribution.md'}, {'err': '', 'exit': 0, 'step': 'reconcile docs/adr-019-attribution.md'}, {'err': '', 'exit': 0, 'step': 'stage config/analytics.prod.json'}, {'err': '', 'exit': 0, 'step': 'reconcile config/analytics.prod.json'}, {'err': '', 'exit': 0, 'step': 'stage config/analytics.sandbox.json'}, {'err': '', 'exit': 0, 'step': 'reconcile config/analytics.sandbox.json'}, {'err': '', 'exit': 0, 'step': 'stage docs/adr-007-retries.md'}, {'err': '', 'exit': 0, 'step': 'reconcile docs/adr-007-retries.md'}, {'err': '', 'exit': 0, 'step': 'stage notes/meeting-2026-09-10.md'}, {'err': '', 'exit': 0, 'step': 'reconcile notes/meeting-2026-09-10.md'}, {'err': '', 'exit': 0, 'step': 'stage docs/adr-001-postgres.md'}, {'err': '', 'exit': 0, 'step': 'reconcile docs/adr-001-postgres.md'}, {'err': '', 'exit': 0, 'step': 'stage notes/memo-2026-09-01-window-change.md correction'}, {'err': '', 'exit': 0, 'step': 'reconcile notes/memo-2026-09-01-window-change.md correction'}, {'err': '', 'exit': 0, 'step': 'build-pages'}, {'err': '', 'exit': 0, 'step': 'verify'}] | [{'err': '', 'exit': 0, 'step': 'init'}, {'err': '', 'exit': 0, 'step': 'ingest docs/adr-007-retries.md'}, {'err': '', 'exit': 0, 'step': 'ingest docs/adr-019-attribution.md'}, {'err': '', 'exit': 0, 'step': 'ingest notes/memo-2026-09-01-window-change.md'}, {'err': '', 'exit': 0, 'step': 'ingest config/analytics.prod.json'}, {'err': '', 'exit': 0, 'step': 'ingest config/analytics.sandbox.json'}, {'err': '', 'exit': 0, 'step': 'ingest notes/meeting-2026-09-10.md'}, {'err': '', 'exit': 0, 'step': 'ingest docs/adr-001-postgres.md'}, {'err': '', 'exit': 0, 'step': 'stage docs/adr-019-attribution.md'}, {'err': '', 'exit': 0, 'step': 'reconcile docs/adr-019-attribution.md'}, {'err': '', 'exit': 0, 'step': 'stage config/analytics.prod.json'}, {'err': '', 'exit': 0, 'step': 'reconcile config/analytics.prod.json'}, {'err': '', 'exit': 0, 'step': 'stage config/analytics.sandbox.json'}, {'err': '', 'exit': 0, 'step': 'reconcile config/analytics.sandbox.json'}, {'err': '', 'exit': 0, 'step': 'stage docs/adr-007-retries.md'}, {'err': '', 'exit': 0, 'step': 'reconcile docs/adr-007-retries.md'}, {'err': '', 'exit': 0, 'step': 'stage notes/meeting-2026-09-10.md'}, {'err': '', 'exit': 0, 'step': 'reconcile notes/meeting-2026-09-10.md'}, {'err': '', 'exit': 0, 'step': 'stage docs/adr-001-postgres.md'}, {'err': '', 'exit': 0, 'step': 'reconcile docs/adr-001-postgres.md'}, {'err': '', 'exit': 0, 'step': 'stage notes/memo-2026-09-01-window-change.md correction'}, {'err': '', 'exit': 0, 'step': 'reconcile notes/memo-2026-09-01-window-change.md correction'}, {'err': '', 'exit': 0, 'step': 'build-pages'}, {'err': '', 'exit': 0, 'step': 'verify'}] | = | unchanged/new |
| `context.budget_compliance` | True | True | = | unchanged/new |
| `context.compression_ratio` | 6.52 | 6.81 | ↑ | better |
| `context.pack_tokens_avg` | 275.2 | 263.8 | ↓ | better |
| `engine` | /var/folders/rw/lz_rc3xs33q9rb8c3jp7whn80000gn/T/llmwiki-engine-bb7a51f-krknfmhr/skill/llm-wiki/scripts/wiki.py | /Users/harry/Desktop/personal/llm-wiki/.worktrees/skill-effectiveness/skill/llm-wiki/scripts/wiki.py | = | informational |
| `guard.near_dup_blocked` | False | True | = | informational |
| `guard.near_dup_write_exit` | 0 | 3 | = | informational |
| `guard.seed_write_exit` | 0 | 0 | = | unchanged/new |
| `guard.stray_orphan_detected_by_doctor` | False | True | = | informational |
| `label` | base2 | unified | = | informational |
| `retrieval.gold_recall_at_5` | 1.0 | 1.0 | = | unchanged/new |
| `retrieval.mrr` | 0.7 | 0.7 | = | unchanged/new |
| `retrieval.precision_at_5` | 0.24 | 0.24 | = | unchanged/new |
| `retrieval.refusals_correct` | True | True | = | unchanged/new |
| `topology.dead_end_pages` | 1 | 1 | = | unchanged/new |
| `topology.near_dup_pairs` | 1 | 0 | ↓ | better |
| `topology.orphan_pages` | 3 | 2 | ↓ | better |
| `topology.pages` | 3 | 2 | = | informational |
| `topology.section_indexes_linked` | 1 | 1 | = | unchanged/new |
| `twosource.corroborated` | 2 | 2 | = | unchanged/new |
| `twosource.waiting` | 2 | 2 | = | unchanged/new |

| doctor code | before | after |
|---|---|---|
| `dead_end_page` | 0 | 2 |
| `missing_effective_date` | 8 | 8 |
| `missing_scope` | 8 | 8 |
| `orphan_page` | 0 | 2 |
| `possible_duplicate` | 1 | 1 |
| `staging_leftover` | 7 | 7 |
| `unlisted_page` | 0 | 2 |
| `unregistered_page` | 1 | 1 |
