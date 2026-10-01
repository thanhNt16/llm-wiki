import { useCallback, useEffect, useState } from "react";
import { GraphTab } from "./components/GraphTab";
import { StatsTab } from "./components/StatsTab";
import type { TabId } from "./lib/types";
import { useWikiMeta } from "./hooks/useGraphData";

const TAB_IDS: TabId[] = ["graph", "stats"];

interface RouteState {
  tab: TabId;
  project: string | null;
}

function readRoute(): RouteState {
  const params = new URLSearchParams(window.location.search);
  const rawTab = params.get("tab");
  const tab = TAB_IDS.includes(rawTab as TabId) ? (rawTab as TabId) : "graph";
  return { tab, project: params.get("project") };
}

function routeUrl(tab: TabId, project: string | null): string {
  const params = new URLSearchParams();
  params.set("tab", tab);
  if (project) params.set("project", project);
  return `${window.location.pathname}?${params.toString()}${window.location.hash}`;
}

export function App() {
  const [route, setRoute] = useState<RouteState>(readRoute);
  const { tab: activeTab, project: selectedProject } = route;
  const { meta } = useWikiMeta();
  const displayProject = selectedProject ?? meta?.project ?? null;

  useEffect(() => {
    const initial = readRoute();
    window.history.replaceState(null, "", routeUrl(initial.tab, initial.project));
  }, []);

  useEffect(() => {
    const onPopState = () => setRoute(readRoute());
    window.addEventListener("popstate", onPopState);
    return () => window.removeEventListener("popstate", onPopState);
  }, []);

  const navigate = useCallback((tab: TabId, project: string | null) => {
    const url = routeUrl(tab, project);
    const current = `${window.location.pathname}${window.location.search}${window.location.hash}`;
    if (url === current) return;
    window.history.pushState(null, "", url);
    setRoute({ tab, project });
  }, []);

  const tabs: { id: TabId; label: string }[] = [
    { id: "graph", label: "Graph" },
    { id: "stats", label: "Overview" },
  ];

  return (
    <div className="h-screen flex flex-col bg-background text-foreground">
      <header className="flex items-center justify-between px-5 h-12 border-b border-border bg-[#0b1920]/80 backdrop-blur-md shrink-0">
        <div className="flex items-center gap-6">
          <div className="flex items-center gap-2.5">
            <div className="w-[7px] h-[7px] rounded-full bg-primary" />
            <span className="text-[13px] font-semibold text-foreground/90 tracking-tight">
              {(displayProject ?? "wiki") + " · llm-wiki"}
            </span>
            <span className="translate-y-px text-[10px] font-mono text-foreground/30">
              {meta ? `${meta.claim_count.toLocaleString()} claims · ${meta.source_count} sources` : "loading…"}
            </span>
          </div>
          <nav className="flex items-center gap-0.5">
            {tabs.map((tab) => (
              <button
                key={tab.id}
                onClick={() => navigate(tab.id, selectedProject)}
                className={`px-3 py-1 rounded-md text-[12px] font-medium transition-all ${
                  activeTab === tab.id
                    ? "bg-primary/15 text-primary"
                    : "text-muted-foreground hover:text-foreground hover:bg-white/[0.04]"
                }`}
              >
                {tab.label}
              </button>
            ))}
          </nav>
        </div>
        {displayProject && (
          <div className="flex items-center gap-2 px-3 py-1 rounded-lg bg-white/[0.04] border border-border/30">
            <span className="text-[10px] text-foreground/30 uppercase tracking-wider">
              knowledge base
            </span>
            <span className="text-[11px] text-primary font-mono truncate max-w-[300px]">
              {displayProject}
            </span>
          </div>
        )}
      </header>
      <main className="flex-1 min-h-0">
        {activeTab === "graph" ? (
          <GraphTab project={displayProject} />
        ) : (
          <StatsTab onSelectProject={(p) => navigate("graph", p)} />
        )}
      </main>
    </div>
  );
}
