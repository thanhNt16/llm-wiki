---
name: wiki-visualize
description: Use when asked to visualize, explore, or render the llm-wiki knowledge graph in a browser — entities, sources, decisions, authored page nodes (concepts/entities/procedures/questions/changes/sources, colored by page type) and claim detail — or when the user says wiki-visualize.
---

# wiki-visualize — knowledge-graph explorer

Renders `.llm-wiki` as an interactive 3D force graph (React + Three.js,
forked from codebase-memory's UI): entity/source/decision nodes plus authored
wiki-page nodes (concepts/entities/procedures/questions/changes/sources,
colored per page type), evidence / contains / mentions / decision /
documents / depends_on edges, claim detail panel, label + edge filters,
stats overview.

## Prerequisites

- `node >= 18` and `npm` — check `command -v npm` first; if missing, tell the
  user and stop. First `npm install` needs network.
- An initialized `.llm-wiki` (run `wiki-init` / `wiki-ingest` /
  `wiki-compile` first if the graph is empty).

## Engine

Data comes from the deterministic CLI in the `llm-wiki` skill:

```bash
WIKI=$(ls ~/.omp/agent/skills/llm-wiki/scripts/wiki.py ~/.agents/skills/llm-wiki/scripts/wiki.py ~/.cellockai/skills/llm-wiki/scripts/wiki.py 2>/dev/null | head -1)
python3 "$WIKI" graph-data          # one JSON object on stdout
```

Full payload contract: `references/COMMANDS.md#graph-data` inside the
`llm-wiki` skill.

## First run — copy the app

`$SKILL_DIR` is the directory containing this SKILL.md. The app template
lives in `$SKILL_DIR/assets/graph-ui/`.

```bash
if [ ! -d .llm-wiki/graph-ui ]; then
  cp -R "$SKILL_DIR/assets/graph-ui" .llm-wiki/graph-ui
fi
cd .llm-wiki/graph-ui && npm install   # once; ~350 packages
```

Never overwrite an existing `.llm-wiki/graph-ui` — it may be customized.

## Every invocation — regenerate data

```bash
python3 "$WIKI" graph-data > .llm-wiki/graph-ui/public/graph-data.json
```

Data refreshes by rewriting this file — the app fetches it at runtime and
computes the layout client-side; no rebuild needed.

## Serve

```bash
cd .llm-wiki/graph-ui
npm run dev          # → http://localhost:5173/?tab=graph (auto-port if busy)
# or static:
npm run build && npx serve dist
```

Open the URL for the user and report it. Empty graph → likely no claims yet:
check `wiki.py status` `counts.claims`.

## Node types

`wtype` derives from the second subject segment: `.llm-wiki/graph-types.json`
(`{"pm97": "ticket", "sql": "migration"}`) overrides; else `pm-\d+`/`stc-\d+`
→ `ticket`, everything else `domain`. Sources/decisions are fixed kinds.

## Hard rules

- Never edit `.llm-wiki` canonical files from this skill — read-only.
- Regenerate `graph-data.json` on every invocation; stale data is the common
  failure. If the UI shows old nodes, refresh the page (browser cache) after
  regenerating.
