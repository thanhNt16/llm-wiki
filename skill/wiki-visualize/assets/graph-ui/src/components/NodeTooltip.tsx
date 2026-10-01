import { Html } from "@react-three/drei";
import type { GraphNode } from "../lib/types";
import { colorForLabel } from "../lib/colors";

interface NodeTooltipProps {
  node: GraphNode;
}

export function NodeTooltip({ node }: NodeTooltipProps) {
  const summary = node.summary ?? [];
  return (
    <Html
      position={[node.x, node.y + node.size * 0.7, node.z]}
      center
      style={{ pointerEvents: "none" }}
    >
      <div className="bg-[#1a1a2e]/95 backdrop-blur border border-white/10 rounded-lg px-3 py-2 text-xs shadow-xl max-w-[380px]">
        <div className="flex items-center gap-1.5 mb-1">
          <span
            className="w-2 h-2 rounded-full shrink-0"
            style={{ backgroundColor: colorForLabel(node.label) }}
          />
          <span className="text-white font-medium truncate">{node.name}</span>
          <span className="text-white/30 ml-1 shrink-0">{node.wtype ?? node.label}</span>
        </div>
        {node.qualified_name && (
          <p className="text-white/30 font-mono truncate">{node.qualified_name}</p>
        )}
        {summary.slice(0, 3).map((s, i) => (
          <p key={i} className="text-white/45 truncate mt-0.5">
            <span className="text-[#5ea1ff]/80">{s.p}</span>{" "}
            {String(typeof s.v === "object" ? JSON.stringify(s.v) : s.v).slice(0, 90)}
          </p>
        ))}
        {node.in_calls !== undefined && node.wtype !== "source" && (
          <p className="text-white/25 mt-1 text-[10px]">
            {node.in_calls} claim{node.in_calls === 1 ? "" : "s"}
            {node.sources?.length ? ` · ${node.sources.length} source${node.sources.length === 1 ? "" : "s"}` : ""}
          </p>
        )}
        <p className="text-white/20 mt-1 text-[10px]">click for claims →</p>
      </div>
    </Html>
  );
}
