import { useMemo, useState } from "react";
import { ScrollArea } from "@/components/ui/scroll-area";
import { colorForLabel } from "../lib/colors";
import type { GraphNode, GraphEdge, WikiClaim } from "../lib/types";

interface Connection {
  node: GraphNode;
  edgeType: string;
  direction: "inbound" | "outbound";
}

interface NodeDetailPanelProps {
  node: GraphNode;
  allNodes: GraphNode[];
  allEdges: GraphEdge[];
  project: string | null;
  repoInfo: unknown | null;
  canGoBack: boolean;
  onBack: () => void;
  onClose: () => void;
  onNavigate: (node: GraphNode) => void;
}

/* Predicates that carry headline information — rendered as the summary card */
const KEY_PREDICATES: Record<string, true> = {
  status: true, summary: true, title: true, verdict: true,
  feature: true, outcome: true, stale: true,
};

const STATUS_TONE: Record<string, { bg: string; fg: string }> = {
  accepted: { bg: "#22c55e22", fg: "#4ade80" },
  implemented: { bg: "#22c55e22", fg: "#4ade80" },
  passed: { bg: "#22c55e22", fg: "#4ade80" },
  done: { bg: "#22c55e22", fg: "#4ade80" },
  provisional: { bg: "#eab30822", fg: "#facc15" },
  draft: { bg: "#eab30822", fg: "#facc15" },
  open: { bg: "#eab30822", fg: "#facc15" },
  superseded: { bg: "#64748b22", fg: "#94a3b8" },
  deprecated: { bg: "#ef444422", fg: "#f87171" },
  failed: { bg: "#ef444422", fg: "#f87171" },
  blocked: { bg: "#ef444422", fg: "#f87171" },
};

const EDGE_ICON: Record<string, string> = {
  EVIDENCE: "◈",
  MENTIONS: "◎",
  CONTAINS: "▸",
};

/* Review flags from graphdata.py — badges rendered only when the payload
 * carries them (backward compat with older graph-data.json). */
const FLAGS: {
  key: keyof NonNullable<GraphNode["flags"]>;
  label: string;
  bg: string;
  fg: string;
}[] = [
  { key: "review_open", label: "open review", bg: "#eab30822", fg: "#facc15" },
  { key: "disputed", label: "disputed", bg: "#ef444422", fg: "#f87171" },
  { key: "superseded", label: "superseded", bg: "#64748b22", fg: "#94a3b8" },
  { key: "stale", label: "stale", bg: "#f9731622", fg: "#fb923c" },
  { key: "orphan", label: "orphan", bg: "#a855f722", fg: "#c084fc" },
];

function statusTone(status?: string): { bg: string; fg: string } {
  return STATUS_TONE[(status ?? "").toLowerCase()] ?? { bg: "#38bdf822", fg: "#7dd3fc" };
}

function claimValue(v: unknown): string {
  return typeof v === "object" ? JSON.stringify(v) : String(v ?? "");
}

/** Strip the redundant entity prefix from claim subjects (`ptt.stc37.foo` → `foo`). */
function shortSubject(node: GraphNode, claimSubject: string): string {
  const qn = node.qualified_name;
  if (qn && claimSubject.startsWith(qn + ".")) return claimSubject.slice(qn.length + 1);
  const last = claimSubject.split(".").pop() ?? claimSubject;
  return last;
}

export function NodeDetailPanel({
  node,
  allNodes,
  allEdges,
  canGoBack,
  onBack,
  onClose,
  onNavigate,
}: NodeDetailPanelProps) {
  const connections = useMemo(() => {
    const byId = new Map(allNodes.map((n) => [n.id, n]));
    const list: Connection[] = [];
    for (const edge of allEdges) {
      if (edge.source === node.id) {
        const t = byId.get(edge.target);
        if (t) list.push({ node: t, edgeType: edge.type, direction: "outbound" });
      } else if (edge.target === node.id) {
        const s = byId.get(edge.source);
        if (s) list.push({ node: s, edgeType: edge.type, direction: "inbound" });
      }
    }
    return list;
  }, [node, allNodes, allEdges]);

  const outbound = connections.filter((c) => c.direction === "outbound");
  const inbound = connections.filter((c) => c.direction === "inbound");
  const claims = node.claims ?? [];
  const superseded = claims.filter((c) => c.st === "superseded" || c.sup_by);
  const [copied, setCopied] = useState<string | null>(null);
  const copyCommand = (text: string, id: string) => {
    const done = () => {
      setCopied(id);
      setTimeout(() => setCopied((c) => (c === id ? null : c)), 1500);
    };
    navigator.clipboard?.writeText(text).then(done, () => {
      /* Clipboard permission denied — execCommand fallback (still works
       * in Electron views and older engines). */
      const ta = document.createElement("textarea");
      ta.value = text;
      document.body.appendChild(ta);
      ta.select();
      document.execCommand("copy");
      ta.remove();
      done();
    });
  };
  const sources = node.sources ?? [];

  /* Summary: headline claims deduped per predicate — latest wins */
  const { summaryClaims, otherClaims } = useMemo(() => {
    const latest = new Map<string, WikiClaim>();
    const rest: WikiClaim[] = [];
    for (const c of claims) {
      if (KEY_PREDICATES[c.p]) latest.set(c.p, c);
      else rest.push(c);
    }
    return { summaryClaims: [...latest.values()].slice(0, 4), otherClaims: rest };
  }, [claims]);

  const groupByType = (conns: Connection[]) => {
    const g = new Map<string, Connection[]>();
    for (const c of conns) g.set(c.edgeType, [...(g.get(c.edgeType) ?? []), c]);
    return [...g.entries()].sort((a, b) => b[1].length - a[1].length);
  };

  /* Sources that are doc nodes in the graph (for trace-back navigation) */
  const sourceNodes = useMemo(() => {
    const byName = new Map(
      allNodes.filter((n) => n.wtype === "source").map((n) => [n.qualified_name, n] as const),
    );
    return sources.map((s) => ({ path: s, node: byName.get(s) }));
  }, [sources, allNodes]);

  const isSource = node.wtype === "source";

  return (
    <div className="w-full bg-[#0b1920]/95 backdrop-blur-xl flex flex-col h-full min-h-0 overflow-hidden">
      {/* Header */}
      <div className="px-4 pt-3 pb-3 border-b border-border/30">
        <div className="flex items-start justify-between gap-1 mb-2">
          {canGoBack ? (
            <button
              onClick={onBack}
              className="shrink-0 flex items-center gap-1 mt-0.5 px-1.5 py-0.5 -ml-1.5 rounded-md text-[11px] font-medium text-foreground/35 hover:text-foreground/80 hover:bg-white/[0.06] transition-colors"
              title="Back to previous node"
            >
              <span className="text-[13px] leading-none">←</span> back
            </button>
          ) : (
            <span className="w-4" />
          )}
          <div className="min-w-0 flex-1">
            <div className="flex items-center gap-2 mb-1.5">
              <span
                className="w-2.5 h-2.5 rounded-full shrink-0"
                style={{ backgroundColor: colorForLabel(node.label) }}
              />
              <h3 className="text-[13px] font-semibold text-foreground truncate">{node.name}</h3>
            </div>
            <div className="flex items-center gap-1.5">
              <span
                className="inline-block px-2 py-0.5 rounded-md text-[10px] font-medium"
                style={{
                  backgroundColor: colorForLabel(node.label) + "18",
                  color: colorForLabel(node.label),
                }}
              >
                {node.wtype ?? node.label}
              </span>
              {summaryClaims.find((c) => c.p === "status") && (
                <span
                  className="inline-block px-2 py-0.5 rounded-md text-[10px] font-semibold"
                  style={{
                    backgroundColor: statusTone(claimValue(summaryClaims.find((c) => c.p === "status")!.v)).bg,
                    color: statusTone(claimValue(summaryClaims.find((c) => c.p === "status")!.v)).fg,
                  }}
                >
                  {claimValue(summaryClaims.find((c) => c.p === "status")!.v)}
                </span>
              )}
              {FLAGS.filter((f) => node.flags?.[f.key]).map((f) => (
                <span
                  key={f.key}
                  className="inline-block px-2 py-0.5 rounded-md text-[10px] font-semibold"
                  style={{ backgroundColor: f.bg, color: f.fg }}
                >
                  {f.label}
                </span>
              ))}
            </div>
          </div>
          <button
            onClick={onClose}
            className="text-foreground/20 hover:text-foreground/50 transition-colors text-[16px] leading-none p-1 shrink-0"
          >
            ×
          </button>
        </div>

        {node.qualified_name && node.qualified_name !== node.name && (
          <p className="text-[10.5px] text-foreground/25 font-mono mt-1.5 break-all leading-relaxed">
            {node.qualified_name}
          </p>
        )}

        {/* Stats */}
        <div className="flex gap-5 mt-3">
          {[
            { label: "Claims", value: claims.length, color: "text-primary" },
            { label: "Out", value: outbound.length, color: "text-cyan-400" },
            { label: "In", value: inbound.length, color: "text-accent" },
            { label: "Sources", value: sources.length, color: "text-foreground" },
          ].map((s) => (
            <div key={s.label}>
              <p className="text-[9px] text-foreground/25 uppercase tracking-widest">{s.label}</p>
              <p className={`text-[18px] font-semibold tabular-nums ${s.color}`}>{s.value}</p>
            </div>
          ))}
        </div>
      </div>

      <ScrollArea className="flex-1 min-h-0">
        <div className="px-4 py-3 space-y-4">
          {/* Review context — engine review annotations (contract: node.review) */}
          {node.review && node.review.length > 0 && (
            <div>
              <p className="text-[11px] font-medium text-foreground/40 mb-2">
                Review items <span className="text-foreground/15">({node.review.length})</span>
              </p>
              <div className="space-y-1.5">
                {node.review.map((r) => (
                  <div
                    key={r.id}
                    className="rounded-md border border-white/[0.05] bg-white/[0.02] px-2.5 py-1.5"
                  >
                    <div className="flex items-center gap-2">
                      <span className="text-[9px] uppercase tracking-widest text-foreground/35">{r.kind}</span>
                      <span
                        className="px-1 py-px rounded text-[8.5px] font-semibold"
                        style={{ backgroundColor: statusTone(r.status).bg, color: statusTone(r.status).fg }}
                      >
                        {r.status}
                      </span>
                      <span className="ml-auto text-[9.5px] font-mono text-foreground/25">{r.id}</span>
                    </div>
                    {r.risk && <p className="text-[10.5px] text-foreground/50 mt-1">risk: {r.risk}</p>}
                    {r.affected.length > 0 && (
                      <p className="text-[10px] text-foreground/35 font-mono break-all mt-0.5">
                        affects: {r.affected.join(", ")}
                      </p>
                    )}
                    <button
                      onClick={() => copyCommand(`wiki.py review show ${r.id}`, r.id)}
                      className="mt-1 text-[10px] font-mono text-[#2dd4bf]/80 hover:text-[#2dd4bf] hover:underline"
                      title="Copy command"
                    >
                      {copied === r.id ? "copied ✓" : `$ wiki.py review show ${r.id}`}
                    </button>
                  </div>
                ))}
              </div>
            </div>
          )}

          {/* Supersession chain — claims replaced by newer versions */}
          {superseded.length > 0 && (
            <div>
              <p className="text-[11px] font-medium text-foreground/40 mb-2">
                Supersession chain <span className="text-foreground/15">({superseded.length})</span>
              </p>
              <div className="space-y-1">
                {superseded.map((c) => (
                  <p key={c.id} className="text-[10.5px] font-mono text-foreground/40 break-all">
                    {c.id} <span className="text-foreground/20">→ superseded by</span>{" "}
                    <span className="text-[#94a3b8]">{c.sup_by ?? "(unresolved)"}</span>
                  </p>
                ))}
              </div>
            </div>
          )}

          {/* Summary card — headline claims */}
          {summaryClaims.length > 0 && (
            <div>
              <p className="text-[11px] font-medium text-foreground/40 mb-2">Summary</p>
              <div className="rounded-lg border border-primary/15 bg-primary/[0.05] px-3 py-2.5 space-y-2">
                {summaryClaims.map((c: WikiClaim) => (
                  <div key={c.id}>
                    <p className="text-[9px] uppercase tracking-widest text-primary/50 font-medium">
                      {c.p}
                    </p>
                    <p className="text-[11.5px] text-foreground/85 leading-snug break-words">
                      {claimValue(c.v)}
                    </p>
                  </div>
                ))}
              </div>
            </div>
          )}

          {/* All facts */}
          {otherClaims.length > 0 && (
            <div>
              <p className="text-[11px] font-medium text-foreground/40 mb-2">
                Facts <span className="text-foreground/15">({otherClaims.length})</span>
              </p>
              <div className="space-y-1.5">
                {otherClaims.map((c: WikiClaim) => {
                  const sub = shortSubject(node, c.s);
                  return (
                    <div
                      key={c.id}
                      className="rounded-md border border-white/[0.05] bg-white/[0.02] px-2.5 py-1.5"
                    >
                      <p className="text-[11px] leading-snug">
                        {sub !== node.name.split(".").pop() && (
                          <>
                            <span className="text-[#a78bfa]/80">{sub}</span>
                            <span className="text-foreground/25">.</span>
                          </>
                        )}
                        <span className="text-foreground/80 font-medium">{c.p}</span>
                        <span className="text-foreground/25"> = </span>
                        <span className="text-foreground/70 break-all">{claimValue(c.v)}</span>
                      </p>
                      <p className="text-[9.5px] text-foreground/25 mt-1 font-mono flex items-center gap-1 flex-wrap">
                        {c.st && (
                          <span
                            className="px-1 py-px rounded text-[8.5px] font-semibold"
                            style={{
                              backgroundColor: statusTone(c.st).bg,
                              color: statusTone(c.st).fg,
                            }}
                          >
                            {c.st}
                          </span>
                        )}
                        {c.auth && <span>{c.auth}</span>}
                        <button
                          className="text-[#2dd4bf]/80 hover:text-[#2dd4bf] hover:underline truncate max-w-[70%]"
                          onClick={() => {
                            const sn = sourceNodes.find((x) => x.path === c.src)?.node;
                            if (sn) onNavigate(sn);
                          }}
                        >
                          {c.src ? c.src.split("/").pop() : "?"}
                        </button>
                        {c.loc && <span className="text-foreground/15">§ {c.loc}</span>}
                        {c.vf && <span className="text-foreground/15">{c.vf}</span>}
                      </p>
                    </div>
                  );
                })}
              </div>
            </div>
          )}

          {/* Trace back to sources */}
          {sourceNodes.length > 0 && !isSource && (
            <div>
              <p className="text-[11px] font-medium text-foreground/40 mb-2">
                Evidence sources <span className="text-foreground/15">({sourceNodes.length})</span>
              </p>
              <div className="space-y-px">
                {sourceNodes.map(({ path, node: sn }) => (
                  <button
                    key={path}
                    onClick={() => sn && onNavigate(sn)}
                    disabled={!sn}
                    className="flex items-center gap-1.5 w-full text-left px-2 py-[4px] rounded-md hover:bg-white/[0.04] text-[11px] transition-colors group disabled:opacity-60"
                  >
                    <span className="text-foreground/15 text-[10px] group-hover:text-[#475569]">◈</span>
                    <span
                      className="w-[5px] h-[5px] rounded-full shrink-0"
                      style={{ backgroundColor: "#475569" }}
                    />
                    <span className="text-foreground/55 group-hover:text-foreground/80 truncate font-mono">
                      {path}
                    </span>
                  </button>
                ))}
              </div>
            </div>
          )}

          {/* Relations */}
          {outbound.length > 0 && (
            <ConnectionSection
              title="References"
              count={outbound.length}
              groups={groupByType(outbound)}
              onNavigate={onNavigate}
            />
          )}
          {inbound.length > 0 && (
            <ConnectionSection
              title="Referenced by"
              count={inbound.length}
              groups={groupByType(inbound)}
              onNavigate={onNavigate}
            />
          )}
          {claims.length === 0 && connections.length === 0 && sources.length === 0 && (
            <p className="text-[12px] text-foreground/20 text-center py-8">No details</p>
          )}
        </div>
      </ScrollArea>
    </div>
  );
}

function ConnectionSection({
  title,
  count,
  groups,
  onNavigate,
}: {
  title: string;
  count: number;
  groups: [string, Connection[]][];
  onNavigate: (n: GraphNode) => void;
}) {
  return (
    <div>
      <p className="text-[11px] font-medium text-foreground/40 mb-2">
        {title} <span className="text-foreground/15">({count})</span>
      </p>
      {groups.map(([type, conns]) => (
        <div key={type} className="mb-2">
          <p className="text-[9px] text-foreground/20 uppercase tracking-wider mb-1 font-medium">
            {EDGE_ICON[type] ?? "·"} {type.replace(/_/g, " ").toLowerCase()}
          </p>
          <div className="space-y-px">
            {conns.slice(0, 25).map((c, i) => (
              <button
                key={`${c.node.id}-${i}`}
                onClick={() => onNavigate(c.node)}
                className="flex items-center gap-1.5 w-full text-left px-2 py-[4px] rounded-md hover:bg-white/[0.04] text-[11px] transition-colors group"
              >
                <span
                  className="w-[5px] h-[5px] rounded-full shrink-0"
                  style={{ backgroundColor: colorForLabel(c.node.label) }}
                />
                <span className="text-foreground/55 group-hover:text-foreground/80 truncate">
                  {c.node.name}
                </span>
                <span className="text-foreground/10 ml-auto text-[10px] shrink-0">
                  {c.node.wtype ?? c.node.label}
                </span>
              </button>
            ))}
            {conns.length > 25 && (
              <p className="text-[10px] text-foreground/15 px-2 py-1">+{conns.length - 25} more</p>
            )}
          </div>
        </div>
      ))}
    </div>
  );
}
