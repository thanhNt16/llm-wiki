import { describe, it, expect, vi, beforeEach } from "vitest";
import { render, screen } from "@testing-library/react";
import type { GraphData } from "../lib/types";
import type { WikiPayload } from "../lib/layout";

const fake = (over: Partial<WikiPayload> = {}): WikiPayload => ({
  project: "p", nodes: [], links: [], claim_count: 0, source_count: 0, ...over,
} as unknown as WikiPayload);
let payload: WikiPayload = fake();
vi.mock("../lib/layout", async (orig) => {
  const mod = await orig<typeof import("../lib/layout")>();
  return { ...mod, loadWikiData: () => Promise.resolve(mod.payloadToGraphData(payload)) };
});
const { StatsTab } = await import("./StatsTab");
const { NodeDetailPanel } = await import("./NodeDetailPanel");
const { FilterPanel } = await import("./FilterPanel");

const node = (over: Record<string, unknown> = {}): GraphData["nodes"][number] => ({
  id: 0, x: 0, y: 0, z: 0, label: "Entity", name: "a", size: 4, color: "#fff",
  ...over,
} as GraphData["nodes"][number]);

beforeEach(() => {
  payload = fake({ nodes: [{ id: 0, kind: "entity", key: "p.a", label: "a", wtype: "domain", claim_count: 7 }] });
});

describe("StatsTab R4/R5/R14", () => {
  it("hides trust card and generated_at on old payloads", async () => {
    render(<StatsTab onSelectProject={() => {}} />);
    await screen.findByText(/claims extracted/);
    expect(screen.queryByText("Trust / Review health")).toBeNull();
    expect(screen.queryByText(/generated /)).toBeNull();
  });
  it("shows trust card, generated_at, claim_count label on new payloads", async () => {
    payload = fake({
      nodes: [{ id: 0, kind: "entity", key: "p.a", label: "a", wtype: "domain", claim_count: 7,
        flags: { stale: true } }],
      meta: { review: { open: 1, deferred: 2, total: 3 }, generated_at: "2026-10-02T00:00:00Z" },
      claims: [],
    } as unknown as WikiPayload);
    render(<StatsTab onSelectProject={() => {}} />);
    await screen.findByText("Trust / Review health");
    expect(screen.getByText(/1 open · 2 deferred · 3 total/)).toBeTruthy();
    expect(screen.getByText(/generated 2026-10-02T00:00:00Z/)).toBeTruthy();
    expect(screen.getByText(/7 claims/)).toBeTruthy();
  });
});

describe("NodeDetailPanel R6", () => {
  it("renders no badges/review sections without flags", () => {
    render(<NodeDetailPanel node={node()} allNodes={[]} allEdges={[]} project={null}
      repoInfo={null} canGoBack={false} onBack={() => {}} onClose={() => {}}
      onNavigate={() => {}} />);
    expect(screen.queryByText(/review show/)).toBeNull();
    expect(screen.queryByText("Supersession chain")).toBeNull();
  });
  it("renders flags, review items, copyable command, supersession chain", async () => {
    const n = node({
      flags: { review_open: true, disputed: true },
      review: [{ id: "review_x1", kind: "possible_contradiction", risk: "high", affected: ["claim_9"], status: "open" }],
      claims: [{ id: "claim_1", s: "a", p: "k", v: 1, st: "superseded", sup_by: "claim_2" }],
    });
    render(<NodeDetailPanel node={n} allNodes={[]} allEdges={[]} project={null}
      repoInfo={null} canGoBack={false} onBack={() => {}} onClose={() => {}}
      onNavigate={() => {}} />);
    expect(screen.getByText("open review")).toBeTruthy();
    expect(screen.getByText("disputed")).toBeTruthy();
    expect(screen.getByText("possible_contradiction")).toBeTruthy();
    expect(screen.getByText(/risk: high/)).toBeTruthy();
    expect(screen.getByText(/affects: claim_9/)).toBeTruthy();
    expect(screen.getByText(/\$ wiki.py review show review_x1/)).toBeTruthy();
    expect(screen.getByText("Supersession chain")).toBeTruthy();
    expect(screen.getByText(/superseded by/)).toBeTruthy();
  });
});

describe("FilterPanel R7", () => {
  const base = {
    enabledLabels: new Set(["Entity"]), enabledEdgeTypes: new Set(["EVIDENCE"]),
    enabledFlags: new Set<string>(), showLabels: false,
    onToggleLabel: () => {}, onToggleEdgeType: () => {}, onToggleFlag: () => {},
    onToggleShowLabels: () => {}, onEnableAll: () => {}, onDisableAll: () => {},
  };
  it("hides flag filters without flags", () => {
    render(<FilterPanel data={{ nodes: [node()], edges: [], total_nodes: 1 } as GraphData} {...base} />);
    expect(screen.queryByText("Review flags")).toBeNull();
  });
  it("shows flag filters with counts when flags exist", () => {
    const data = { nodes: [node({ flags: { stale: true, orphan: true } }), node()], edges: [], total_nodes: 2 } as GraphData;
    render(<FilterPanel data={data} {...base} />);
    expect(screen.getByText("Review flags")).toBeTruthy();
    expect(screen.getByText("stale").closest("button")?.textContent).toContain("1");
    expect(screen.getByText("orphan").closest("button")?.textContent).toContain("1");
    expect(screen.queryByText(/open review/)).toBeNull(); // zero-count flags hidden
  });
});
