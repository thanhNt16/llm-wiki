---
name: wiki-review
description: Use when asked to review, resolve, or act on llm-wiki conflicts, contradictions, or claim candidates, or when the user says wiki-review.
---

# wiki-review — human decisions on conflicts

Presents open review items to the human and records their decision. Review
actions are transactional and change canonical state — the human decides, the
script applies.

## Engine

All state changes go through the deterministic CLI shipped with the `llm-wiki`
skill (installed alongside this one). Resolve it once, then run from the
project root:

```bash
WIKI=$(ls ~/.omp/agent/skills/llm-wiki/scripts/wiki.py ~/.agents/skills/llm-wiki/scripts/wiki.py 2>/dev/null | head -1)
python3 "$WIKI" review list                     # open items
python3 "$WIKI" review list --kind possible_contradiction
python3 "$WIKI" review show --item review_...
python3 "$WIKI" review act --item review_... --action accept --params params.json
```

Actions: `accept reject merge mark_duplicate mark_authoritative
mark_superseded set_scope set_validity defer`. Params JSON e.g.
`{"claim_id": "claim_...", "scope": {"environment": "staging"}}` or
`{"claim_id": "...", "target": "claim_..."}` for mark_superseded.

After acting, run `wiki.py build-pages` if a claim changed, then
`wiki.py verify`.

## Hard rules

- Present both sides of a conflict; do not auto-resolve semantic debt.
- Never edit claim or review files by hand.
- Exit codes: 3 validation, 4 stale revision, 5 locked.

Full contract: `references/COMMANDS.md#review` inside the `llm-wiki` skill.
