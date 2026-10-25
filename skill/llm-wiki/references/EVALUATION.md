# EVALUATION.md — how this skill is tested

## Layers (PRD §54)

Mechanical correctness, semantic correctness, memory correctness, retrieval
usefulness, context efficiency, safety — kept as separate gates, never one score.

## Five-layer testing strategy

| Layer | What | Lives | Command | Pass |
|---|---|---|---|---|
| Unit | every engine behavior, TDD per change | `skill/llm-wiki/scripts/tests/` | `python3 -m unittest discover -s skill/llm-wiki/scripts/tests` | all green |
| Deterministic gates | 10 release gates (PRD §58; Gate 10 = benchmark) + lifecycle suites | `evals/deterministic/` | `python3 evals/deterministic/run_gates.py` | all PASS |
| Smoke | install→resolve→init→ingest→verify→doctor→query→uninstall per install mode | `evals/smoke/` | `python3 evals/smoke/run_smoke.py --mode all` | <10 s/mode, exit 0, explicit `skipped` rows OK |
| Perf/cost | wall-time + pack-token budgets vs baselines (hard fail only >10×) | `evals/perf/` | `python3 evals/perf/check_budgets.py` (`--record` to re-baseline) | within 10× of `baselines.json` |
| E2E runtimes + semantic + matrix | installed skill driven by agent CLIs; scenario assertions; engine×scenario ladder | `evals/e2e/`, `evals/semantic/`, `evals/matrix/` | `python3 evals/e2e/run_e2e.py --runtime all` | scenario checklist; `skipped` when a runtime is absent |

Sequencing: layers 1–2 gate every commit; smoke + perf baselines land right
after green; e2e runs last (it tests the finished installed skill) and is the
pre-release gate.

## Deterministic release gates (PRD §58) — must be 100%

```bash
python3 evals/deterministic/run_gates.py
```

| Gate | Meaning |
|---|---|
| citation_resolution | every emitted citation resolves to a real source version |
| hash_dedup | same source bytes never create duplicate versions |
| correction_propagation | corrections invalidate ALL dependent artifacts |
| stale_commit_detection | concurrent stale commits are rejected |
| injection_immunity | instruction-like content cannot change wiki behavior |
| project_isolation | project A never leaks into project B |
| context_budget | packs never exceed the hard budget |
| transaction_atomicity | failed transactions never partially commit |
| retraction_invalidation | retraction invalidates all descendants |
| benchmark | Gate #10 — `evals/benchmark/run_benchmark.py` `run()` must report `pass` (wiki lane vs baseline + doctor fixtures + verify) |

Gate #10 is already wired: `run_gates.py` imports `run_benchmark.py` in-process
via `importlib` and fails on any scenario failure, unmet doctor warnings, or a
red verify — no separate runbook step needed. For a standalone run with the
comparison table printed: `python3 evals/benchmark/run_benchmark.py`.

Unit suite: `python3 -m unittest discover -s skill/llm-wiki/scripts/tests`.

## Evaluation tiers

- **Tier 1 (CI / every commit):** unit suite + `run_gates.py` (10 gates) +
  determinism. This is the cheap, always-green tier.
- **Tier 2 (change validation):** A/B bench — base `bb7a51f` vs current HEAD —
  plus `evals/perf/check_budgets.py`; run before merging engine changes that
  touch pages, query, or context; results committed.
- **Two-arm semantic (E5):** release-time only — re-run the semantic protocol
  across base vs unified on demand.

## Review-surface acceptance gates

Review UX is a contract, not a nicety — every gate must hold after a compile
run that produces conflicts:

- **Queue visible** — open review items appear in `overview.md` and in
  `graph-data` output (`meta.review` counts; per-claim review flags), not
  only in a JSONL file nobody reads.
- **Both sides shown** — `review show` (and any page/overview rendering of a
  contradiction) presents both claims with their ids, values, and origins.
  Never one side, never an averaged compromise.
- **Deferred ≠ deleted** — `defer` keeps the item discoverable via
  `review list --all`; deferred items must remain visible in the queue
  surfaces (overview/graph flags), or debt silently disappears.
- **Changelog records mutations** — every mutation lands in the operations
  log, which `build-pages` derives into `wiki/changes/index.md` (agent-authored
  `wiki/changes/<slug>.md` pages add the narrative), so "what changed" is
  answerable from the wiki itself.
- **Verify green after each action** — after every `review act`, `write-page`,
  or `build-pages`, `wiki.py verify` reports `"ok": true` before the next
  step. A red verify blocks progression, never ships.

## Semantic scenarios (PRD §57) — run against `evals/golden-corpus`

Copy the corpus to a temp project, run init → ingest(docs/, notes/, config/,
data/) → compile (you are the extractor) → answer:

1. **Direct recall** — "Which database does the order service use?"
2. **Cross-source synthesis** — "Why do we use transactional outbox?"
3. **Correction** — "What is the current attribution window?" (must say 7d)
4. **Historical** — "What was it before September?" (must say 30d, cite memo)
5. **Scope** — "Does staging use the same window?" (30d; scope difference)
6. **Conflict** — "How many retries are configured?" (present both 3 and 5)
7. **Unanswerable** — "What is our 2028 pricing plan?" (must refuse)
8. **Isolation** — corpus A answers never cite corpus B evidence.

Pass criteria per PRD §59: citation correctness 100%; refusal on
unanswerable; correct scope/historical handling. Record results to
`evals/semantic/results.jsonl` (scenario, answer, citations, pass).

## Baseline comparison (PRD §60)

Compare against plain Markdown + one PROJECT.md + text search on: recall,
correction handling, historical accuracy, traceability, context efficiency,
session continuation. If the wiki doesn't beat the baseline on correction
handling and traceability, its complexity is not justified.

## Benchmark layer (deterministic, no LLM)

`python3 evals/benchmarks/bench.py --engine <wiki.py> --label <name>` builds
the planted wiki end-to-end and measures retrieval (precision/recall/MRR vs
gold), context efficiency (pack tokens vs budget, compression), navigation
topology (orphans, dead ends, near-dup pairs), and the two-source rule
(corroborated vs waiting targets). `compare.py` diffs two runs — run it
against the base revision before merging engine changes that touch pages,
query, or context. Page corroboration defaults: `--min-claims 2
--min-sources 2` (the two-source rule: one source is an anecdote, two are
knowledge).

## Evaluation matrix (`evals/matrix/`)

Multi-version experiment runner. Axes: **engines** (base `bb7a51f`, branch
HEADs, any commit via `git archive` extraction) × **scenarios** (golden
corpus, seeded synthetic scale ladder N=100/300/1000, adversarial fault
fixtures: `near_dup_flood`, `orphan_farm`, `walled_garden`, `stale_ttl_farm`)
× **workloads** (build, query, maintenance, repair, lint sweep).

- `generator.synthetic_corpus(n, seed, fault)` — deterministic seeded corpora
  with planted claims manifest and optional fault injection.
- `runner.run(engine_refs, scenarios, experiments)` — `runner.py --extract
  <commit>` archives a commit's engine into /tmp for cross-version runs.
- Experiments: E1 version ladder (per-engine metric set); E2 scale sweep;
  E3 maintenance sim (k cycles of ingest→compile→correction→lint→repair);
  E4 failure drills (lint recall per planted fault, guard block rate,
  repair-plan coverage, false positives); E5 two-arm semantic; E6
  determinism double-run on every experiment (raises on drift).
- Output: `evals/matrix/results/matrix-<date>.json` + HTML view; every
  result embeds `experiment`, `engine_refs`, `date`.
