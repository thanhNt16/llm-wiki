# EVALUATION.md — how this skill is tested

## Layers (PRD §54)

Mechanical correctness, semantic correctness, memory correctness, retrieval
usefulness, context efficiency, safety — kept as separate gates, never one score.

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

Unit suite: `python3 -m unittest discover -s skill/scripts/tests`.

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
