# graph-ui — llm-wiki knowledge graph explorer

Fork of `codebase-memory`'s Three.js graph UI, rewired to render `.llm-wiki`
knowledge: Ticket / Entity / AgentSurface / Migration / QA nodes with
`evidence`, `mentions`, `contains` edges; claims listed per node with
trace-back-to-source in the detail panel.

Static — no backend. All data baked into `src/data/wikiGraph.ts` (generated,
do not edit).

## Open

```bash
npx serve dist          # or: open dist/index.html via any static server
```

## Regenerate after new wiki data

```bash
# from repo root:
python3 .llm-wiki/graph-ui/scripts/build-data.py > /tmp/wiki-gdata.json
cd .llm-wiki/graph-ui
node --experimental-strip-types scripts/gen-graph.mjs < /tmp/wiki-gdata.json
npm install && npm run build
```

`build-data.py` reads `.llm-wiki/claims/claim_*.json` and produces the
entity/source/link payload. `gen-graph.mjs` runs a d3-force-3d layout and
emits the typed data module.

## Dev

```bash
npm install
npm run dev
```
