# Known issues (discovered during skill-v2 dogfood, 2026-10-25)

## Fixed in round 2

Fixes for the retrieval eval + doctor-finding census findings (`T1` =
`5077e0a`, `T2` = `4aefbb2`; full retrieval numbers live in
`evals/retrieval/RESULTS.md`).

| ID | Finding | Before evidence | Fix | After evidence |
|----|---------|-----------------|-----|----------------|
| F1 | Paraphrase miss — "blank purchase-order title field" didn't reach `stc-108` | gold q02 R@5=0 | `T2` subject-alias expansion (slug aliases + page-title↔deps index) + field-weighted BM25 | retrieval eval: R@5=1.0 (`evals/retrieval/RESULTS.md`) |
| F2 | Predicate collision — `status` outranked real subjects | q03 ranked `stc-97` above `stc-88` | `T2` per-field IDF down-weights corpus-common predicates (subject×3/predicate×2/value×1) | retrieval eval: P@5=0.711, MRR=0.844 |
| F3 | Warn flood — `missing_effective_date` + `missing_scope` = 66% of 495 doctor findings | doctor census on ptt demo wiki | `T1` warn→info (schema-optional fields) | both codes emit `severity: info`; warn count dropped ≥60% |
| F4 | `staging_leftover` ×26 read as error-adjacent | doctor census | `T1` warn→info (kept receipts are designed behavior) | emits info; exit status unaffected |
| F5 | `orphan_source` ×105 info — unextracted vs extracted-empty indistinguishable | doctor census | `T1` split: `unextracted_source` (no claim cites it) vs `orphan_source` (cited, no page covers) | both codes documented in COMMANDS.md doctor section |
| F6 | Single-source `open_question` subjects starved in `waiting[]` | lane-fill e2e needed manual feeding | `T1` `page_targets` exempts `open_question`-predicated subjects from both thresholds | such subjects appear in `targets[]` at 1 claim |
| F7 | `dead_end_page`/`orphan_page` ×34 — pages not cross-linked | doctor census | `T4` docs: CONVENTIONS.md `## See also` cross-linking guidance (no engine change) | authored pages add `[[wiki:]]` See-also links over time |
| F8 | `supersedes` double-count → `items not unique` exit 3 | COMMANDS.md gotcha + S4 transcript | `T1` `reconcile-apply` validates candidate∩classification overlap → named `TxnError` | error now says `supersedes double-count: …put it only in the classification` |


## KI-1: `compile --resume` multi-run apply can hit stale-version ConflictError

Discovered by the semantic-eval run (Task 5 report).

When two or more staged runs need classification, a single `compile --resume`
applies them in staging-dir order. Each run's `comparisons.json` was snapshotted
at `reconcile_prepare` time; an earlier apply bumping a shared claim's version
leaves the later run's snapshot stale → `ConflictError` (exit 4). On replay,
applied runs keep `pending.json`/`classifications.json`, so the next resume can
re-stage or crash on missing `compile.json` (replay path expects
`state == "prepared"`).

Workaround: apply one run per resume, or delete the affected `run_*` staging
dir and re-stage.

Proper fix (not landed): re-derive the comparison for rows being applied at
`_apply` time (compare candidate vs. CURRENT claim), and consume
`pending.json`/`classifications.json`/`receipt.json` on successful apply so
staging dirs don't accumulate (`staging_leftover` doctor warns on them —
benchmark records 12 per run).

Also worth folding in: `reconcile` is single-match auto-verdict only — two
independent sources asserting the same scoped value in the same run can
produce parallel accepted claims instead of CORROBORATION merges (observed:
two `click_lookback_days=7 prod` claims).

## Scope deviations from the 2026-10-25 skill-v2 spec (deliberate)

- **`doctor --scope <dir>`** (spec WS1.3, incremental lint) — not shipped; the
  near-dup check is O(pages²), fine at current wiki sizes. Revisit when lint
  cost actually bites (~N² lesson, L14).
- **`evals/semantic/TRIGGERS.md`** (spec WS3.5, trigger-eval table) — not
  shipped; trigger coverage is spot-checked during dogfooding instead.
- **Benchmark runs 6 scenarios** vs spec's "9" — the lane comparison covers
  the §60 correctness axes; isolation/injection remain covered by the
  dedicated deterministic gates and semantic suite.
- **`results.json` is gitignored** — regenerated per run; embeds
  `engine_commit`, so committing it would dirty the tree every run.

## Residual minors (final-review triage)

- `PAGE_LINK_RE` requires ≥2-char slugs — a 1-char subject page can exist but
  can never be `[[wiki:]]`-linked (silent non-match). Narrow; widen regex if
  ever needed.
- Self-links are now rejected at write time (`write_page`), but a deps
  path dep equal to the artifact itself written without a body link is
  still possible via `--deps` — cosmetic, verify doesn't flag it.

## `wiki dream` (2026-10-25)

Added corpus reconciliation/GC (`docs/superpowers/specs/2026-10-25-dream-design.md`).
Known boundaries:

- **Sources move outside the transaction.** `stage_write`/`stage_delete` are
  file-level; a directory move can't ride the commit. Dead-source moves are
  done pre-commit — safe because the moved data is by definition uncited,
  and an abort leaves only an extra dir under `.state/dream-archive/`.
- **`stale_decision` is reported, never archived.** A decision resting on
  superseded claims needs human judgment (retract vs re-anchor); dream
  prints it in the plan's `skip` list only via doctor.
- **`derived` drops edges, doesn't rebuild.** Stale artifacts stay marked
  stale for `wiki compile`/`build-pages` to regenerate — dream cleans the
  graph, it doesn't re-render.
- **Accepted-without-evidence claims are skipped, not archived.**
  `unsupported_accepted` is doctor-warn human-review territory; only
  `candidate` orphans archive.
