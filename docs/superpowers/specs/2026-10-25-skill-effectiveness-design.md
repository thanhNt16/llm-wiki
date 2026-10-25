# Skill-effectiveness phase — design spec

Date: 2026-10-25
Status: auto-approved (user pre-approved brainstorm/plan decisions for this phase)
Project: llm-wiki (skill + `wiki.py` engine), branch `skill-effectiveness`
Base commit: `bb7a51f` (checkpoint: skill refinements, 346 tests + 9 gates green)

## Problem

The skillset works mechanically (346 unit tests, 9 deterministic gates) but two
gaps remain, both called out by the 2026-10 landscape research (277 sources,
`llm-wiki-landscape.html` + `llm-wiki-report.html`):

1. **Known field lessons are not implemented.** The engine has claim-level
   health checks (orphan claims, possible duplicates) but nothing at the
   *page-topology* level, where the field's hardest-won lessons live:
   orphan/dead-end pages, near-duplicate page creation ("the default failure
   is near-duplicate pages"), and single-source concept pages.
2. **No proof of improvement.** PRD §60's baseline comparison (wiki vs plain
   markdown + grep) has never been executed on main, and there is no
   before/after benchmark harness that shows an engine change actually moved a
   number. External commentary's verdict on the whole pattern: "evaluation is
   the weakest link — with just vibes at this point."

## Goal

Make the skillset measurably more effective, helpful, and clean — and prove it
with numbers: deterministic before/after engine benchmarks (base `bb7a51f` vs
HEAD) plus an agent-run two-arm semantic comparison (with skill vs without).

## Lessons → changes map

| # | Lesson (source) | Change |
|---|---|---|
| L1 | Orphan pages get no readership; 1 inlink clears, 3 ensures reachability; dead-ends and walled gardens are symmetric failures (Wikipedia orphan guideline; small-world topology) | `doctor` page-topology checks: `orphan_page`, `dead_end_page`, `hub_overload` (info) |
| L2 | "Extend, don't create" — near-duplicate pages are the default failure (HagaiHen podcast wiki; DecodingAI workshop) | `page-targets` emits `similar_existing` for proposed pages above a similarity threshold; `write-page` refuses to create a near-duplicate page without `--allow-new` |
| L3 | "One note mentioning it is a fact about the note; two notes make it a fact about your knowledge" (DecodingAI two-source rule) | **Already implemented** — `page_targets` requires `--min-claims 2 --min-sources 2` (distinct origins, set-union) with an `authoritative` bypass; below-threshold subjects land on the `waiting` list. This phase *measures* it (bench metric: corroborated vs waiting ratio) and documents it in EVALUATION.md instead of rebuilding it |
| L4 | Evaluation is the weakest link; claims of improvement need numbers | `evals/benchmarks/` deterministic harness + before/after comparison |
| L5 | PRD §60: complexity is justified only if the wiki beats plain markdown + grep on correction handling and traceability | Two-arm semantic eval (grep arm vs wiki arm) executed by subagents on the golden corpus |

## Engine changes (all deterministic, no LLM)

### 1. Navigation-topology lint (`wikicore/doctor.py`)

The engine's navigation model is `wiki/overview.md` → `wiki/<section>/index.md`
→ section pages, with pages citing claims via `[[c_id]]` (pages do not link
each other by slug). The lint checks that fabric (deterministic file reads +
regex, mirroring how `build-pages` writes):

- `orphan_page` (warning): a `wiki/<section>/*.md` page absent from its
  section `index.md` — unreachable except by filesystem listing (hand-written
  strays, retraction leftovers). Index pages and `overview.md` are exempt.
- `dead_end_page` (warning): a content page containing neither a `[[claim]]`
  reference nor a markdown link to another wiki artifact — a reader/agent that
  lands there cannot navigate anywhere.
- `section_index_unlinked` (warning): a `wiki/<section>/index.md` that
  `overview.md` does not link (broken progressive-disclosure ladder).

Severity: warning (not error) — topology debt must be visible in `doctor` and
`verify` JSON but must not fail `verify` (existing behavior: only `error`
severity fails). This keeps the gate additive.

### 2. Extend-don't-create guard (`pages.py`)

- `page-targets`: for each proposed target, compute title+aliases similarity
  against existing pages (normalized token Jaccard ≥ 0.6 OR normalized title
  equality). Emit `similar_existing: [{slug, title, similarity}]` on the
  target and mark routing `extend-first`.
- `write-page`: when creating a *new* page whose title is near-duplicate of an
  existing one (same threshold), exit code 3 (validation) with a message
  naming the twin and suggesting `--extend <slug>` instead; `--allow-new`
  overrides (records `forced_new: true` in page metadata).

### 3. Two-source rule — measurement only (already in the engine)

Default `--min-claims 2 --min-sources 2` corroborates pages; the bench
harness records corroborated/waiting/authoritative ratios as metrics, and
EVALUATION.md documents the rule as a first-class skill behavior (it is the
field's two-source rule). No engine change.

### 4. Skill text wiring

- `SKILL.md` + `references/COMMANDS.md`: document the guard behavior
  (`--allow-new`, extend-first routing) and the new doctor codes.
- `references/EVALUATION.md`: add the benchmark harness as a third evaluation
  layer (after gates and semantic scenarios).
- `skill/wiki-write/PAGES.md` (if it carries page guidance): extend-first
  authoring rule with the two-source and orphan lessons inline.

## Benchmark harness (`evals/benchmarks/`, new)

Fully deterministic — no LLM anywhere. Reuses the run_gates.py technique of
staging planted claims directly.

- `bench.py --engine <wiki.py> --label <name> [--keep]`:
  1. Copy `evals/golden-corpus` to a temp project.
  2. Drive the engine CLI end-to-end: `init` → `ingest` (docs/, notes/,
     config/, data/) → `stage-candidates` (planted claim set — the same
     correction/conflict/scope fixtures the gates use, scripted once in
     `claims_fixture.py`) → `reconcile-apply` (auto-verdicts) →
     `build-pages` → `verify`.
  3. Measure (from CLI JSON output + reading the built wiki dir — harness
     code, never engine internals, so the same harness measures both engines):
     - **retrieval**: for each entry in `questions-gold.jsonl` (question →
       gold claim/source ids, authored from the planted fixtures): is the gold
       claim in `query-prepare` top-5? precision@5, gold-recall@5,
       MRR.
     - **context efficiency**: `context-pack --task <gold task> --budget 6000`
       → tokens used vs budget; corpus compression ratio (pack tokens vs
       full-corpus tokens).
     - **topology**: page count, orphan pages, dead-end pages,
       section-index linkage, near-duplicate page pairs (Jaccard ≥ 0.6 on
       normalized subject tokens), corroborated/waiting page-target ratios
       (two-source rule metrics).
     - **hygiene**: doctor finding counts by code/severity.
  4. Emit `bench-<label>.json`.
- `compare.py before.json after.json`: side-by-side diff table (markdown +
  JSON) with per-metric delta and direction annotation (↑ better / ↓ better /
  =).
- Planted fixture includes one deliberate near-duplicate page attempt (guard
  target), one single-origin subject (lands on the waiting list — two-source
  metric target), and one hand-written stray page (orphan-lint target), so the
  new checks and metrics have something true to catch — this is what makes
  the before/after delta nonzero and honest.

### Fair before/after protocol

Same harness code, same corpus, same fixtures. "Before" = engine checked out
from base commit `bb7a51f` into a temp dir (`git worktree add --detach` or
`git archive`), "after" = HEAD. Both run on this machine back-to-back.

## Two-arm semantic eval (PRD §60, agent-run)

Protocol (subagents, golden corpus, 8 standard scenarios from
`references/EVALUATION.md`):

- **Arm A (without skill)**: subagent gets the plain golden corpus copy +
  `rg`/`grep`, answers all 8 questions with citations (file:line), effort
  notes (searches run).
- **Arm B (with skill)**: the deterministic wiki built by `bench.py` (planted
  claims); subagent gets corpus + `.llm-wiki/` and the query/context-pack
  commands, answers the same 8 with claim citations.
- Scoring (mechanical, done by orchestrator): citation correctness (100%
  required), correction/historical/scope/conflict handled, unanswerable
  refusal, effort (tool calls, tokens estimated). Results appended to
  `evals/semantic/results.jsonl` with `arm` field.

## Testing

- Unit tests per engine feature: topology findings on a crafted wiki (orphan,
  dead-end, unlinked index, exemptions), guard refusal + `--allow-new`
  override, `similar_existing` emission.
- Existing suites must stay green (346 tests, 9 gates).
- `bench.py` gets a smoke test (runs end-to-end on the corpus, emits valid
  JSON).
- Harness determinism check: run bench twice on the same engine → identical
  metrics.

## Success criteria

1. All tests + gates green at HEAD.
2. `compare.py base→head` shows: near-duplicate page pairs ↓ (guard), orphan/
   dead-end pages surfaced (new lint — count goes 0→N *detected*, i.e. the
   lint works), no regression in retrieval/context metrics.
3. Two-arm semantic results recorded; wiki arm wins correction + traceability
   (PRD §60's justification), or the failure is documented honestly.
4. Docs updated (SKILL.md, COMMANDS.md, EVALUATION.md).
5. Final interactive HTML recap report delivered (separate deliverable).

## Out of scope (YAGNI)

Topics registry (lives on `topic-map-compile` branch), FTS5/BM25 derived
index, claim sidecars, multi-agent merge semantics, ptt real-corpus re-run,
any LLM-dependent benchmarking.

## Risks

- **Link extraction mismatch**: wiki pages use a specific link syntax; if
  `pages.py` already normalizes links, reuse it — otherwise the lint reads
  built pages the same way `build-pages` writes them (round-trip safe).
- **Guard false positives** blocking legitimate new pages: threshold 0.6 on
  normalized tokens + `--allow-new` escape hatch keeps it advisory-in-practice.
- **Semantic eval variance**: subagent answers vary; scoring stays mechanical
  (citation resolves, dates match, refusal present) to keep it fair.
