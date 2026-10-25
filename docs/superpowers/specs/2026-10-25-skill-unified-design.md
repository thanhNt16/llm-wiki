# Skill-unified — design spec

Date: 2026-10-25
Status: approved in dialogue (all sections; 5 user decisions + 2 deviation sign-offs)
Project: llm-wiki (skill + `wiki.py` engine), worktree `.worktrees/skill-effectiveness`,
branch `skill-effectiveness`
Inputs: pasted "Consolidated Synthesis & Unified Implementation Blueprint" (3 copies, identical),
branch comparison of `skill-effectiveness` (57e2f23..19b4127) vs `skill-v2` (8c76489..f42e7bc),
both forked from `bb7a51f`.
Companion review report: `skill-unified-design-review.html` (repo root, generated).

## Goal

Merge `skill-v2` into `skill-effectiveness` and reconcile the 7 overlapping files per the
blueprint; fill all four "missing holes" (lint→repair, KI-1 resume staleness, O(N²) lint
scaling, ingest token triage); wire docs; and extend evaluation with a five-layer testing
strategy plus a multi-version scenario/benchmark matrix — all proven by measurement.

## Locked decisions (user)

| # | Decision |
|---|---|
| 1 | Unify here: merge skill-v2 INTO skill-effectiveness in this worktree (overrides blueprint runbook step 1) |
| 2 | All four holes in scope |
| 3 | Ingest triage = deterministic triage manifest (no LLM in the triage pass) |
| 4 | Merge then tasks: additive merge commit (possibly red), reconciliation as TDD tasks |
| 5 | Add evaluation-matrix section (Section 7) with multi-version experiments |
| D1 | `--fix-indexes` regenerates indexes through the build-pages code path — never appends (real indexes are markdown tables) |
| D2 | KI-1 fingerprint = content hash of `raw/` + `claims/` + sources manifest; excludes derived `wiki/`; content hashes not mtimes |

Blueprint semantic decisions adopted verbatim: dual near-dup tiers (write gate Jaccard ≥ 0.60
or exact-title; doctor `near_duplicate_subject` ≥ 0.70 unlinked-only), `[[wiki:kind/slug]]`
as the cross-page primitive across write/verify/doctor/graph-data, all 7 lint checks with the
rename (eff's index-membership `orphan_page` → `unlisted_page`; v2's graph-reachability keeps
`orphan_page`), `dead_end_page` = v2 semantics (zero outbound `[[wiki:]]` links + distinct
hint when claim links present), union CLI flags, tests union, results.jsonl union under
skill-effectiveness's schema validator, both eval tiers documented in EVALUATION.md.

## 1 · Merge mechanics

- `git merge skill-v2` on `skill-effectiveness`. Conflict policy: both-sides where additive
  (flags, checks, tests); semantic collisions (orphan/dead-end naming) take one side with a
  `TODO-UNIFY` marker that the next task replaces with tested code.
- `evals/semantic/results.jsonl`: union both row sets in the merge commit (data only).
- The merge commit may leave suites red; the task sequence drives green before anything else.
- Full histories preserved (true merge, no rebase/cherry-pick).

## 2 · Engine reconciliation (7 files)

**`wikicore/synthesize.py`** — keep skill-effectiveness's `similar_pages(wiki, kind, subject,
title, exclude_slug=None)` signature (benchmarked/tested); fold in v2's normalized-title
fast-path. `write_page` gains v2's page-link validation: `PAGE_LINK_RE`, self-links rejected,
every `[[wiki:kind/slug]]` target must exist and be declared in deps (dep token
`wiki:<kind>/<slug>`); keeps `allow_new` gate; on `allow_new` override persists
`forced_new: true`. Refusal message names up to 3 twins with similarity + the copy-pasteable
remediation ("extend <ref>" or "--allow-new"). Thresholds as module constants:
`NEAR_DUP_WRITE_THRESHOLD = 0.60`, `NEAR_DUP_LINT_THRESHOLD = 0.70`.

**`wikicore/doctor.py`** — two tiers:
- `_index_topology_checks`: `unlisted_page` (page absent from its section index), 
  `section_index_unlinked` (section index absent from overview.md).
- `_page_graph_checks` (over body `[[wiki:]]` links + page-dep edges): `orphan_page`
  (zero inbound edges, only when >1 valid page), `dead_end_page` (zero outbound links,
  hint distinguishes claim-citing pages), `walled_garden` (undirected component ≥ 3 pages,
  largest component excluded), `near_duplicate_subject` (≥ 0.70, pair unlinked in either
  direction), `stale_page_ttl` (frontmatter `stale_after` < today).
- Malformed frontmatter → `error`-severity finding, never a crash (v2 robustness);
  deterministic sorted iteration. All topology/TTL/near-dup findings severity `warn`
  (never flip doctor `ok` or verify).

**`scripts/wiki.py`** — union CLI: `write-page --allow-new` (exists), `ingest --kind` with
v2's scope-guarded choices (post-merge vocabulary), `doctor --scope <substring filter>`,
`doctor --fix-indexes`; NEW `triage` + `ingest --triage` (Section 3.4).

**Docs** — SKILL.md stays a thin router under the 600-word body gate (gains one triage line;
extend-first red flag stays). v2's 6-lesson operating rules land per-command in
`references/COMMANDS.md`; write-path detail in `skill/wiki-write`. EVALUATION.md documents
both eval tiers + the matrix runner + the testing layers. Tests: union with renames
(index-membership assertions → `unlisted_page`; dead-end assertions flip to v2 semantics).

## 3 · The four holes

### 3.1 Lint → repair
Extract index-writing from `build_pages` into reusable `rebuild_section_indexes(wiki)`;
`doctor --fix-indexes` re-runs it under staged-transaction discipline, repairing exactly
`unlisted_page` + `section_index_unlinked` (idempotent: run twice → byte-identical).
Graph findings cannot be auto-repaired; doctor emits a `repair_plan`: per-finding agent
actions ("add `[[wiki:<ref>]]` to X's See-also + deps"). Engine never writes prose.

### 3.2 KI-1 — compile resume staleness
`compute_corpus_fingerprint(wiki)` = SHA-256 (via existing `hashing` module) over sorted
content of `raw/` + `claims/` + sources manifest; stored in compile run state at plan time;
on `compile --resume` mismatch → staging invalidated, targets recomputed, agent told why
("corpus changed since plan — recomputing"). No mtimes; derived `wiki/` excluded.

### 3.3 O(N²) → inverted token index
One shared inverted index (token → pages) + normalized-title map in `synthesize`, used by
`similar_pages()` and doctor's near-dup scan; only token-sharing pairs compared
(lossless: Jaccard > 0 ⇒ shared token). Test: seeded random fixture, indexed ≡ naive
all-pairs; plus N=300 synthetic-page smoke.

### 3.4 Ingest triage manifest
`wiki.py triage --dir <path> [--out manifest.json]`: deterministic scan (extension, path
patterns, size, frontmatter sniff) → proposals `{file, kind, priority, rationale}`; agent
reviews/edits the single manifest; `wiki.py ingest --triage <manifest>` batch-ingests with
kinds applied (v2's post-merge `--kind` vocabulary). No LLM in the triage pass. Two-source
corroboration still gates promotion at compile.

## 4 · Documentation

Per Section 2 docs paragraph; COMMANDS.md carries v2's 6 lessons (classify-first, extend-first,
two-pass compile, dual-linking See-also, navigation ladder, human-only `verified`) merged with
skill-effectiveness's engine-path resolution and enforcement flags.

## 5 · Evaluation after unification (tiers)

- Tier 1 (CI): unit suite + 10 gates (v2's Gate #10 arrives via merge) + determinism check.
- Tier 2 (change validation): A/B bench base `bb7a51f` vs unified HEAD — cumulative delta now
  includes v2 features; results committed.
- Two-arm semantic (E5): release-time only, re-run across base vs unified on demand.

## 6 · Five-layer testing strategy

| Layer | Scope | Lives | Runs | Pass |
|---|---|---|---|---|
| Unit | every engine behavior, TDD per task | `scripts/tests/` | every commit | all green |
| Integration | 10 gates + lifecycle suites (guard→allow-new→forced_new→lint-detects; fix-indexes→verify green; triage→ingest→kinds; KI-1 mutate→invalidate→converge) | `evals/deterministic/` + tests | every commit | gates PASS |
| E2E runtimes | installed skill driven by omp (`omp -p --no-session`), Claude Code (`claude -p`), CellockAI; 6 scripted scenarios S1 init→compile→cited answer, S2 near-dup→extend, S3 triage, S4 correction→flip-with-history, S5 refusal, S6 query ladder; artifact assertions only; transcripts feed cost metrics; absent runtime = explicit skip | `evals/e2e/` (new) | pre-release + on-demand | scenario checklist |
| Smoke | per install mode: install→resolve→init→ingest fixture→verify→doctor→query→clean uninstall | `evals/smoke/run_smoke.py` (new) | post-install, CI, pre-tag | < 10 s, exit 0 |
| Perf/cost | timing budgets vs `evals/perf/baselines.json` (hard fail only >10× regression), scale ladder curves, pack-token compliance, token/command accounting per cycle + e2e scenario | `evals/matrix/` + `evals/perf/` | cheap tier CI; full on-demand | budgets hold |

Sequencing: layers 1–2 with merge/hole tasks; smoke + perf baselines right after green;
e2e harness last (tests the finished skill) → becomes the pre-release gate.

## 7 · Evaluation matrix (`evals/matrix/`, new)

Axes: **engines** (base `bb7a51f`, both branch HEADs, unified HEAD, any two commits via
`git archive`) × **scenarios** (golden corpus; seeded synthetic scale ladder N=100/300/1000;
adversarial fixtures: near-dup flood, orphan farm, walled garden, stale-TTL farm; later ptt)
× **workloads** (build, query, maintenance, repair, lint sweep).

Experiments: E1 version ladder; E2 scale sweep (O(N²) vs indexed curve, phase wall times,
pack tokens vs size, query stability); E3 maintenance simulation (k=10 cycles:
ingest→compile→correction→recompile→churn→lint→repair→snapshot; staleness caught/missed,
propagation latency, repair effectiveness, topology drift, cost per cycle); E4 failure-mode
drills (lint recall per planted fault class, guard block rate, repair-plan coverage,
false-positive rate on clean corpus); E5 two-arm semantic across versions; E6 determinism
double-run on every experiment. Output: `evals/matrix/results/matrix-<date>.json` + HTML
matrix view; every future chart states its experiment id; generators seeded and committed.

## Success criteria

1. Merge commit + reconciliation tasks all green: unit (~380+), 10 gates, integration suites.
2. All four holes delivered with their named tests (idempotence, invalidation, equivalence,
   triage round-trip) passing.
3. Smoke command passes for every install mode available on this machine.
4. Perf baselines committed; cheap-tier budget asserts in CI path.
5. E2E harness runs ≥ omp scenarios green; other runtimes recorded or explicitly skipped.
6. Matrix E1 + E2 + E4 + E6 executed on unified HEAD with committed results; E3 runs ≥ 5
   cycles; results charted with experiment ids.
7. A/B bench re-run: base vs unified HEAD committed; no retrieval regression.
8. Docs updated (SKILL/COMMANDS/EVALUATION/wiki-write); spec lessons traceable.

## Out of scope (YAGNI)

ptt real-corpus re-run; topics registry (topic-map-compile branch); SQLite FTS5/BM25 layer;
claim sidecars; multi-agent merge semantics; vector retrieval; e2e prose-quality judgment.

## Risks

- Merge conflicts larger than the 7 known files → surfaced by the merge commit itself;
  additive policy + TODO-UNIFY markers bound the blast radius.
- Guard friction (wrongly blocked pages) → twin-naming messages + `--allow-new`; e2e S2
  verifies the agent path out of the block.
- Synthetic-corpus generator bias → adversarial fixtures modeled on dogfood findings;
  ptt re-run planned next phase.
- E2E runtime availability/cost → explicit skips, token budgets per scenario, pre-release
  (not per-commit) cadence.
- Matrix runtime blowup → small N ladder, on-demand execution, E6 determinism to catch flakes.
