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
WIKI=$(ls "$SKILL_DIR/../llm-wiki/scripts/wiki.py" ~/.omp/agent/skills/llm-wiki/scripts/wiki.py ~/.agents/skills/llm-wiki/scripts/wiki.py ~/.cellockai/skills/llm-wiki/scripts/wiki.py "$PWD/../llm-wiki/scripts/wiki.py" 2>/dev/null | head -1)
[ -f "$WIKI" ] || echo "wiki.py not found — set WIKI=/path/to/llm-wiki/scripts/wiki.py manually"
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

The app fetches `graph-data.json` at runtime and computes the layout
client-side. Rewrite the file on every invocation BEFORE serving or
building — stale data is the common failure. The dev server picks the file
up on page reload; for `dist/` you must rebuild after regenerating (see
below).

## Serve

```bash
cd .llm-wiki/graph-ui
npm run dev          # → http://localhost:5173 (auto-port if busy)
# or static — regenerate public/graph-data.json FIRST, then:
npm run build && npx serve dist
```

`npm run build` copies `public/` into `dist/`, so a static bundle ships the
graph-data.json that existed at build time — always regenerate it before
building. Open the URL for the user and report it. Empty graph → likely no
claims yet: check `wiki.py status` `counts.claims`.

## Node types

`wtype` derives from the second subject segment: `.llm-wiki/graph-types.json`
(`{"pm97": "ticket", "sql": "migration"}`) overrides; else segments matching
`^(?:stc|pm)-?\d+$` (case-insensitive, dash optional) → `ticket`, everything
else `domain`. Sources/decisions are fixed kinds.

## Hard rules

- Never edit `.llm-wiki` canonical files from this skill — read-only.
- Regenerate `graph-data.json` on every invocation; stale data is the common
  failure. If the UI shows old nodes, refresh the page (browser cache) after
  regenerating.
