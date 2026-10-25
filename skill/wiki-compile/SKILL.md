---
name: wiki-compile
description: Use when asked to compile, extract, or reconcile ingested llm-wiki evidence into claims and knowledge, or when the user says wiki-compile.
---

# wiki-compile — evidence → claims

Converts pending evidence into accepted knowledge. Three phases; the engine
handles deterministic transitions while you provide semantic candidate data
and classifications when needed.

## Engine

All state changes go through the deterministic CLI shipped with the `llm-wiki`
skill (installed alongside this one). Resolve it once, then run from the
project root:

```bash
WIKI=$(ls "$SKILL_DIR/../llm-wiki/scripts/wiki.py" ~/.omp/agent/skills/llm-wiki/scripts/wiki.py ~/.agents/skills/llm-wiki/scripts/wiki.py ~/.cellockai/skills/llm-wiki/scripts/wiki.py "$PWD/../llm-wiki/scripts/wiki.py" 2>/dev/null | head -1)
[ -f "$WIKI" ] || echo "wiki.py not found — set WIKI=/path/to/llm-wiki/scripts/wiki.py manually"
python3 "$WIKI" compile        # → pending[] with content_path + topics_path + candidates_path
# per pending entry:
#  1) write topics.jsonl (one topic/line) BEFORE candidates:
#     {"subject":"lake.s3","kind":"entity","title":"Amazon S3","aliases":["S3"]}
#     kinds: concept|entity|procedure|question|change|decision
#  2) read each content_path, write shorthand JSONL to candidates_path:
#     {"subject":"a.b","predicate":"p","value":7,"locator":"h:Heading","authority":"manual"}
python3 "$WIKI" compile --resume   # → auto counts + needs_review rows (usually empty)
# if needs_review: write classifications.json (only those rows), then:
python3 "$WIKI" compile --resume   # → receipt + verify.ok
```

### Shorthand

Agent fields: `subject`, `predicate`, `value`, `locator`, `authority`, and
optional `scope`, `valid_from`, `supersedes`, `evidence`. Engine-stamped fields:
`id`, `root_origin`, source identity/version, normalized locator and authority,
and candidate status/timestamps. Locator prefixes: `h:` (heading), `l:` (line
range), `s:` (free-form text snippet; normalizes to locator type `text`). Authority names:
`manual|doc|config|decision|adr|code|verified|inferred`; `name:source` provides
an explicit authority source.

Runs live under `.llm-wiki/.state/staging/<run_id>/`, with
`topics.jsonl`, `candidates.jsonl`, `classifications.json`, and optional
`report.json`. The engine auto-classifies UNRELATED, DUPLICATE, and
CORROBORATION; only `needs_review` rows require agent classifications.
Low-level commands remain available when needed.

After the final `compile --resume` lands:

0. Topics — the receipt's `topics` block lists `decision_topics[]` (record each
   with `wiki.py record-decision --file decision.json`), `unclaimed[]`
   (declared but no claims — extract more or accept), `auto_registered[]`,
   `kind_conflicts[]` (resolve via `review act set_topic_kind` on the
   `topic_kind_conflict` item), and `no_topics_declared` warnings.
1. Plan — run `python3 "$WIKI" page-targets` and list EVERY `targets[]`
   entry with a one-line justification BEFORE authoring; write the plan,
   then write the pages.
2. Synthesis — for EVERY entry in the `targets[]` list you just planned,
   author or refresh the page per the `wiki-write` skill. Use
   `target.artifact` and `target.claim_ids` verbatim — never hardcode
   `wiki/concepts/<slug>.md`; the kind router decides the directory.
   Write the page markdown to a file (or stdin / `--text`), then commit with
   `python3 "$WIKI" write-page --file page.md --artifact <target.artifact> --deps '<claim_ids comma-joined>'`
   (e.g. `--deps 'claim_abc123@3,claim_def456@1'`). A later
   `page-targets --stale-only` pass re-lists anything that goes stale.
3. `python3 "$WIKI" build-pages` — rebuilds index/decision stubs and clears stale flags.
4. `python3 "$WIKI" verify` — must report `"ok": true` before you finish. Fix every
   error (unresolved links, stale pages) and re-verify.

Report the run receipt: changes counts, conflicts (new review items).

## Hard rules

- **Evidence ≠ Claim ≠ Decision.** Claims are structured interpretations with
  citations; decisions are what the project deliberately chose — never
  inferred from repeated claims.
- Never write claim files by hand; every mutation goes through `wiki.py`.
- Exit code 4 (stale revision): re-run `compile`/`status`, re-apply your
  semantic decision, commit with the fresh revision.
- Exit codes: 3 validation, 4 stale revision, 5 locked.

Full contract: `references/COMMANDS.md#compile` inside the `llm-wiki` skill.
