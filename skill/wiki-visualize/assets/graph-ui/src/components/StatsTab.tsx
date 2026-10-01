import { useEffect, useMemo, useState } from "react";
import { ScrollArea } from "@/components/ui/scroll-area";
import { colorForLabel } from "../lib/colors";
import { loadWikiData } from "../lib/layout";
import type { GraphData } from "../lib/types";

interface StatsTabProps {
  onSelectProject: (project: string) => void;
}

export function StatsTab({ onSelectProject }: StatsTabProps) {
  const [graph, setGraph] = useState<GraphData | null>(null);

  useEffect(() => {
    loadWikiData().then(setGraph).catch(() => {});
  }, []);

  const { labelCounts, edgeCounts, topEntities } = useMemo(() => {
    const lc = new Map<string, number>();
    const ec = new Map<string, number>();
    if (!graph) return { labelCounts: [], edgeCounts: [], topEntities: [] };
    for (const n of graph.nodes) lc.set(n.label, (lc.get(n.label) ?? 0) + 1);
    for (const e of graph.edges) ec.set(e.type, (ec.get(e.type) ?? 0) + 1);
    const top = [...graph.nodes]
      .filter((n) => n.wtype !== "source" && n.wtype !== "decision")
      .sort((a, b) => (b.in_calls ?? 0) - (a.in_calls ?? 0))
      .slice(0, 20);
    return {
      labelCounts: [...lc.entries()].sort((a, b) => b[1] - a[1]),
      edgeCounts: [...ec.entries()].sort((a, b) => b[1] - a[1]),
      topEntities: top,
    };
  }, [graph]);

  const meta = graph?.meta;

  return (
    <ScrollArea className="h-full">
      <div className="max-w-4xl mx-auto px-6 py-8">
        <h1 className="text-lg font-semibold text-foreground/90 mb-1">
          {meta?.project ?? "wiki"} knowledge base
        </h1>
        <p className="text-[12px] text-foreground/40 mb-8">
          {meta
            ? `${meta.claim_count.toLocaleString()} claims extracted from ${meta.source_count} source documents`
            : "loading…"}
        </p>

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
                <span className="ml-auto tabular-nums text-foreground/30 shrink-0">{n.in_calls} claims</span>
              </button>
            ))}
          </div>
        </div>
      </div>
    </ScrollArea>
  );
}
