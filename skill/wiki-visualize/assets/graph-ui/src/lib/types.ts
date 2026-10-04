/* Graph data types matching the C layout3d.c JSON output */

export interface GraphNode {
  id: number;
  x: number;
  y: number;
  z: number;
  label: string;
  name: string;
  file_path?: string;
  qualified_name?: string;
  start_line?: number;
  end_line?: number;
  size: number;
  color: string;
  /* Dead-code classification from the backend layout (layout3d.c). */
  status?: NodeStatus;
  in_calls?: number;
  /* wiki-graph fork: knowledge payload */
  wtype?: "ticket" | "domain" | "agent" | "migration" | "qa" | "source" | "decision"
    | "page_concept" | "page_entity" | "page_procedure" | "page_question"
    | "page_source" | "page_change";
  summary?: { p: string; v: unknown }[];
  claims?: WikiClaim[];
  sources?: string[];
  claim_count?: number;
  flags?: NodeFlags;
  review?: ClaimReviewItem[];
  title?: string;
  origin?: string;
}

/* Review/audit annotations from graphdata.py — absent in older payloads. */
export interface NodeFlags {
  review_open?: boolean;
  disputed?: boolean;
  superseded?: boolean;
  stale?: boolean;
  orphan?: boolean;
}

export interface ClaimReviewItem {
  id: string;
  kind: string;
  risk: string;
  affected: string[];
  status: string;
}

export interface ReviewCounts {
  open: number;
  deferred: number;
  total: number;
}


export interface WikiClaim {
  id: string;
  s: string;         // subject
  p: string;         // predicate
  v: unknown;        // value
  scope?: Record<string, unknown>;
  vf?: string | null;
  vt?: string | null;
  st?: string;
  sup?: string[];       // supersedes (ids this claim replaced)
  sup_by?: string | null; // superseded_by
  auth?: string;
  src?: string;
  loc?: string | null;
}

export type NodeStatus =
  | "dead"
  | "single"
  | "entry"
  | "test"
  | "exported"
  | "normal"
  | "structural";

/* Git remote metadata for building GitHub deep-links (/api/repo-info). */
export interface RepoInfo {
  root_path: string;
  branch: string;
  remote_url: string;
  web_base: string; /* e.g. github.com/<org>/<repo> */
  blob_base: string; /* e.g. github.com/<org>/<repo>/blob/<branch> */
}

export interface GraphEdge {
  source: number;
  target: number;
  type: string;
}

export interface LinkedProject {
  project: string;
  nodes: GraphNode[];
  edges: GraphEdge[];
  offset: { x: number; y: number; z: number };
  cross_edges: GraphEdge[];
}

/* Missed-graph skeleton (#963): the file structure of files the indexer
 * could not fully cover, laid out as a satellite cluster beside the code
 * galaxy (server-computed offset, same shape as LinkedProject's). */
export interface MissedGraph {
  nodes: GraphNode[];
  edges: GraphEdge[];
  offset: { x: number; y: number; z: number };
}

export interface GraphData {
  nodes: GraphNode[];
  edges: GraphEdge[];
  total_nodes: number;
  linked_projects?: LinkedProject[];
  missed_graph?: MissedGraph;
  meta?: {
    project: string;
    claim_count: number;
    source_count: number;
    decision_count?: number;
    generated_at?: string;
    review?: ReviewCounts;
  };
}

export interface Project {
  name: string;
  root_path: string;
  indexed_at: string;
}

export interface SchemaInfo {
  node_labels: { label: string; count: number }[];
  edge_types: { type: string; count: number }[];
  total_nodes: number;
  total_edges: number;
}

export type TabId = "graph" | "stats" | "control";

export interface ProcessInfo {
  pid: number;
  cpu: number;
  rss_mb: number;
  elapsed: string;
  command: string;
  is_self: boolean;
}
