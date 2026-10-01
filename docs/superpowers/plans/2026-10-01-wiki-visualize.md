# Plan: wiki-visualize — graph UI skill + `wiki.py graph-data`

Spec: `docs/superpowers/specs/2026-10-01-wiki-visualize-design.md` (commit 1adb39d).
Source to port: `~/Desktop/cellock/ptt/.llm-wiki/graph-ui/` (React19 + Vite6 +
@react-three/fiber fork of codebase-memory's UI; verified fields below).

## Contracts

**`graph-data` stdout** — single JSON object:
```json
{"project": "<name>",
 "nodes": [{"id":0,"kind":"entity","key":"ptt.pm97","label":"pm97",
            "wtype":"ticket","claim_count":14,"size":18.7,
            "summary":[{"p":"status","v":"shipped"}],
            "claims":[{"id":"claim_…","subject":"…","predicate":"…","value":…,
                       "status":"accepted","scope":{},"valid_from":null,
                       "valid_to":null,"authority":"…",
                       "evidence":[{"source_id":"source_…","version":1,
                                    "locator":{"type":"heading","value":"…"}}],
                       "supersedes":[]}],
            "sources":["source_…","docs/HANDOVER.md"]}],
 "links": [{"source":0,"target":1,"type":"evidence","w":1}],
 "claim_count":N,"source_count":M,"decision_count":K,
 "generated_at":"…Z","warnings":[]}
```
Node `id` numeric 0-based; entity nodes first, then decision, then source
nodes; `links.source/target` are node ids. `sources` = source_ids present in
`sources/` + raw `authority.source` strings (pane shows both). `size` =
`6*sqrt(claim_count)` min 4 (entity) / 5 (source) / 7 (decision).

**kind**: `entity` | `source` | `decision`. **wtype** (entity only, colored):
`ticket|domain|agent|migration|qa` via optional `.llm-wiki/graph-types.json`
(`{"<segment>": "<type>"}`) else regex `stc|pm-?\d+`→ticket else `domain`;
source→`source`, decision→`decision`.

**Edges**: `evidence` (entity→source node, one per cited source_id),
`contains` (entity key `a.b` → `a.b.c`), `mentions` (alias co-ref heuristic:
2nd segment len≥5, not stopword, ≥2 claims, matched ≥2× in other entity's
claim values), `decision` (decision node→entity whose subject matches
`decision.subject` or `about`). Edges derive from non-superseded claims only;
superseded claims remain in `claims` with their status.

**App data path**: `useGraphData` fetches `./graph-data.json` (vite `base:
"./"`, file lives in `public/`); a `useForceLayout` hook computes x/y/z via
`d3-force-3d` (already a dep) with gen-graph's force params; GraphTab renders
the laid-out copy. `types.ts`: `wtype` += `"decision"`; nodes gain
`claim_count`,`title`,`origin`; `GraphData` gains `meta?: {project,
claim_count, source_count, generated_at}`.

## Tasks

### Engine

- **T1 `wikicore/graphdata.py` + `cmd_graph_data`**: read `claims/claim_*.json`,
  `decisions/*.json`, `sources/*/manifest.json`, `wiki.json` (project name),
  optional `graph-types.json`. Group claims by 2-seg key; build nodes/links per
  contract; `--output` flag optional (default stdout). Exit codes: 0 ok, 3
  TxnError (uninitialized). Register subparser in `wiki.py`; add to `EXIT_CODES`
  nothing new needed. Determinism: sort keys, sorted evidence, stable order.

- **T2 tests `tests/test_graphdata.py`**: entity grouping + label strip;
  evidence edge + source node; contains edge; mentions edge (fixture with
  alias collision); decision node + edge; wtype map override + regex fallback;
  superseded claims excluded from edges but present in claims; deterministic
  byte-equal output across two runs (modulo generated_at); empty wiki → valid
  empty payload; uninitialized → exit 3.

### UI port (`skill/wiki-visualize/assets/graph-ui/`)

- **T3 vendor**: `cp -R` ptt `graph-ui/` minus `node_modules/`, `dist/`,
  `src/data/`, `scripts/gen-graph.mjs`, `scripts/build-data.py`. Create
  `public/` with placeholder `graph-data.json` (`{"nodes":[],"links":[]}`).
  Strip `vite.config.ts` proxy/server block; set `base:"./"`.

- **T4 adapt data path**: rewrite `useGraphData` → `fetch("./graph-data.json")`
  (loading/error); `data/meta.json` type. `App.tsx` + `StatsTab.tsx`: replace
  `WIKI_*` imports with payload `meta` (fetch once at app level or share via
  context — simplest: export `useWikiMeta` from the hook file, backed by the
  same fetch promise cache).

- **T5 client layout**: `src/lib/layout.ts` — `computeLayout(nodes, links):
  GraphNode[]` running forceLink/forceManyBody/forceX/Y/Z/forceCollide with
  gen-graph params (link distance: evidence 26 else 44; charge: source -4
  else -14; centering .05/.05/.08; collide size*0.9+2; 400 ticks). Map payload
  nodes → `GraphNode` (id, label, name=label, qualified_name=key,
  file_path=key.split(".")[1] or key for sidebar grouping, size, color=
  TYPE_COLORS[wtype], status=wtype-specific like upstream STATUS map, in_calls
  =claim_count, summary/claims/sources passthrough). Hook `useForceLayout` in
  GraphTab: `useMemo` on data → replace nodes with positioned copies.
  NodeDetailPanel/NodeLabels/NodeTooltip/Sidebar untouched (consume GraphNode).

- **T6 edge-type + decision colors**: `EDGE_TYPE_COLORS` += decision/mentions/
  contains/evidence (already partially there? check colors.ts); `wtype` union
  += `"decision"`; label color map += decision.

### Skill

- **T7 `skill/wiki-visualize/SKILL.md`**: contract from spec §2 flow —
  resolve `$SKILL_DIR` (script dir of the skill), copy assets if
  `.llm-wiki/graph-ui` absent, `wiki.py graph-data > …/public/graph-data.json`,
  `npm install` once, `npm run dev` (5173) or `build`+serve. Prerequisites
  (node≥18, npm, network first time). Refresh instructions. Register in
  README.md skill table + any skill index/install.sh list if present.

### Verify + review

- **T8**: `python3 -m unittest discover -s tests` in `skill/llm-wiki/scripts`
  full pass; smoke: `graph-data` on a small fixture wiki → node counts match
  expected; UI: `npm install && npm run build` in a scratch copy + load
  (dev server or dist) — screenshot nodes render + detail panel claims.
- **T9**: task-reviewer on the branch diff; fix findings; final review; merge.

## Files

```
skill/llm-wiki/scripts/wikicore/graphdata.py   NEW
skill/llm-wiki/scripts/wiki.py                 +graph-data subcommand
skill/llm-wiki/scripts/tests/test_graphdata.py NEW
skill/llm-wiki/references/COMMANDS.md          +graph-data contract row
skill/wiki-visualize/SKILL.md                  NEW
skill/wiki-visualize/assets/graph-ui/…         vendored + adapted
install.sh                                     register skill dir if install
                                               copies skill/* (verify)
README.md                                      skill table row
```

## Risks / notes

- `npm install` on user's machine is the only network dependency; SKILL.md
  must state it and abort cleanly.
- `graph-types.json` deliberately minimal (segment→type map); richer typing
  deferred — PRD doesn't require it.
- Determinism: dict order + sorted keys; `generated_at` is the only
  nondeterministic field (tests strip it).
- Sidebar uses `file_path` as a path tree — set to the entity key with
  project prefix stripped, so entities nest under their second segment;
  source nodes get `file_path` = "sources/<title>" to keep them grouped.
- The fork uses label strings as filter keys — labels must be unique;
  collision (two entities same label) → suffix `key` in `name`? Keep label
  = full key when ambiguous (dedupe: if two entities strip to same label,
  keep full key for both).
