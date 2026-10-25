# graph-ui — llm-wiki knowledge graph explorer

Fork of `codebase-memory`'s Three.js graph UI, rewired to render `.llm-wiki`
knowledge: entity/source/decision nodes plus authored page nodes
(concept/entity/procedure/question/change/source, colored per page type),
with `evidence`, `mentions`, `contains`, `documents`, and `depends_on`
edges; claims listed per node with trace-back-to-source in the detail panel.

Static — no backend, no build-time data generator. The app fetches
`./graph-data.json` at runtime (`src/lib/layout.ts#loadWikiData`) and
computes the force layout client-side with `d3-force-3d`
(`computeLayout`/`payloadToGraphData` in `src/lib/layout.ts`). Refreshing
data means rewriting that JSON file — never editing `src/`.

## Data

`graph-data.json` is produced by the llm-wiki engine, not by this repo:

```bash
python3 "$WIKI" graph-data > public/graph-data.json
```

Payload shape: `{project, nodes, links, claim_count, source_count,
decision_count, generated_at, warnings, meta}` — see `WikiPayload` in
`src/lib/layout.ts` and `references/COMMANDS.md#graph-data` in the
llm-wiki skill. A missing file surfaces a readable error in the UI telling
you to regenerate.

## Node types

`wtype` → label/color palette (the `LABEL`/`TYPE_COLORS` tables in
`src/lib/layout.ts`): `ticket` (Ticket), `domain` (Entity), `agent`
(AgentSurface), `migration` (Migration), `qa` (QA), `source` (Doc),
`decision` (Decision), and page kinds `page_concept`, `page_entity`,
`page_procedure`, `page_question`, `page_source`, `page_change`
(Page·Concept … Page·Change). Colors live in `TYPE_COLORS`; change them
there, not in a data file.

## Dev

```bash
npm install
npm run dev          # vite → http://localhost:5173
```

The dev server serves `public/graph-data.json` directly — rewrite it and
reload the page for fresh data.

## Build / serve statically

```bash
python3 "$WIKI" graph-data > public/graph-data.json   # refresh data first
npm run build                                        # vite copies public/ → dist/
npx serve dist
```

Data is baked into `dist/` at build time; regenerate `public/graph-data.json`
and rebuild to ship newer data.

## Regeneration checklist

1. `python3 "$WIKI" graph-data > public/graph-data.json`
2. Dev server: reload the page. Static: `npm run build`, then serve `dist/`.
