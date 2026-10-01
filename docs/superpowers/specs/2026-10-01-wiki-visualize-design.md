# wiki-visualize — graph UI for .llm-wiki

Date: 2026-10-01
Status: approved design (pending spec review)

## Goal

A new `wiki-visualize` skill that renders a project's `.llm-wiki` knowledge
graph in the browser: entities, sources, decisions, edges, claim detail —
ported from the `codebase-memory` Three.js graph UI (as already forked at
`ptt/.llm-wiki/graph-ui/`).

## Decisions (from clarifying Q&A)

- UI = **vendored React+Vite fork** of codebase-memory's graph UI (the ptt
  `graph-ui/` fork), shipped inside the skill at `assets/graph-ui/`.
- Data = **runtime JSON fetch** — the app fetches `public/graph-data.json`;
  no rebuild needed to refresh data.
- Node model = **entities + sources + decisions**; claims live in the detail
  panel (full fields: predicate, value, status, scope, validity, evidence,
  supersedes).
- Layout = **client-side d3-force-3d** inside the app (`useForceLayout` hook);
  `gen-graph.mjs` is dropped — the engine emits unpositioned data.

## 1. Engine: `wiki.py graph-data`

New module `skill/llm-wiki/scripts/wikicore/graphdata.py` and a `graph-data`
subcommand. Read-only — no transaction, no mutation; prints one JSON object.

```bash
wiki.py graph-data          # -> {"nodes":[...],"links":[...],...} on stdout
```

### Payload

```json
{
  "project": "<wiki.json project name>",
  "nodes": [
    {"id": "n0", "kind": "entity", "key": "ptt.pm97", "label": "pm97",
     "wtype": "ticket", "claim_count": 14,
     "summary": [{"p": "status", "v": "shipped"}],
     "claims": [{"id": "claim_…", "predicate": "…", "value": …,
                 "status": "accepted", "scope": {}, "valid_from": null,
                 "valid_to": null, "authority": "…",
                 "evidence": [{"source_id": "…", "version": 1,
                               "locator": {"type": "heading", "value": "…"}}],
                 "supersedes": []}],
     "sources": ["source_…"]},
    {"id": "s0", "kind": "source", "key": "source_…", "label": "HANDOVER.md",
     "wtype": "source", "title": "…", "origin": "…"},
    {"id": "d0", "kind": "decision", "key": "decision_…", "label": "…",
     "wtype": "decision"}
  ],
  "links": [{"source": "n0", "target": "s0", "type": "evidence", "w": 1}],
  "claim_count": 812, "source_count": 34, "decision_count": 5,
  "generated_at": "…Z"
}
```

### Derivation rules

- **Entities**: claims grouped by 2-segment subject prefix
  (`ptt.pm97.mentions` → `ptt.pm97`); label strips the project-name prefix
  (`wiki.json`'s `project`, or the first subject segment).
- **Sources**: every `sources/*/manifest.json` → a node (`title` or basename
  of `origin`). Entities link to the sources their claims cite
  (`evidence[].source_id`) and to `authority.source` when it names a known
  source id; unknown authority strings are ignored (no phantom nodes).
- **Decisions**: `decisions/*.json` → `wtype: "decision"` nodes linked to the
  entity whose subject matches `decision.subject` (if present).
- **Edges**: `evidence` (entity→source), `contains` (subject hierarchy:
  `a.b` contains `a.b.c`'s grouping), `mentions` (alias co-reference — ported
  from the fork: entity alias (2nd segment, len≥5, not stopword, ≥2 claims)
  matched ≥2 times in another entity's claim values).
- **wtype**: optional `.llm-wiki/graph-types.json` maps the second subject
  segment to a type (`{"pm97": "ticket", "sql": "migration", "qa": "qa",
  "agent_api": "agent"}`); unmapped → `"domain"`. No map file → all entities
  `"domain"`. Regex ticket heuristic (`stc|pm-\d+`) kept as fallback so ptt
  keeps working without a map.
- **Superseded claims** included in `claims` with their `status`; panel
  renders them struck/dimmed. Graph edges derive from non-superseded claims
  only.
- **Determinism**: sorted iteration everywhere; identical wiki state →
  identical JSON (byte-equal except `generated_at`).

## 2. Skill: `skill/wiki-visualize/`

```
SKILL.md                    — usage contract
assets/graph-ui/            — vendored fork (adapted)
```

Adaptations from `ptt/.llm-wiki/graph-ui/`:

- `src/hooks/useGraphData.ts` → fetch `public/graph-data.json` at runtime
  (loading/error states retained).
- `src/lib/layout.ts` (new) — `computeLayout(nodes, links)` running
  `d3-force-3d` (~400 ticks, the gen-graph force config: link distance
  evidence=26/else=44, charge source=-4/else=-14, x/y/z centering 0.05/0.05/
  0.08, collide `size*0.9+2`). Called in `useMemo` inside GraphTab.
- `gen-graph.mjs` deleted; `src/data/wikiGraph.ts` deleted;
  `WIKI_PROJECT`/`WIKI_CLAIM_COUNT`/`WIKI_SOURCE_COUNT` constants move into
  the JSON payload (`project`, `claim_count`, `source_count`).
- `vite.config.ts`: backend-proxy block removed (leftover from upstream);
  `base: "./"` so `dist/` works from `file://`-ish static serving too.
- `stats` tab keeps working off the same payload.
- Type fixups: `wtype` union gains `"decision"`; `GraphNode` gains
  `claim_count`, `title`, `origin` (source nodes).

### SKILL.md flow

```bash
# one-time per project
cp -R "$SKILL_DIR/assets/graph-ui" .llm-wiki/graph-ui
cd .llm-wiki/graph-ui && npm install        # requires network + node ≥ 18

# every visualize/refresh
python3 "$WIKI" graph-data > .llm-wiki/graph-ui/public/graph-data.json
cd .llm-wiki/graph-ui && npm run dev        # → http://localhost:5173/?tab=graph
# or: npm run build && npx serve dist
```

Skill instructs the agent to: resolve `$SKILL_DIR` from its own file, init-copy
only if absent (never overwrite a customized copy), regenerate JSON on every
invocation, prefer `npm run dev` unless the user wants a static `dist/`.

## 3. Error handling

| Case | Behavior |
|---|---|
| `graph-types.json` malformed | payload gains `"warnings": [...]`; invalid map → entities all `domain` |
| No node/npm at skill runtime | SKILL.md tells the agent to report the prerequisite and stop |
| `public/graph-data.json` missing in UI | useGraphData error state: "run `wiki.py graph-data`" |

## 4. Testing

Engine tests (`tests/test_graphdata.py`):

- claims → grouped entity nodes; label strips project prefix
- evidence edges: claim `evidence[].source_id` → source node exists + linked
- `contains` hierarchy edge: `a.b` / `a.b.c` grouping
- `graph-types.json` override → wtype applied; absent → domain
- superseded claims present in payload with status but produce no edges
- determinism: two runs → byte-equal minus `generated_at`
- empty wiki → valid empty payload
- exit 3 on uninitialized wiki

UI verification: one manual build+load check (`npm install && npm run build`,
load `dist/` or dev server, confirm nodes render and panel shows claims) —
recorded as smoke evidence in the report. No vitest additions (the fork's
existing tests — density/i18n — should still pass: `npm test`).

## 5. Non-goals

- No 2D/xyflow variant; no `graph.html` single-file mode (documented
  alternative in ptt, not shipped).
- No live wiki watching/auto-regeneration.
- No engine-side layout.
- No modification of compile/query/review paths.
