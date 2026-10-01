---
name: wiki-compile
description: Use when asked to compile, extract, or reconcile ingested llm-wiki evidence into claims and knowledge, or when the user says wiki-compile.
---

# wiki-compile — evidence → claims

Converts pending evidence into accepted knowledge. Four phases; you (the
agent) do the semantic parts between deterministic script calls.

## Engine

All state changes go through the deterministic CLI shipped with the `llm-wiki`
skill (installed alongside this one). Resolve it once, then run from the
project root:

```bash
WIKI=$(ls ~/.omp/agent/skills/llm-wiki/scripts/wiki.py ~/.agents/skills/llm-wiki/scripts/wiki.py ~/.cellockai/skills/llm-wiki/scripts/wiki.py 2>/dev/null | head -1)
python3 "$WIKI" compile-plan
```

For each pending entry, read the normalized content
(`.llm-wiki/sources/<id>/content.md`) — **one source at a time, never load the
whole wiki** — extract candidate claims per `references/EXTRACTION.md`, write
them to a JSON file, then:

```bash
python3 "$WIKI" stage-candidates --file candidates.json --source-id <id> --source-version <n> --report report.json
# → {"run_id": "run_..."}
python3 "$WIKI" reconcile-prepare --run <run_id>
```

For every comparison in the output, classify per
`references/RECONCILIATION.md` into UNRELATED / DUPLICATE / CORROBORATION /
CORRECTION / POLICY_CHANGE / SCOPE_DIFFERENCE / CONTRADICTION, write
classifications JSON:

```json
[{"index": 0, "relationship": "CORRECTION", "target_claim_id": "claim_...",
  "valid_from": "2026-09-01", "valid_to": null,
  "authority": {"type": "explicit_project_decision", "source": "ADR-019"}}]
```

```bash
python3 "$WIKI" reconcile-apply --run <run_id> --classifications classifications.json
```

The apply is a single atomic transaction: claims created/superseded, review
items appended, dependents invalidated, source marked compiled. Then:

1. Regenerate invalidated concept/procedure pages you authored (keep their
   frontmatter `deps` updated to the current claim versions).
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
