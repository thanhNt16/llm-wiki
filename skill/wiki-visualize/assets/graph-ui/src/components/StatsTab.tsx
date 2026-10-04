import { useEffect, useMemo, useState } from "react";
import { ScrollArea } from "@/components/ui/scroll-area";
import { colorForLabel } from "../lib/colors";
import { loadWikiData, refreshWikiData } from "../lib/layout";
import type { GraphData } from "../lib/types";

interface StatsTabProps {
  onSelectProject: (project: string) => void;
}

interface TrustHealth {
  statuses: Record<"accepted" | "provisional" | "disputed" | "rejected" | "superseded", number>;
  stale: number;
  flagged: boolean;
}

export function StatsTab({ onSelectProject }: StatsTabProps) {
  const [graph, setGraph] = useState<GraphData | null>(null);
  const [refreshing, setRefreshing] = useState(false);

  useEffect(() => {
    loadWikiData().then(setGraph).catch(() => {});
  }, []);

  const reload = () => {
    setRefreshing(true);
    refreshWikiData()
      .then(setGraph)
      .catch(() => {})
      .finally(() => setRefreshing(false));
  };

  const { labelCounts, edgeCounts, topEntities, trust } = useMemo(() => {
    const lc = new Map<string, number>();
    const ec = new Map<string, number>();
    if (!graph)
      return { labelCounts: [], edgeCounts: [], topEntities: [], trust: null };
    for (const n of graph.nodes) lc.set(n.label, (lc.get(n.label) ?? 0) + 1);
    for (const e of graph.edges) ec.set(e.type, (ec.get(e.type) ?? 0) + 1);
    const top = [...graph.nodes]
      .filter((n) => n.wtype !== "source" && n.wtype !== "decision")
      .sort(
        (a, b) =>
          (b.claim_count ?? b.in_calls ?? 0) - (a.claim_count ?? a.in_calls ?? 0),
      )
      .slice(0, 20);
    const statuses: TrustHealth["statuses"] = {
      accepted: 0,
      provisional: 0,
      disputed: 0,
      rejected: 0,
      superseded: 0,
    };
    let stale = 0;
    let flagged = false;
    for (const n of graph.nodes) {
      if (n.flags) {
        if (Object.values(n.flags).some(Boolean)) flagged = true;
        if (n.flags.stale) stale += 1;
      }
      for (const c of n.claims ?? []) {
        const k = (c.st ?? "").toLowerCase();
        if (k in statuses) statuses[k as keyof TrustHealth["statuses"]] += 1;
      }
    }
    return {
      labelCounts: [...lc.entries()].sort((a, b) => b[1] - a[1]),
      edgeCounts: [...ec.entries()].sort((a, b) => b[1] - a[1]),
      topEntities: top,
      trust: { statuses, stale, flagged } as TrustHealth,
    };
  }, [graph]);

  const meta = graph?.meta;
  const showTrust = !!meta?.review || !!trust?.flagged;

  return (
    <ScrollArea className="h-full">
      <div className="max-w-4xl mx-auto px-6 py-8">
        <div className="flex items-start justify-between gap-4">
          <div>
            <h1 className="text-lg font-semibold text-foreground/90 mb-1">
              {meta?.project ?? "wiki"} knowledge base
            </h1>
            <p className="text-[12px] text-foreground/40 mb-8">
              {meta
                ? `${meta.claim_count.toLocaleString()} claims extracted from ${meta.source_count} source documents`
                : "loading…"}
              {meta?.generated_at && (
                <span className="text-foreground/25"> · generated {meta.generated_at}</span>
              )}
            </p>
          </div>
          <button
            onClick={reload}
            disabled={refreshing}
            className="shrink-0 text-[11px] text-foreground/40 hover:text-foreground/80 transition-colors disabled:opacity-40 mt-1"
          >
            {refreshing ? "refreshing…" : "↻ refresh"}
          </button>
        </div>

        {showTrust && trust && (
          <div className="rounded-lg border border-border/40 bg-white/[0.02] p-4 mb-8">
            <p className="text-[10px] uppercase tracking-widest text-foreground/30 mb-3">
              Trust / Review health
            </p>
            <div className="grid grid-cols-2 gap-x-8 gap-y-1.5">
              {meta?.review && (
                <div className="flex items-center gap-2 text-[12px]">
                  <span className="text-foreground/70">Review queue</span>
                  <span className="ml-auto tabular-nums text-foreground/35">
                    {meta.review.open} open · {meta.review.deferred} deferred ·{" "}
                    {meta.review.total} total
                  </span>
                </div>
              )}
              {(
                [
                  ["accepted claims", trust.statuses.accepted],
                  ["provisional claims", trust.statuses.provisional],
                  ["disputed claims", trust.statuses.disputed],
                  ["rejected claims", trust.statuses.rejected],
                  ["superseded claims", trust.statuses.superseded],
                  ...(trust.flagged ? ([["stale artifacts", trust.stale]] as [string, number][]) : []),
                ] as [string, number][]
              ).map(([label, count]) => (
                <div key={label} className="flex items-center gap-2 text-[12px]">
                  <span className="text-foreground/70">{label}</span>
                  <span className="ml-auto tabular-nums text-foreground/35">
                    {count.toLocaleString()}
                  </span>
                </div>
              ))}
            </div>
          </div>
        )}

        <div className="grid grid-cols-2 gap-6 mb-8">
          <div className="rounded-lg border border-border/40 bg-white/[0.02] p-4">
            <p className="text-[10px] uppercase tracking-widest text-foreground/30 mb-3">Node types</p>
            <div className="space-y-1.5">
              {labelCounts.map(([label, count]) => (
                <div key={label} className="flex items-center gap-2 text-[12px]">
                  <span className="w-2 h-2 rounded-full" style={{ backgroundColor: colorForLabel(label) }} />
                  <span className="text-foreground/70">{label}</span>
                  <span className="ml-auto tabular-nums text-foreground/35">{count}</span>
                </div>
              ))}
            </div>
          </div>
          <div className="rounded-lg border border-border/40 bg-white/[0.02] p-4">
            <p className="text-[10px] uppercase tracking-widest text-foreground/30 mb-3">Relationship types</p>
            <div className="space-y-1.5">
              {edgeCounts.map(([type, count]) => (
                <div key={type} className="flex items-center gap-2 text-[12px]">
                  <span className="text-foreground/70">{type.replace(/_/g, " ").toLowerCase()}</span>
                  <span className="ml-auto tabular-nums text-foreground/35">{count}</span>
                </div>
              ))}
            </div>
          </div>
        </div>

        <div className="rounded-lg border border-border/40 bg-white/[0.02] p-4">
          <p className="text-[10px] uppercase tracking-widest text-foreground/30 mb-3">Most documented entities</p>
          <div className="grid grid-cols-2 gap-x-8">
            {topEntities.map((n) => (
              <button
                key={n.id}
                onClick={() => meta && onSelectProject(meta.project)}
                className="flex items-center gap-2 py-1 text-[12px] text-left hover:bg-white/[0.03] rounded px-1 group"
              >
                <span className="w-2 h-2 rounded-full shrink-0" style={{ backgroundColor: colorForLabel(n.label) }} />
                <span className="text-foreground/60 group-hover:text-foreground/90 truncate font-mono">{n.name}</span>
                <span className="ml-auto tabular-nums text-foreground/30 shrink-0">
                  {(n.claim_count ?? n.in_calls ?? 0).toLocaleString()} claims
                </span>
              </button>
            ))}
          </div>
        </div>
      </div>
    </ScrollArea>
  );
}
