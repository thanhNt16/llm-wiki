# Skill-v2: lessons-learned skillset hardening + benchmark harness — design

Date: 2026-10-25. Branch: `skill-v2` (worktree `.worktrees/skill-v2`).
Basis: `docs/lessons-learned-digest.md` (L1–L30, distilled from
`llm-wiki-landscape.html` + `llm-wiki-report.html`, 347 combined sources).

## Problem

The skillset is architecturally sound (evidence→claims→decisions→pages,
staged transactions, supersede semantics, two-source eligibility, injection
warnings — all present and green: 346 tests, 9/9 gates). But the *operating
guidance* in SKILL.md/references lags the field's documented lessons, and the
PRD §60 baseline benchmark — the one artefact that proves the wiki beats
plain-Markdown baseline — was never implemented. Landscape verdict:
"evaluation is just vibes at this point"; shipping a reproducible benchmark
is the differentiator.

## Audit result (current state)

| Lesson | State |
|---|---|
| L4 two-source eligibility, L18 supersede, L21 injection, L8 refuse-on-gap, L11 lint-reports-not-edits, L15 deterministic checks, L5 no auto-promote | **implemented** |
| L1 extend-don't-create (page/subject near-dup guidance) | **missing** — slug collisions dedup files but agents are never told to search subjects first |
| L2 plan-then-write, L3 typed source classifier | **partial** — extraction per-source exists; no classify step, no plan pass before page writes |
| L7 navigation ladder | **partial** — index-first documented; no "stop at first sufficient answer"; page bodies can't link to pages (`LINK_RE` = claim/decision ids only) |
| L12 orphan rule, L13 walled gardens, L14 N² lint | **missing** — `LINK_RE` only matches `[[claim_*]]`/`[[decision_*]]`; no page graph at all (though `deps` already *accepts* `wiki/<kind>/<slug>.md` paths — unused) |
| L19 verified human-only, L20 stale_after TTL | **missing** — `verified` authority exists but nothing gates it to humans; no TTL field |
| L22 knowledge vs state split | **partial** — `session` source kind exists; no digest→page workflow doc |
| L26–L28 benchmarks | **missing** — semantic results.jsonl stale (Sept, 4/9 rows); no §60 baseline harness; no trigger/perf evals (§55) |

## Design (3 workstreams)

### WS1 — Engine: page-link graph + doctor checks (small, deterministic)

The `deps` contract already accepts `wiki/<kind>/<slug>.md` artifact paths and
`verify`/`doctor` resolve them — so page→page edges exist in the dependency
graph today; only the authoring convention and diagnostics are missing.

1. **`[[page]]` body links**: extend `LINK_RE` consumers so a page body can
   cite another page as `[[wiki:<kind>/<slug>]]` (e.g. `[[wiki:concepts/order-attribution]]`).
   - `synthesize.py` link extraction: `[[wiki:…]]` targets must appear in
     `deps` as the corresponding `wiki/<kind>/<slug>.md` dep (linked ⊆ deps,
     same rule as claims).
   - `pages.py` verify: `[[wiki:…]]` must resolve to an existing page file.
   - `graph-data`: emit `page→page` edges from deps so the 3D explorer shows
     the wiki's actual topology.
2. **Doctor page-graph checks** (new warn codes):
   - `orphan_page` — page with zero inbound page-links and absent from any
     question page / index beyond its section index (L12).
   - `dead_end_page` — page with zero `[[wiki:…]]` outlinks (L12 symmetric).
   - `walled_garden` — a connected component of ≥3 pages whose `[[wiki:…]]`
     edges never leave the component (L13).
   - `near_duplicate_subject` — ≥2 page subjects whose token sets overlap
     ≥ Jaccard 0.7 without a `[[wiki:…]]` link between them (L1 canary).
   - `stale_page_ttl` — page frontmatter `stale_after` date < today (L20;
     field optional, check only when present).
3. **`doctor --scope <dir>`** — restrict checks to a subtree for incremental
   lint on large wikis (L14 mitigation; full scan stays default).

### WS2 — Skill instructions (agent-facing lessons)

- `wiki-ingest`: add **classify-first step** — agent tags each source
  `spec|adr|memo|meeting|config|code|transcript|session|other` before ingest;
  pass `--text-ref`/`kind` accordingly; use the kind to pick extraction
  authority defaults at compile (L3).
- `EXTRACTION.md`: add subject-consolidation rule — run `wiki.py subjects`
  before minting a new subject; prefer extending an existing subject (L1);
  add `source_kind` guidance tying authority choice to the classified kind.
- `wiki-compile`: explicit **plan-then-write** two-pass wording for the
  page-write phase: first list targets + one-line justification, then author
  pages (L2).
- `wiki-write` CONVENTIONS+PAGES: `## See also` section contract — 1–3
  `[[wiki:…]]` links to related pages is expected on concept/entity pages
  (L12/L7); near-duplicate check before authoring a new page (L1);
  `stale_after` optional frontmatter documented (L20).
- `wiki-query`: navigation ladder made explicit — index/section index →
  matching pages → cited claims → raw source last resort; **stop at first
  sufficient answer** (L7).
- `wiki-doctor`: document new page-graph codes and the editorial repair loop
  — retarget/split/delete are agent+human decisions, never silent edits (L11).
- `llm-wiki` SKILL.md: add working-state note — mutable task/session state
  goes to session sources and decision records, never wiki pages (L22);
  trust note that `verified` authority implies human verification (L19).

### WS3 — Benchmark + eval hardening (PRD §55/§60 gaps)

New `evals/benchmark/`:

1. **`run_benchmark.py`** — deterministic, stdlib-only, no model calls:
   - **Wiki lane**: copy golden-corpus → `init` → ingest all → compile with a
     **pinned `candidates.jsonl`** (gold extraction committed to
     `evals/golden-corpus/bench/candidates.jsonl`) → `build-pages` → for each
     of the 9 scenario questions run `query-prepare` (+`--as-of` for the
     historical case) and score against expected subject/predicate/answer
     patterns from `bench/expectations.json`.
   - **Baseline lane** (PRD §60): same corpus minus wiki — answer via
     keyword search (`grep`-style ranking in pure Python over raw files +
     a human-written `PROJECT.md` we commit as the baseline's manual summary);
     score identical questions.
   - **Metrics** (per §60): recall (gold-answer presence), correction
     handling (new-value wins), historical accuracy (as-of), scope
     correctness, conflict surfacing (both values), refusal on unanswerable,
     citation/traceability (citation resolves to source version vs. file
     name), context efficiency (bytes the lane had to load to answer).
   - Output: `evals/benchmark/results.json` + stdout table; exit non-zero if
     wiki lane fails any correctness metric (baseline may fail — that's the
     point, it's the comparison).
2. **Page-graph fixture**: add 2 planted near-duplicate subjects + 1 orphan
   + 1 walled garden to golden-corpus bench fixtures so the new doctor codes
   have golden expectations (`test_bench_doctor` in run_benchmark).
3. **`test_gates_wrapper.py`** extended: benchmark runs inside the release
   gate suite as gate 10 (`benchmark_correctness`).
4. **Semantic refresh**: re-run the 9 SCENARIOS.md scenarios on the golden
   corpus with current HEAD (agent-driven pass) and append to
   `results.jsonl` with `engine_commit` — replaces the stale Sept rows.
5. **Trigger matrix**: `evals/semantic/TRIGGERS.md` — intent→skill table for
   §55 trigger evals (doc; verified by spot checks during dogfood).

### Non-goals

- No SQLite/FTS5 derived index (premature: <200-page ceiling not hit in
  evals; indexed as future work, L9/L10 noted in docs).
- No bi-temporal engine upgrade; `valid_from/to` + supersede already covers
  the MVP temporal model.
- No multi-agent merge semantics (open frontier, out of scope per landscape).

## Error handling / testing

- Every new doctor code gets a unit test in `test_doctor.py` + a golden
  fixture trigger in benchmark.
- `[[wiki:…]]` link parsing: tests for linked⊆deps, unresolved target
  rejection, malformed target rejection.
- Benchmark is deterministic: same inputs → same results.json; CI-safe.

## Final deliverable

`skill-v2-report.html` (root): single-file interactive report — process recap
(superpowers flow steps), lesson→change mapping, before/after benchmark
tables + charts, doctor-graph findings demoed on the bench corpus.
