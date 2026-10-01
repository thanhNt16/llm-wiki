/* Engine payload → GraphData mapping + client-side 3D force layout.
 * Data comes from `wiki.py graph-data` written to public/graph-data.json —
 * layout runs here (d3-force-3d, already a dep) instead of a build-time
 * generator, so refreshing data is a file rewrite, not a rebuild. */
import {
  forceSimulation,
  forceLink,
  forceManyBody,
  forceCollide,
  forceX,
  forceY,
  forceZ,
  type SimulationNodeDatum,
} from "d3-force-3d";
import type { GraphData, GraphEdge, GraphNode, WikiClaim } from "./types";

export interface WikiPayloadNode {
  id: number;
  kind: "entity" | "source" | "decision";
  key: string;
  label: string;
  wtype?: string;
  claim_count?: number;
  size?: number;
  summary?: { p: string; v: unknown }[];
  claims?: Record<string, unknown>[];
  sources?: string[];
  title?: string;
  origin?: string;
}

export interface WikiPayloadLink {
  source: number;
  target: number;
  type: string;
  w?: number;
}

export interface WikiPayload {
  project: string;
  nodes: WikiPayloadNode[];
  links: WikiPayloadLink[];
  claim_count: number;
  source_count: number;
  decision_count?: number;
  generated_at?: string;
  warnings?: string[];
}

const TYPE_COLORS: Record<string, string> = {
  ticket: "#5ea1ff",
  domain: "#a78bfa",
  agent: "#2dd4bf",
  migration: "#e5a954",
  qa: "#f472b6",
  source: "#475569",
  decision: "#fbbf24",
};

/* label (filter/color category) per wiki type — matches upstream gen-graph */
const LABEL: Record<string, string> = {
  ticket: "Ticket",
  domain: "Entity",
  agent: "AgentSurface",
  migration: "Migration",
  qa: "QA",
  source: "Doc",
  decision: "Decision",
};

const STATUS: Record<string, GraphNode["status"]> = {
  ticket: "normal",
  domain: "normal",
  agent: "entry",
  migration: "single",
  qa: "test",
  source: "structural",
  decision: "entry",
};

/* Engine emits full claim dicts; the detail panel consumes the compact
 * WikiClaim shape (build-data.py `slim` contract). */
function slimClaim(c: Record<string, unknown>): WikiClaim {
  const ev =
    (c.evidence as { source_id?: string; locator?: { value?: string } }[]) ??
    [];
  const auth =
    (c.authority as { type?: string; source?: string } | undefined) ?? {};
  return {
    id: String(c.id ?? ""),
    s: String(c.subject ?? ""),
    p: String(c.predicate ?? ""),
    v: c.value,
    scope: (c.scope as Record<string, unknown>) ?? {},
    vf: (c.valid_from as string | null) ?? null,
    vt: (c.valid_to as string | null) ?? null,
    st: c.status as string | undefined,
    auth: auth.type,
    src: auth.source,
    loc: ev[0]?.locator?.value ?? null,
  };
}

function filePathFor(n: WikiPayloadNode): string {
  if (n.kind === "source") return `sources/${n.title ?? n.key}`;
  if (n.kind === "decision") return `decisions/${n.key}`;
  /* entity keys look like "ptt.pm97" — strip the first segment so the
   * sidebar groups by module; "entities/" is a virtual root because the
   * sidebar renders only directory children, never root-level leaves. */
  return `entities/${n.key.split(".").slice(1).join("/") || n.key}`;
}

export function payloadToGraphData(payload: WikiPayload): GraphData {
  const nodes: GraphNode[] = payload.nodes.map((n, i) => {
    const wtype = (n.wtype ?? "domain") as GraphNode["wtype"];
    return {
      id: i,
      label: LABEL[wtype ?? ""] ?? "Entity",
      wtype,
      name: n.label,
      qualified_name: n.key,
      file_path: filePathFor(n),
      size: n.size ?? 4,
      color: TYPE_COLORS[wtype ?? ""] ?? "#94a3b8",
      status: STATUS[wtype ?? ""] ?? "normal",
      in_calls: n.claim_count ?? 0,
      claim_count: n.claim_count ?? 0,
      summary: n.summary,
      claims: (n.claims ?? []).map(slimClaim),
      sources: n.sources,
      title: n.title,
      origin: n.origin,
      x: 0,
      y: 0,
      z: 0,
    };
  });
  const edges: GraphEdge[] = payload.links
    .filter((l) => l.source < nodes.length && l.target < nodes.length)
    .map((l) => ({
      source: l.source,
      target: l.target,
      type: l.type.toUpperCase(),
    }));
  return {
    nodes,
    edges,
    total_nodes: nodes.length,
    meta: {
      project: payload.project,
      claim_count: payload.claim_count,
      source_count: payload.source_count,
      decision_count: payload.decision_count ?? 0,
      generated_at: payload.generated_at,
    },
  };
}

/* ~400 ticks of the upstream gen-graph force config, on the mapped nodes. */
export function computeLayout(data: GraphData): GraphData {
  type SimNode = GraphNode & SimulationNodeDatum;
  const nodes = data.nodes.map((n) => ({ ...n })) as SimNode[];
  const simLinks = data.edges.map((e) => ({ ...e }));
  const sim = forceSimulation(nodes, 3)
    .force(
      "link",
      forceLink(simLinks)
        .distance((l: { type: string }) => (l.type === "EVIDENCE" ? 26 : 44))
        .strength((l: { type: string }) => (l.type === "EVIDENCE" ? 0.5 : 0.8)),
    )
    .force(
      "charge",
      forceManyBody().strength((n: SimNode) =>
        n.wtype === "source" ? -4 : -14,
      ),
    )
    .force("x", forceX(0).strength(0.05))
    .force("y", forceY(0).strength(0.05))
    .force("z", forceZ(0).strength(0.08))
    .force("collide", forceCollide((n: SimNode) => n.size * 0.9 + 2));
  for (let i = 0; i < 400; i++) sim.tick();
  sim.stop();
  return { ...data, nodes };
}

/* Shared loader: fetch + map + layout once, cache the promise. Refresh by
 * reloading the page (data file changes between runs, not during a session). */
let cache: Promise<GraphData> | null = null;

export function loadWikiData(): Promise<GraphData> {
  if (!cache) {
    cache = fetch("./graph-data.json")
      .then((r) => {
        if (!r.ok) {
          throw new Error(
            `graph-data.json missing (${r.status}) — run \`wiki.py graph-data > .llm-wiki/graph-ui/public/graph-data.json\``,
          );
        }
        return r.json() as Promise<WikiPayload>;
      })
      .then((payload) => computeLayout(payloadToGraphData(payload)))
      .catch((e) => {
        cache = null; /* allow retry after fixing the data file */
        throw e;
      });
  }
  return cache;
}
