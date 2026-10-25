# Improve Round 2 — Retrieval Redesign + Surgical Finding Fixes

**Date:** 2026-10-25 · **Branch:** skill-effectiveness · **Status:** approved (user)

Combines Track A (surgical per-finding fixes) + Track B (deterministic
BM25-lite retrieval redesign). Driven by the retrieval eval
(`evals/retrieval/`, committed `af1d697`) and the doctor-finding census on
`/tmp/ptt-wiki-demo-2` (495 findings / 0 errors).

## Findings being fixed

| ID | Finding | Evidence | Fix |
|----|---------|----------|-----|
| F1 | Paraphrase miss — "blank purchase-order title field" doesn't reach `stc-108` | q02 R@5=0 | B: subject-alias expansion + field-weighted BM25 |
| F2 | Predicate collision — `status` outranks real subjects | q03: stc-97 > stc-88 | B: IDF down-weights corpus-common predicates |
| F3 | Warn flood — `missing_effective_date` + `missing_scope` = 66% of 495 findings | doctor census | A: warn→info (schema-optional fields) |
| F4 | `staging_leftover` ×26 reads as error-adjacent | doctor census | A: warn→info (kept receipts are designed behavior) |
| F5 | `orphan_source` ×105 info — unextracted vs extracted-empty indistinguishable | doctor census | A: split code `unextracted_source` vs `orphan_source` |
| F6 | Questions lane starvation — single-source open_question subjects never become targets | lane-fill e2e needed manual feeding | A: `page_targets` min_claims=1 for `open_question`-predicated subjects |
| F7 | `dead_end_page`/`orphan_page` ×34 — pages not cross-linked | doctor census | skill-side guidance only (no engine change; documented) |
| F8 | `supersedes` double-count → `items not unique` exit 3 | COMMANDS.md + S4 transcript | A: validate candidate∩classification overlap → named TxnError |

F1+F2 are the only findings that require Track B; everything else is a
one-region surgical change.

## Track B — query.py scoring redesign

**Replace** `_score()`'s linear token weights with a deterministic,
stdlib-only **BM25-lite** over three fields:

```
doc(claim) = { subject: <subject tokens>,
               predicate: <predicate tokens>,
               value: <value tokens> }
score(q, c) = Σ_fields w_f · BM25_field(q_tokens, field_tokens)
weights:  subject ×3 · predicate ×2 · value ×1
BM25:     k1=1.2, b=0.75, IDF per field across all claims
```

- **Subject-alias index** built at query time (not persisted):
  - slug parts (`stc-108` → {stc,108, stc108}), both directions
  - `wiki/<kind>/<slug>.md` frontmatter `title:` ↔ its page deps' subjects
  - matched subjects expand the query's token set against the subject field
- **Determinism**: identical claim set → identical ranking; ties sort by
  `claim_id` ascending (stable across runs, testable).
- **Unchanged surfaces**: `--as-of` filtering, terminal-status exclusion,
  `superseded[]`/`conflicts[]` sections, output schema (matches[] entries
  keep `score`, `score_breakdown` becomes per-field BM25 contributions).
- **Bounds**: default params, no hand-tuning to the gold set; if a gold
  question still misses, that's an honest signal, not a tuning target.
- **Failure behavior**: empty wiki → `matches:[]`; malformed page
  frontmatter during alias indexing → skip page + `warnings[]` entry;
  never raises (prepare is read-only).

## Track A — surgical changes

1. **doctor.py F3**: `missing_effective_date`, `missing_scope` → info
   severity (schema-optional; extractor guidance lives in compile-plan,
   not per-claim warns).
2. **doctor.py F4**: `staging_leftover` → info.
3. **doctor.py F5**: split `orphan_source` → `unextracted_source` (zero
   claims reference it) vs `orphan_source` (claims exist but no page
   coverage). Both stay info.
4. **synthesize.py F6**: `page_targets` — a subject whose claims include
   an `open_question` predicate joins targets at `min_claims:1` regardless
   of `min_sources` (open questions are definitionally awaiting humans;
   holding them back is the starvation).
5. **compile.py F8**: in `reconcile_apply` before staging, if a
   candidate's `supersedes[]` and its classification's `target_claim_id`
   both reference the same target → `TxnError("supersedes double-count:
   target X appears in candidate.supersedes and classification; put it
   only in the classification")`.
6. **Skill docs F7**: CONVENTIONS.md gains a `## See also` / cross-linking
   note so authored pages produce `[[wiki:]]` links (reduces both warns
   over time; no engine change).

## Data flow (Track B)

```
query-prepare --question Q
  → claims.list_claims()
  → build field docs + IDF tables
  → build subject-alias index (pages + slugs)
  → expand Q tokens through aliases
  → BM25 rank (per-field, weighted)
  → same filters (status/as_of/valid_to)
  → emit {matches, superseded, conflicts} — schema unchanged
```

## Acceptance / thresholds

- `evals/retrieval/run_retrieval.py --wiki /tmp/ptt-wiki-demo-2`:
  **recall@5 = 1.0, precision@5 ≥ 0.75, MRR ≥ 0.85, unanswerable 2/2 clean**
- gold.json grows by ≥6 questions (4 paraphrase, 2 predicate-collision);
  paraphrase-style "title field blank when editing PO" must rank `stc-108`
  in top-5.
- doctor on the same corpus: warn-count drops ≥60% (495 → ≤200) with
  **zero new error findings**; `auto_decision_stub` info preserved.
- F6: seeded wiki with a single-source `open_question` subject → appears
  in `page-targets` `targets[]` (was `waiting[]`).
- F8: unit test asserting the named TxnError (no `items not unique`).
- 425-test suite stays green (+new tests), 10/10 gates.

## Non-goals

- No LLM/embedding retrieval (stdlib-only stays the constraint).
- No `wiki-visualize` app changes (its data contract is `graph-data` —
  untouched).
- No changes to Jev advisory wiring.
