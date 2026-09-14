# CONTEXT-PACKS.md — bounded context assembly (PRD §18-22)

## When to use

- Before starting a task in a project with a wiki: build a task pack.
- Starting a new session: `--resume` gives the change briefing.
- User asks "what changed since last time": `--changes-since <context_id>`
  (or `--resume`, which diffs since the most recent receipt).

## Budgets (PRD §19)

Always pass an explicit `--budget` for task packs when the user gives one;
otherwise the configured default (6000) applies. The pack NEVER exceeds the
budget — sections are added in priority order and assembly stops at the cap:

1. critical constraints (accepted explicit decisions)
2. accepted decisions
3. directly relevant claims (token overlap with your task text — phrase the
   `--task` with domain words, not pronouns)
4. implementation references
5. unresolved conflicts
6. related concept pages
7. supplementary evidence

If a section shows `omitted: budget exhausted`, either accept the smaller pack
or raise the budget deliberately.

## Consuming a pack

```bash
wiki.py context-pack --task "fix order attribution edge case" --budget 6000 --print-pack
```

The printed Markdown is your working context. The receipt JSON next to the
pack records `claims` and `decisions` by `id@version` — cite those ids when
your work depends on them, so future corrections trace back to this moment.

## Session continuity

`--resume` output contains: operations since the last receipt, new/changed
claims, recently superseded assumptions (with old→new values), open review
items, and a snapshot. Lead your session summary with superseded assumptions —
they invalidate whatever you believed last session.

## Registration and invalidation

Each pack registers its dependency set. When someone later corrects a claim,
stale packs are detected by `wiki.py verify` / `wiki.py doctor`. Regenerate a
stale pack instead of trusting it.
