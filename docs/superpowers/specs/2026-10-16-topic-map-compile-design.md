# Topic-map compile — design spec

Date: 2026-10-16
Status: approved (design); pending implementation plan
Project: llm-wiki (skill + `wiki.py` engine)

## Problem

`wiki.py compile` produces claims but, for typical single-source ingests, few or
no `wiki/` pages. Two causes:

1. `page-targets` eligibility requires ≥2 eligible claims spanning ≥2 distinct
   `root_origin`s (or one `explicit_project_decision`). A subject supported only
   by the ingested source itself — e.g. `s3`, `bigquery` — never qualifies.
2. Kind routing is a regex/prefix heuristic (`.llm-wiki/page-kinds.json`,
   default `concepts`), so even qualifying subjects land as concepts with no
   topical structure.

The intended pipeline — discover concepts/entities/procedures/decisions first,
then attach claims — has no first-class representation.

## Decision

Option A (approved): topology-first compile. Add a topic-discovery phase to
`compile` whose output is merged into a **canonical topic registry**;
`page-targets` consults the registry for kind routing and lowers eligibility
for declared subjects. Claims remain canonical; `wiki/` remains derived. The
`Evidence → Claim → Decision → Derived` architecture and all verify/deps
machinery are unchanged.

Rejected: Option B (pages as canonical state) — inverts the "derived is
disposable" invariant; full provenance/reconcile rewrite for marginal benefit.

## Approved behaviors (user decisions)

| Question | Decision |
|---|---|
| Undeclared candidate subjects | Warn + auto-register as `concept` (`auto_registered: true`) |
| `kind: decision` topics | New `wiki.py record-decision` command mints `decision_*.json` post-apply |
| Same subject, different kinds | `topic_kind_conflict` review item; registry keeps first kind; routing falls back to `page-kinds.json` until resolved |
| Declared topic with zero claims | Kept in registry; listed in receipt `unclaimed[]` |

## Design

### 1. Compile pipeline — `topics.jsonl` per run

`wiki.py compile` (extract phase) returns `topics_path` in every `pending[]`
entry:

```json
{"source_id": "src_x", "run_id": "run_y",
 "content_path": "...",
 "topics_path": ".../.state/staging/run_y/topics.jsonl",
 "candidates_path": ".../.state/staging/run_y/candidates.jsonl",
 "classifications_path": ".../.state/staging/run_y/classifications.json"}
```

Agent contract per source: **write `topics.jsonl` first, then
`candidates.jsonl`**. One topic per line:

```json
{"subject":"lake.data-lake","kind":"concept","title":"Data lake"}
{"subject":"lake.s3","kind":"entity","title":"Amazon S3","aliases":["S3","Simple Storage Service"]}
{"subject":"policy.use-postgres","kind":"decision","title":"Use Postgres","decided_on":"2026-10-01"}
```

Row fields: `subject` (required), `kind` (required), `title` (required),
`aliases` (optional list), `decided_on` (optional hint for decision topics).

Allowed kinds (singular; plural input normalized): `concept | entity |
procedure | question | change | decision`. `source` is not a writable kind —
the engine auto-registers every compiled source as a source-kind topic keyed
on deterministic subject `source.<slug-of-title>`.

Missing `topics.jsonl` → receipt warning `no_topics_declared`; compile
proceeds (backward compatible with runs staged before this feature).
Malformed rows → exit 3 with the offending line numbers.

### 2. Canonical registry — `.llm-wiki/topics.json`

New canonical object, schema-validated (`schemas/topics.schema.json`) and
written only inside the `reconcile_apply` transaction — same discipline as
claims/decisions.

```json
{"version": 3,
 "topics": {
   "lake.s3": {"kind": "entity", "title": "Amazon S3",
     "aliases": ["S3"], "declared_by": [{"source_id": "src_x", "version": 1}],
     "status": "resolved", "alternate_kinds": []},
   "source.my-doc": {"kind": "source", "title": "my-doc",
     "declared_by": [{"source_id": "src_y", "version": 1}],
     "status": "resolved", "auto_registered": true}
 }}
```

Merge semantics at apply time, per declared row:

| Situation | Outcome |
|---|---|
| Same subject + same kind | Union `aliases`; first title wins (receipt warning when later non-empty titles differ); `declared_by` appended |
| Same subject, different kind | Append `topic_kind_conflict` review item; registry keeps first kind; entry `status: "conflict"`, `alternate_kinds` records the loser; `page-targets` falls back to `page-kinds.json` routing for that subject until resolved (applies equally to `decision`-kind rows) |
| Candidate subject never declared | Auto-register `{kind: "concept", "title": <last subject segment, title-cased>, auto_registered: true}` + receipt warning; subjects matched by exact string after `.strip()` (case-sensitive, consistent with claim subjects) |
| `kind: "decision"` row | Merged like any topic (status resolved/conflict) but never emits a page target; surfaced in receipt `decision_topics[]` for `record-decision` follow-up |
| Declared topic, zero claims after apply | Registry entry kept; listed in receipt `unclaimed[]` |

Compile receipt gains a `topics` block:

```json
{"topics": {"declared": 6, "registered": 4,
  "auto_registered": ["misc.foo"], "kind_conflicts": ["lake.delta"],
  "unclaimed": ["lake.presto"], "decision_topics": ["policy.use-postgres"],
  "warnings": ["no_topics_declared (run_...)", "..."]}}
```

### 3. `page-targets` — declared-kind routing, single-claim eligibility

Target-selection precedence:

1. Registry topic with `status: "resolved"` **and ≥1 eligible claim**
   (`accepted`/`provisional`, per `ELIGIBLE_STATUSES`) → emit target with
   `artifact = wiki/<kind-plural>/<slug>.md`, `reason: "declared"`,
   `claim_ids` as today.
2. Subject whose registry entry is `conflict`, or undeclared subject →
   existing rules unchanged (≥2 claims / ≥2 origins, or
   `explicit_project_decision`; `page-kinds.json` routing; default
   `concepts`).
3. `--kind` CLI flag still overrides all routing.

Decision-kind topics never appear in targets. Source-kind topics route to
`wiki/sources/<slug>.md`.

Example outcome for a single data-engineering doc ingest:
`wiki/concepts/data-lake.md`, `wiki/concepts/lakehouse.md`,
`wiki/concepts/data-warehouse.md`, `wiki/entities/s3.md`,
`wiki/entities/delta-lake.md`, `wiki/entities/bigquery.md`, plus
`wiki/sources/<doc-slug>.md` — all from one source version.

### 4. New command — `record-decision`

```bash
wiki.py record-decision --file decision.json
```

Input JSON (agent supplies; stamped fields forbidden in input):

```json
{"title": "Use Postgres for metadata",
 "status": "accepted",
 "decided_on": "2026-10-01",
 "claims": ["claim_01J..."],
 "evidence": [{"source_id": "src_x", "source_version": 1,
   "locator": {"type": "heading", "value": "Decision"}}],
 "body": "...", "scope": {"service": "catalog"}}
```

Engine stamps `id` (`decision_` ULID), `recorded_at`, `version: 1`; validates
against `decision.schema.json`; every `claims[]` id must resolve to an
existing non-terminal claim and every `evidence[].source_id` to a known
source — violations → exit 3. Commits via `Transaction` (`save_decision`).
Run post-apply for each `decision_topics[]` entry so cited claim ids exist.
`build-pages` renders `wiki/decisions/<id>.md` stubs as today — no changes
needed there.

### 5. Introspection, resolution, doctor, backfill

- `wiki.py topics` — registry census: subject, kind, status, declared_by
  count, live claim count. Mirrors `subjects` for claims.
- `review act --action set_topic_kind --params '{"subject":"lake.delta","kind":"entity"}'`
  — new review action; resolves `topic_kind_conflict` items by rewriting the
  registry kind, clearing `status` to `resolved`, and marking the page stale
  if its artifact dir changes.
- `wiki.py topics --backfill` — one-shot migration for existing wikis:
  registers every existing claim subject. Kind is taken from an existing
  page's artifact directory when one exists, else `page-kinds.json` routing.
  Idempotent; never overwrites a `resolved` entry.
- `doctor` additions: unresolved `topic_kind_conflict` count (warn);
  `unclaimed` topics (info); topic-kind vs existing-page-kind mismatch
  (error); `auto_registered` debt (info).

### 6. Engine changes (files)

| File | Change |
|---|---|
| `wikicore/topics.py` | NEW: registry load/save/merge/validate; slug helpers; merge table from §2 |
| `wikicore/compile.py` | `pending[]` gains `topics_path`; apply merges topics.jsonl + auto-registration + unclaimed/conflict detection into receipt; mint `topic_kind_conflict` review items |
| `wikicore/synthesize.py` | `page_targets`: registry-first routing + `reason: "declared"` eligibility |
| `wikicore/review.py` | `set_topic_kind` action |
| `wikicore/claims.py` | `create_decision` helper (stamp + schema + txn) backing `record-decision` |
| `wikicore/doctor.py` | new findings listed in §5 |
| `wikicore/store.py` | `topics.json` recognized in canonical layout |
| `wiki.py` | subcommands: `record-decision`, `topics` (`--backfill`); pass-through unchanged |
| `schemas/topics.schema.json` | registry schema |
| `scripts/tests/` | new test module(s), pytest style matching existing suite |

### 7. Skill/doc updates

- `skill/wiki-compile/SKILL.md` — extract phase becomes two-file contract
  (topics before candidates); receipt reporting includes `topics` block.
- `references/EXTRACTION.md` — `topics.jsonl` row format, kind vocabulary,
  "declare every subject before extracting claims for it".
- `references/COMMANDS.md` — `topics`, `record-decision`, new
  `page-targets` semantics, `set_topic_kind` review action.
- `references/ARCHITECTURE.md` — topic registry is a fifth canonical object;
  invariants unchanged.
- `wiki-write` `PAGES.md` / `CONVENTIONS.md` — note `reason: "declared"`
  targets and kind-explicit artifacts.
- `skill/llm-wiki/SKILL.md` — command router gains `topics` /
  `record-decision` rows.

## Error handling

- Malformed `topics.jsonl` (bad JSON, missing `subject`/`kind`/`title`,
  unknown kind) → exit 3 listing line numbers; staging dir preserved for fix-
  and-resume.
- Exit 4 (stale revision) on apply: unchanged — re-run `compile` to refresh.
- `record-decision` referencing unknown/terminal claim ids → exit 3 with ids.
- `set_topic_kind` on a non-conflict subject → exit 3.

## Testing

New pytest module(s) under `scripts/tests/` covering:

- topic merge determinism (same-subject same-kind union; title-first-wins)
- kind conflict → review item → `set_topic_kind` → resolved routing
- auto-registration of undeclared subjects + receipt warnings
- declared single-claim eligibility and declared-kind routing beating
  `page-kinds.json`
- conflict subject falls back to `page-kinds.json` until resolved
- `record-decision` happy path + unknown-claim rejection
- `topics --backfill` idempotence on a fixture wiki
- missing `topics.jsonl` → warning, not failure
- source-kind auto-registration per compiled source

## Non-goals

- Pages remain derived; no changes to `write-page` validation beyond the new
  target reasons/kinds already permitted.
- No changes to reconcile classification (needs_review flow untouched).
- No graph/query changes; `graph-data` already renders entities/pages.
- Human-authored topics: editing `topics.json` by hand is forbidden (same
  rule as claims); all mutation via compile apply or `set_topic_kind`.
