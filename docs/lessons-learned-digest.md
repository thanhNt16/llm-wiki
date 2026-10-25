# Lessons-learned digest (from llm-wiki-landscape.html + llm-wiki-report.html)

Distilled, actionable findings from the two Oct-24-2026 research reports
(277 + 70 sources). Each item cites its source claim.

## Write path / ingest
- L1. **Near-duplicate pages are the default failure mode** — "extend, don't create" (HagaiHen, 69-page podcast wiki distillation). Ingest must search before creating.
- L2. **Plan-then-write two-pass compile** — first pass lists touched concepts with justifying sentences; second rewrites each page in full. Asking both at once "drops half the page" (earkeep).
- L3. **Typed source classifier lifts answer rates 62%→89%** over 525 docs; ingest decisions ate ~72% of tokens (sassarini-marco). Classify the source type first.
- L4. **Two-source rule** — a standalone page/claim earns existence when ≥2 distinct sources corroborate it (Iusztin).
- L5. **Auto-capture yes, auto-promotion no** — session digests may be auto-captured; promoting to wiki requires explicit step (nvk).
- L6. **Ingest fan-out ~10–15 pages per source** — summary page + entity/concept updates + index + log (Karpathy).

## Read path / query
- L7. **Index-first navigation, stop at first sufficient answer** — progressive disclosure ladder: index → 1–3 pages → source pages → raw as last resort (Pollack, Iusztin).
- L8. **Anti-hallucination discipline** — if the wiki doesn't know, say so; never fill gaps from model memory.
- L9. **~100–200 page ceiling for index-first nav** — plan a derived/disposable index (SQLite FTS5/BM25) before it's needed.
- L10. **Derived indexes are disposable caches, never truth** (nvk Derived Index Protocol).

## Maintenance / lint
- L11. **Lint emits a report; never silently edits** — dead links/orphans are editorial decisions (retarget/split/delete) that scripts can't make. "Budget the decisions, not the lint" (Tuszynski: dead links grew 116→170 while pages grew 261→334).
- L12. **Orphan rule** — don't commit a page without 1–3 inlinks; flag dead-end pages (no outlinks) symmetrically (Wikipedia:Orphan).
- L13. **Walled gardens** — lint should detect clusters that link only internally.
- L14. **Lint cost grows ~N² with page count** (Covenant0028) — need incremental/scoped lint.
- L15. **Deterministic checks first** (nvk C1–C20: index drift, frontmatter enums, placement, link integrity, coverage, registry drift); LLM audits only for semantics.
- L16. **Circular grading risk** — "same model grading its own homework" (Fluture stress test: lint found 23 real issues but grading was circular; "architecture was sound — it needed an immune system"). Deterministic checks + human gates counter it.

## Trust / provenance / temporal
- L17. **Error-profile inversion** — a wrong wiki claim is durable and invisible; it compounds (vsbabu). Claim-level provenance + review gates are load-bearing.
- L18. **Supersede, never delete** — immutable revisions with `supersedes` chains (nvk; Zep bi-temporal).
- L19. **verified is human-only** trust tier; `generated` = LLM-written (OKF, nvk v1.0).
- L20. **Stale claims need TTLs / source fingerprints** — `stale_after` frontmatter, drift-triggered replans (OKF, OpenWiki).
- L21. **Durable hallucinations** — governance: immutable sources, git history, labeled page types (summary vs speculative inference), human approval for consequential writes (Di Costanzo).

## Structure / scope
- L22. **Knowledge accumulates; state supersedes** — separate the accumulating wiki from overwriting working-state ("blocked on login bug" can't be a page). Most common first mistake is one store for both (Režun).
- L23. **Soft schema** — suggested page types + validated merges; a 5-class ontology beats 50-class for LLM extraction.
- L24. **Dual-linking** — `[[slug|Name]]` + `(path)` for Obsidian+agent+GitHub readability (nvk).
- L25. **Session digests are episodic capture**; crystallization converts completed work into digest pages.

## Evaluation (the field's weakest link — our differentiator)
- L26. **"Evaluation is just vibes at this point"** (Iusztin); no benchmark ships with any flagship wiki. WikiBench + LEDGER (OpenWiki) are early steps.
- L27. **Calibration target ≥95%** for extraction/scope/correction/historical accuracy (our PRD §59).
- L28. **Stars measure virality, not substance** (mempalace ~47k★ on hype) — evals must be reproducible, recorded with engine commit.

## Economics
- L29. **Wiki-only answering measured 86% faster / 61% cheaper** per run (Taktile production).
- L30. **Smaller model ingests, larger model queries** (Iusztin: Sonnet in, Opus out).
