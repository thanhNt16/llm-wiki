# COMMANDS.md — per-command contracts

Run scripts from the project root via the `WIKI` variable resolved in the
skill (`python3 "$WIKI" <cmd> …`). All commands print one JSON object.

## init

Idempotent, non-destructive. Creates `.llm-wiki/` layout, config, state.

```bash
wiki.py init --name <project-name>
```

- If output says `"initialized": false`, the wiki already exists — do nothing
  destructive; optionally run `wiki.py doctor`.
- After first init, report: storage path, revision, and the seven commands.

## ingest

Preserves and normalizes evidence. **Never creates or modifies claims.**

```bash
wiki.py ingest --file path/to/doc.md
wiki.py ingest --url https://example.com/spec
wiki.py ingest --text "user said: use Postgres" --text-ref "chat-2026-09-15"
wiki.py ingest --file report.pdf     # preserved; extraction honestly marked not_extracted
wiki.py ingest --dir docs/                        # recursive; batch receipt
wiki.py ingest --dir docs/ --include-hidden --max-bytes 104857600 --force
wiki.py ingest --file img.png --normalized-content desc.md --parser-name agent-vision
wiki.py ingest --file path/to/doc.md --source-id source_...  # pin identity
```

Receipt fields: `source_id`, `version`, `sha256`, `deduplicated`, `warnings`,
`secrets`, `coverage`. Check receipts for:

- `deduplicated: true` → same bytes already ingested; do NOT recompile it.
- `warnings` containing `possible_prompt_injection` → tell the user; the text
  stays evidence-only.
- `secrets.findings` non-empty → surface to the user; policy `deny` aborts.
- Binary coverage `not_extracted` → extraction did not silently fail; if the
  content matters, extract it with a document tool and re-ingest as text.
- `normalized_source: "agent"` → content.md came from `--normalized-content`;
  `parser.name` reflects `--parser-name`.
- `updated_normalized: true` → identical bytes, prior version was
  `not_extracted`; content.md + extraction.json updated in place and the
  version was un-compiled so compile-plan sees the new text.
- `--force` bypasses sha256 dedup and always appends a version — use when the
  extractor improved (markitdown installed, better vision pass).

## compile

Three resumable phases: extract → classify → apply. The first call creates one
staging run per pending source and returns `pending[]` entries with
`content_path`, `topics_path`, `candidates_path`, and `classifications_path`:

```bash
wiki.py compile
# write topics.jsonl (one topic row per subject) to each topics_path FIRST,
# then one shorthand JSON object per line to each candidates_path
wiki.py compile --resume
# if needs_review rows remain, write classifications.json, then:
wiki.py compile --resume
```

Runs are stored in `.llm-wiki/.state/staging/<run_id>/` with `topics.jsonl`,
`candidates.jsonl`, `classifications.json`, and optional `report.json`.
`--resume` advances staged runs: it parses `topics.jsonl` (a missing file adds
a `no_topics_declared` receipt warning) and merges declared rows into
`topics.json`, then returns auto verdict counts plus `needs_review` rows,
then applies the supplied classifications and returns the receipt, changes,
conflicts, and `verify` result. UNRELATED, DUPLICATE, and CORROBORATION are
auto-classified; the agent classifies only `needs_review`.

The low-level `compile-plan`, `stage-candidates`, `reconcile-prepare`, and
`reconcile-apply` commands remain available. `stage-candidates --file` accepts
`.json` arrays/objects and `.jsonl` shorthand files.

After the apply lands, the receipt carries a `topics` block:

```
topics: {declared, registered, auto_registered[], kind_conflicts[],
         decision_topics[], unclaimed[], warnings[]}
```

- `decision_topics[]` — declared `decision`-kind subjects; record each via
  `wiki.py record-decision --file decision.json` (see `record-decision`).
- `kind_conflicts[]` — subjects declared with different kinds across rows;
  each also opens a `topic_kind_conflict` review item, resolved via
  `wiki.py review act --action set_topic_kind`.
- `auto_registered[]` — subjects extracted but never declared; registered as
  `concept` with a warning. `unclaimed[]` — declared topics (except
  `source`/`decision`) that ended the run with zero claims.

Then:

1. Synthesis — regenerate invalidated or stale pages you authored per the
   wiki-write skill: run `wiki.py page-targets`, and for EVERY `targets[]`
   entry use `target.artifact` + `target.claim_ids` verbatim (never hardcode
   `wiki/concepts/<slug>.md`). See the `write` section below.
2. `wiki.py build-pages` — rebuilds index/decision stubs and clears stale
   flags.
3. `wiki.py verify` — must report `"ok": true` before you finish. Fix every
   error (unresolved links, stale pages) and re-verify.

Report the run receipt: changes counts, conflicts (new review items).

## write

Synthesis: turns claims into readable pages under `.llm-wiki/wiki/`. You
write the prose; the script validates and commits. Run after `compile` or
whenever pages are stale. Full page contracts: the `wiki-write` skill
(CONVENTIONS.md, PAGES.md there).

```bash
wiki.py page-targets                    # qualifying subjects + stale pages
wiki.py page-targets --kind concepts    # one kind only
wiki.py page-targets --stale-only       # only refresh targets
```

A subject qualifies with ≥2 eligible claims (`accepted`/`provisional`)
spanning ≥2 distinct `root_origin`s, or one eligible claim whose
`authority.type` is `explicit_project_decision`, OR — regardless of the
claim/origin thresholds — when the subject is a resolved topic in
`.llm-wiki/topics.json` with a page kind (`reason: "declared"`).
`decision`-kind topics never emit page targets (they surface via
`decision_topics[]` for `record-decision`); resolved page-kind topics with
zero eligible claims appear in `waiting[]` as `declared_no_claims`.
Kind routing: a resolved declared topic's kind wins first; `--kind`
overrides; otherwise `.llm-wiki/page-kinds.json` ordered rules
(`subject|prefix|suffix|regex|predicate` matchers, `when.min_claims`, first
match wins) decide the dir — missing file routes everything to `concepts`.

For each target, read its claim details, author the page per the wiki-write
skill's PAGES.md (frontmatter `type subject title created deps`; `updated`
and `stale` are set by the script), then commit with `target.artifact`
verbatim:

```bash
wiki.py write-page --file page.md --artifact <target.artifact> --deps 'claim_abc123@3,claim_def456@1'
# → {"written": "wiki/concepts/attribution.md", "deps": [...], "claims_cited": 4}
wiki.py write-page --file - --artifact wiki/concepts/attribution.md   # page text on stdin
wiki.py write-page --text '<markdown>' --artifact wiki/questions/open-billing.md --deps ''
```

- Deps resolution order: `--deps` (comma-joined) → `--deps-file` (a JSON
  list, OR a `page-targets` target dict — its `claim_ids` are used) → the
  page's `deps:` frontmatter list → if nothing is given, deps are
  auto-derived from the body's `[[claim_*]]` links at their current
  versions. `--base-revision <n>` fails fast instead of exit 4 if another
  writer committed first.
- Typed dep kinds: `claim_X@N`, `decision_Y@N` (current version), or a bare
  `wiki/<kind>/<slug>.md` path that exists on disk. Unknown ids, version
  mismatches, and rejected/superseded claims are rejected.
- `write-page` enforces: kind-matched `type`, non-empty `subject` whose
  slug matches the artifact (collision-suffixed slugs still match),
  non-empty `title`, ISO-8601 `created` on new pages, `linked ⊆ deps`,
  `disputed: true` when a dep claim is disputed (empty deps only for
  `type: question`). Markdown links in the body that resolve to `.md` files
  under `wiki/` are validated and auto-appended to `deps`; unresolved
  bundle-internal links are rejected. It rejects reserved artifacts
  (`wiki/index.md`, `wiki/overview.md`, `wiki/review.md`, `wiki/log.md`,
  `wiki/decisions/*`, any `*/index.md`).
- On success the deps REPLACE any previous registration for the artifact in
  `.state/dependencies.json` and the stale flag clears in the same
  transaction. Exit 4 → re-run `page-targets`, re-author, commit with the
  fresh revision.
- Inspect before refreshing: `wiki.py page-show --artifact wiki/concepts/attribution.md`
  → `{artifact, front, body, claim_details, stale, deps_registered}`.
- After the last page: `wiki.py build-pages`, then `wiki.py verify` (must be
  `"ok": true`). `overview.md`, `wiki/review.md`, `wiki/changes/index.md`,
  and the section indexes are script-generated — never author them with
  `write-page`.

## query

Read-only. For "why do we…", "what does X use", "what changed", historical:

```bash
wiki.py query-prepare --question "what is the current attribution window?"
wiki.py query-prepare --question "what was it in August?" --as-of 2026-08-15
```

The output also carries `superseded[]` — old→new replacements for claims
superseded since the question's scope — and a `stale_knowledge` entry in
`gaps` when cached knowledge no longer covers the question's core. Surface
both; never answer from superseded memory.


Answer contract:

1. **Answer** — the direct result.
2. **Evidence** — cite claim ids and source origins from `matches[].evidence`.
3. **Scope** — note `scope` (e.g. production vs sandbox) whenever present.
4. **Historical applicability** — if `as_of` or superseded claims are involved,
   state what held when.
5. **Unresolved conflicts** — if `conflicts` is non-empty, present both sides.
6. **Gaps** — if `matches` is empty or `gaps` covers the core of the question:
   "I don't have enough project evidence to answer this reliably," then state
   what is known, what is missing, and what evidence would resolve it.

## subjects

Read-only subject census for coverage checks and gap hunting:

```bash
wiki.py subjects                # all subjects
wiki.py subjects --prefix svc.  # one subject prefix
```

→ `{"subjects": [{subject, predicates[], statuses: {<status>: count},
claim_count, source_count, stale_pages[]}]}`, sorted by subject, capped at
100.

Use it to spot claim-heavy subjects with no page, single-origin subjects
that stay claim-only, and pages gone stale per subject.

## topics

Read-only census of the canonical topic registry (`.llm-wiki/topics.json`),
plus an idempotent migration for wikis that predate the registry:

```bash
wiki.py topics            # registry census
wiki.py topics --backfill # register existing claim subjects (idempotent)
```

→ `{"version": N, "topics": [{subject, kind, status, title, claim_count,
declared_by, auto_registered}]}`, sorted by subject. `status` is `resolved`
or `conflict` (declared with differing kinds — resolve via
`review act set_topic_kind`); `declared_by` is a count of declaring sources.

`--backfill` writes `{topics_registered, warnings}` and never overwrites an
existing entry. For each unregistered claim subject it prefers the kind of
an existing page dir (`wiki/<kind>/<slug>.md` already on disk), falling
back to `page-kinds.json` routing (default `concept`); entries are stamped
`auto_registered: true` with `declared_by: backfill`.

## record-decision

Mints a canonical decision record — the apply step for `decision`-kind
topics listed in the receipt's `decision_topics[]`:

```bash
wiki.py record-decision --file decision.json
```

Input: `{title, status, decided_on, claims[], evidence[], body?, scope?}` —
no `id`/`recorded_at`/`version` (engine stamps them). Every `claims[]` id
must exist and be non-terminal. Every `evidence[]` item requires
`source_id` (must resolve to an ingested source), `source_version`, and a
`locator` with a `type` (same enum as claim evidence: `heading`,
`line_range`, …).

## context

Generate a bounded pack for a task; never dump the wiki.

```bash
wiki.py context-pack --task "implement order attribution fix" --budget 6000
wiki.py context-pack --resume
wiki.py context-pack --changes-since <context_id>
wiki.py context-pack --task "..." --budget 6000 --print-pack   # prints the pack text
```

Use `--print-pack` when the agent itself should consume the pack; use the JSON
output when reporting to the user. The receipt (`.llm-wiki/context/<id>.receipt.json`)
records exactly which claims/decisions/pages were included.

## review

Present open review items to the human; record their decision.

```bash
wiki.py review list                     # open items (default status)
wiki.py review list --kind possible_contradiction
wiki.py review list --status deferred   # or --all for open+deferred+resolved
wiki.py review show --item review_...
wiki.py review act --item review_... --action accept --params params.json
```

Actions: `accept reject merge mark_duplicate mark_authoritative
mark_superseded set_scope set_validity defer reopen set_topic_kind`.
`--params` is a JSON FILE path whose contents are e.g.
`{"claim_id": "claim_...", "scope": {"environment": "staging"}}` or
`{"claim_id": "...", "target": "claim_..."}` for mark_superseded.

- `defer` parks an item as deferred (`status: deferred`, `deferred_at` set,
  never `resolved_at`); it stays in the queue and remains discoverable via
  `review list --status deferred` / `--all` and in the derived surfaces
  (`wiki/review.md`, overview, `graph-data` review flags).
- `reopen` returns a deferred item to `open` (clears `deferred_at`, appends
  to the item's history). Receipt shape is unchanged:
  `{"item": ..., "run_id": ...}`.
- `set_topic_kind` resolves a `topic_kind_conflict` item. The params file
  carries `{"subject": "...", "kind": "..."}` — the subject must appear in
  the item's `affected[]`, and `kind` must be one of the kinds declared for
  it (`entry.kind` or `alternate_kinds[]`). It sets the topic's kind, marks
  it `resolved`, and marks pages stale in the old kind dir when it changes.
- Loop: `review list` → `review show` (present BOTH sides) → `review act` →
  `wiki.py build-pages` if a claim changed → `wiki.py verify` green.

## doctor

Static + semantic diagnostics. Run when something looks wrong, or before reporting.

```bash
wiki.py doctor
```

Findings have `severity` (error/warn/info), `code`, `detail`, `fix`.
Categories: schema validity and broken references of canonical objects;
derived-dependency integrity (unknown dep namespaces, stale/malformed dep
versions, orphan edges, dependency cycles); page integrity (subject↔slug
mismatch, slug collisions, disputed deps without labeling, duplicate
page-target artifacts, ambiguous kind routing); interrupted commits
(receipt/revision disagreement, leftover `.state/journal.json` or
`.state/.backup-<rev>/` dirs — recover deliberately, never auto-delete
canonical data); `staging_leftover`; semantic debt (unsupported claims,
disputes, contradictions, stale derived artifacts and context packs, low
extraction coverage); and topic-registry debt — `topic_kind_conflict`
(resolve via `review act set_topic_kind`), `topic_unclaimed` (declared
topic with zero claims), `topics_auto_registered` (kind inferred — declare
kinds explicitly in topics.jsonl), and `topic_page_kind_mismatch` (page
lives under the wrong kind dir). Fix errors first (usually broken
references from out-of-band edits). Report unresolved semantic debt to the
user — do not auto-resolve.

## graph-data

Read-only. Emits the knowledge-graph payload consumed by the `wiki-visualize`
skill (React/Three.js explorer). Never mutates the wiki.

```bash
wiki.py graph-data                    # JSON to stdout
wiki.py graph-data --output out.json  # write file instead
```

Output shape: `{project, nodes, links, claim_count, source_count,
decision_count, page_count, generated_at, warnings, meta}` — `meta.review`
summarizes the queue (`{open, deferred, total}`) and `meta.generated_at`
echoes the build time.

- `nodes[]`: `{id, kind: entity|source|decision|page, key, label, wtype,
  claim_count, size, summary, claims, sources}`; source nodes also carry
  `title`/`origin`. Entity nodes group claims by two-segment subject prefix;
  `label` strips the project prefix.
- Page nodes cover authored `wiki/{sources,concepts,entities,procedures,
  questions,changes}/*.md` (not `index.md`); `key`/`path` are the artifact
  rel path, `label` is the frontmatter `title` else the filename slug,
  `wtype` is `page_<singular>` (`page_concept`, `page_entity`,
  `page_procedure`, `page_question`, `page_source`, `page_change`), and
  `summary` carries `type`, `stale` (from frontmatter) and `title` when
  present. Pages are assigned contiguous ids after decisions, sorted by
  artifact path.
- Edges derive from non-superseded claims only; superseded claims remain in
  `claims[]` with their `status` for the detail panel. Claim nodes carry
  review flags (`review_open`, `disputed`, `superseded`, `stale`, `orphan`)
  so surfaces can show unresolved debt at a glance.
- Optional `.llm-wiki/graph-types.json` maps the second subject segment to a
  `wtype` (`{"pm97": "ticket", "sql": "migration"}`); otherwise
  `pm-\d+/stc-\d+` → `ticket`, else `domain`. A malformed file produces a
  `warnings` entry, not an error.
- Deterministic: identical wiki state → identical output except
  `generated_at`. Exit 3 if the wiki is not initialized.
