---
name: wiki-query
description: Use when asked a project question that llm-wiki memory should answer — "why do we…", "what does X use", "what changed", historical questions — or when the user says wiki-query.
---

# wiki-query — cited answers

Read-only Q&A over the project's evidence-backed knowledge. `query-prepare`
never writes.

## Engine

Queries go through the deterministic CLI shipped with the `llm-wiki` skill
(installed alongside this one). Resolve it once, then run from the project
root:

```bash
WIKI=$(ls ~/.omp/agent/skills/llm-wiki/scripts/wiki.py ~/.agents/skills/llm-wiki/scripts/wiki.py ~/.cellockai/skills/llm-wiki/scripts/wiki.py 2>/dev/null | head -1)
python3 "$WIKI" query-prepare --question "what is the current attribution window?"
python3 "$WIKI" query-prepare --question "what was it in August?" --as-of 2026-08-15
```

## Answer contract — include in prose

1. **Answer** — the direct result.
2. **Evidence** — cite claim ids and source origins from `matches[].evidence`.
3. **Scope** — note `scope` (e.g. production vs sandbox) whenever present.
4. **Historical applicability** — if `as_of` or superseded claims are involved,
   state what held when.
5. **Unresolved conflicts** — if `conflicts` is non-empty, present both sides.
6. **Gaps** — if `matches` is empty or `gaps` covers the core of the question:
   "I don't have enough project evidence to answer this reliably," then state
   what is known, what is missing, and what evidence would resolve it.

## Hard rules

- **Never answer a project question without `query-prepare` output and
  citations.** If the wiki cannot answer, say so — never fabricate project
  state from generic knowledge.
- Never load the whole wiki into context.

Full contract: `references/COMMANDS.md#query` inside the `llm-wiki` skill.
