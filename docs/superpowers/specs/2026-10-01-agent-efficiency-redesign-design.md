# llm-wiki agent-efficiency redesign — spec

Date: 2026-10-01
Status: approved-in-dialogue (sections 1–4 approved individually)

## Problem

A real CellockAI session compiling one `.docx` (42 claims) consumed ~60k tokens
and 5+ approval-gated shell commands because:

1. The agent hand-wrote a ~400-line candidates heredoc repeating mechanical
   fields (`id:"auto"`, `status`, `proposed_by`, `recorded_at`, full
   `evidence[]`, `root_origin`) the engine owns. Shell-escaping produced a real
   bug (`.\\\\setup.ps1` baked into a claim value).
2. `reconcile-prepare` echoed all 42 candidates in full (~2000 lines) though
   the agent only needed the `matches` array.
3. All 42 rows were UNRELATED (empty wiki) — a verdict the engine had already
   computed — yet the agent emitted 42 classification lines.
4. (Note: binary ingest is already handled — markitdown inside the engine,
   `--dir`, `--normalized-content` fallback — so the residual pain is entirely
   in the compile loop.)

Goal: agent does semantics only; engine does all bookkeeping; nothing large
echoes through the chat.

## 1. Command surface — `wiki.py compile` umbrella

Three invocations per compile batch, resumable via run state:

```
python3 wiki.py compile
# → {"pending": [{"source_id", "source_version", "run_id", "content_path",
#     "candidates_path": ".llm-wiki/.state/staging/<run_id>/candidates.jsonl"}]}
# One run per pending source; --resume advances every staged run one phase.

# agent reads each content_path, writes shorthand JSONL at candidates_path

python3 wiki.py compile --resume
# → {"run_id", "auto": {"unrelated": N, "duplicate": N},
#    "needs_review": [{index, key, candidate_value, scope, matches, hint}]}
#    (each needs_review row also carries run_id + source_id for multi-source batches)

# agent writes .llm-wiki/.state/staging/<run_id>/classifications.json covering ONLY
# needs_review rows

python3 wiki.py compile --resume
# → run receipt: {changes, conflicts, revision, verify: {ok}}
```

Phase dispatch is state-driven, not flag-driven: `--resume` inspects the run
dir — candidates.jsonl present → stage+prepare; classifications.json present →
apply+build-pages+verify. Missing expected file → exit 3 naming the path.

Low-level commands (`compile-plan`, `stage-candidates`, `reconcile-prepare`,
`reconcile-apply`, `build-pages`, `verify`) remain for tooling and debugging;
`compile` orchestrates them internally. `wiki-review` flow unchanged.

## 2. Data contracts

### Candidates — shorthand JSONL (`candidates.jsonl`, one object/line)

```jsonl
{"subject":"cellock_ai.platform","predicate":"host_editor","value":"Visual Studio Code","locator":"h:1.1 What is Cellock AI?","authority":"manual"}
{"subject":"mcp.toolbox-postgres","predicate":"db_role_requirement","value":"read-only","locator":"l:269","authority":"decision","scope":{"product":"cellock-ai"}}
```

Agent-owned fields: `subject`, `predicate`, `value`, `locator`, `authority`,
optional `scope`, `valid_from`, `supersedes`.

Engine-stamped fields (agent MUST NOT set): `id` (real `claim_*` id),
`recorded_at`, `status:"candidate"`, `proposed_by`, `evidence` (built from
`source_id`/`source_version`/`locator`), `root_origin`.

Shorthand expansions:
- `locator`: `h:<heading>` → `{"type":"heading","value":…}`;
  `l:<start>[-<end>]` → `{"type":"line_range","value":…}`;
  `s:<section>` → `{"type":"section","value":…}`. A full locator object is
  also accepted.
- `authority`: `manual|doc|config` → `authoritative_source`;
  `decision|adr` → `explicit_project_decision`; `code|verified` →
  `implementation_verification`; `inferred` → `agent_inference`. A full
  authority object is also accepted (custom source names).

Back-compat: the existing verbose JSON array format remains valid input to
`stage-candidates` (detected by `.json` extension / array shape).

### Reconcile output (compact)

`needs_review` rows: `{index, key, candidate_value, scope,
matches:[{claim_id, same_scope, same_value, temporal, status, current_value}],
hint}` — full candidate objects are never echoed back (the agent wrote them).

`hint` = deterministic suggestion (`UNRELATED`, `DUPLICATE`,
`CORROBORATION?`, `CONTRADICTION?`); non-binding, `?` marks judgment calls.

### Classifications

Schema unchanged, but only `needs_review` rows require an entry. Engine
auto-classifies:

- `matches == []` → `UNRELATED`
- single match, `same_scope && same_value`, same `root_origin` lineage →
  `DUPLICATE`
- single match, `same_scope && same_value`, different `root_origin` →
  `CORROBORATION` (deterministic per RECONCILIATION.md step 3)

Receipt gains `auto` counts + `auto_classified: [indices]` so the verdict is
auditable, never silent.

## 3. Ingest — ALREADY LANDED (this spec changes nothing here)

Verified in `wikicore/sources.py`, `dirs.py`, `wiki-ingest/SKILL.md`:

- `ingest --dir <path>` + `--include-hidden` + `--max-bytes` — recursive
  directory ingest already exists (`dirs.ingest_dir`), batch receipt already
  emitted.
- Office formats: `OFFICE_EXTS = {.pdf,.docx,.xlsx,.pptx,.doc,.xls,.ppt}`
  are extracted **inside the engine via markitdown**; on failure the receipt
  reports `coverage.text: not_extracted` and the skill instructs the agent to
  extract text itself and re-ingest with `--normalized-content`.
- Images keep the vision-describe flow — unchanged.

This spec therefore scopes to the compile loop only. (Optional follow-up,
NOT in scope: replacing the markitdown dependency with a stdlib
zipfile+XML shim if dep-free install becomes a requirement.)

## 4. Skills, errors, testing

- `wiki-compile/SKILL.md` — rewritten around the 3-command flow with the
  shorthand JSONL example.
- `wiki-ingest/SKILL.md` — no changes needed (already documents --dir and
  the office-format fallback).
- `llm-wiki` SKILL.md + `references/COMMANDS.md`, `EXTRACTION.md`,
  `RECONCILIATION.md` — updated to match; auto-classified verdicts documented
  as engine-owned, reference now lists only what the agent still decides.
- WIKI resolver line stays as the `ls`-glob fallback (omp / ~/.agents /
  ~/.cellockai) — a PATH shim is not reliably on PATH inside agent shells.
  All phrasing harness-generic.
- Exit codes reused: 3 = malformed shorthand line (names the line number),
  missing candidates/classifications file on `--resume`, or any validation
  failure; 4 = stale revision; 5 = locked.
- Tests (`evals/` + `test_skill_package.py` conventions):
  - unit: shorthand JSONL → claim normalization; locator/authority shorthand
    expansion; auto-classify truth table (UNRELATED/DUPLICATE/CORROBORATION
    boundaries); compact reconcile-prepare output shape.
  - golden: one scenario drives `compile` end-to-end on the fixture project;
    asserts `auto.unrelated` count, residual `needs_review` handling, and
    `verify.ok`.
  - No new runtime deps.

## Non-goals

- No embedded PDF parser (external-tool fallback documented instead).
- No change to claim schema on disk — shorthand is an input format only.
- No change to ingest internals — `--dir`/markitdown/`--normalized-content`
  already shipped.
- No removal of low-level commands or the verbose candidate format.
- `wiki-review`, `wiki-query`, `wiki-context`, `wiki-doctor`, `wiki-visualize`
  flows unchanged.
