# Improve-Round-2 Implementation Plan

**Spec:** `docs/superpowers/specs/2026-10-25-improve-round-2-design.md`
**Mode:** subagent-driven, auto-approved

## Tasks

- **T1 (surgical, engine)** — `wikicore/doctor.py`: `missing_effective_date`,
  `missing_scope`, `staging_leftover` warn→info; split `orphan_source` into
  `unextracted_source` (no claims) vs `orphan_source` (claims exist, no page).
  `wikicore/synthesize.py`: `page_targets` — subjects containing an
  `open_question`-predicated claim join targets at min_claims=1 (bypass
  min_sources too? — NO: keep min_sources for evidence diversity; open
  questions enter waiting→targets at 1 claim). `wikicore/compile.py`: F8 —
  in `reconcile_apply`, if candidate `supersedes` ∩ classification
  `target_claim_id` overlap → named TxnError. Tests in matching test files.
- **T2 (retrieval redesign)** — `wikicore/query.py`: replace `_score` with
  BM25-lite (k1=1.2,b=0.75) over weighted fields (subject×3, predicate×2,
  value×1), per-field IDF; subject-alias index (slug parts + page
  frontmatter titles); deterministic tie-break claim_id. `score_breakdown`
  becomes per-field contributions. New `test_query_bm25.py`.
- **T3 (gold + eval)** — extend `evals/retrieval/gold.json` (+4 paraphrase,
  +2 predicate-collision, +1 as_of); run scorer on ptt demo wiki pre/post;
  thresholds R@5=1.0, P@5≥0.75, MRR≥0.85, unanswerable clean.
- **T4 (docs)** — `docs/known-issues.md` before/after table for F1–F8;
  `wiki-write/references/CONVENTIONS.md` See-also cross-link note (F7);
  COMMANDS.md severity/code changes.

## Ordering

T1 ∥ T2 (disjoint files). T3 after T2 (eval measures new scorer; gold
edits themselves independent but runs must come last). T4 after T1+T2.
Full gate (425+ tests, 10 gates) at end.

## Acceptance

Per spec §Acceptance: retrieval thresholds met on ptt gold set; doctor
warns ≥60% down; zero new errors; F6 single-source open_question → target;
F8 named TxnError regression test; suite green.
