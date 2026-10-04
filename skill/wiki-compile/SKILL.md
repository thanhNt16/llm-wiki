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
WIKI=$(ls ~/.omp/agent/skills/llm-wiki/scripts/wiki.py ~/.agents/skills/llm-wiki/scripts/wiki.py ~/.cellockai/skills/llm-wiki/scripts/wiki.py 2>/dev/null | head -1)
python3 "$WIKI" compile        # → pending[] with content_path + candidates_path
# read each content_path, write shorthand JSONL to candidates_path:
#   {"subject":"a.b","predicate":"p","value":7,"locator":"h:Heading","authority":"manual"}
python3 "$WIKI" compile --resume   # → auto counts + needs_review rows (usually empty)
# if needs_review: write classifications.json (only those rows), then:
python3 "$WIKI" compile --resume   # → receipt + verify.ok
```

### Shorthand

Agent fields: `subject`, `predicate`, `value`, `locator`, `authority`, and
optional `scope`, `valid_from`, `supersedes`, `evidence`. Engine-stamped fields:
`id`, `root_origin`, source identity/version, normalized locator and authority,
and candidate status/timestamps. Locator prefixes: `h:` (heading), `l:` (line
range), `s:` (section). Authority names:
`manual|doc|config|decision|adr|code|verified|inferred`; `name:source` provides
an explicit authority source.

Runs live under `.llm-wiki/.state/staging/<run_id>/`, with
`candidates.jsonl`, `classifications.json`, and optional `report.json`. The
engine auto-classifies UNRELATED, DUPLICATE, and CORROBORATION; only
`needs_review` rows require agent classifications. Low-level commands remain
available when needed.

After the final `compile --resume` lands:

1. Synthesis — run `python3 "$WIKI" page-targets`; for EVERY entry in
   `targets[]`, author or refresh the page per the `wiki-write` skill. Use
   `target.artifact` and `target.claim_ids` verbatim — never hardcode
   `wiki/concepts/<slug>.md`; the kind router decides the directory.
   Commit each page with
   `python3 "$WIKI" write-page --artifact <target.artifact> --deps '<claim_ids comma-joined>'`
   (e.g. `--deps 'claim_abc123@3,claim_def456@1'`). A later
   `page-targets --stale-only` pass re-lists anything that goes stale.
2. `python3 "$WIKI" build-pages` — rebuilds index/decision stubs and clears stale flags.
3. `python3 "$WIKI" verify` — must report `"ok": true` before you finish. Fix every
   error (unresolved links, stale pages) and re-verify.

Report the run receipt: changes counts, conflicts (new review items).

## Hard rules

- **Evidence ≠ Claim ≠ Decision.** Claims are structured interpretations with
  citations; decisions are what the project deliberately chose — never
  inferred from repeated claims.
- Never write claim files by hand; every mutation goes through `wiki.py`.
- Exit code 4 (stale revision): re-run `compile-plan`/`status`, re-apply your
  semantic decision, commit with the fresh revision.
- Exit codes: 3 validation, 4 stale revision, 5 locked.

Full contract: `references/COMMANDS.md#compile` inside the `llm-wiki` skill.
