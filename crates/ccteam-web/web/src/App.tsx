// V0.3.2 F53 (chrome + routes) + F54/F55 (pages) + F58 (auth gate).
//
// F53 shipped the chrome (TopBar + ContentSplit) and three empty
// routes. F54 wired `/` → Dashboard; F55 wired `/p/:slug` and
// `/p/:slug/s/:sid` → ProjectDetail / SessionDetail. F58 wraps
// `<Routes>` with `<TokenEntryGate>` so that when the backend reports
// auth is required AND the global fetchInterceptor has observed a 401,
// the user gets `TokenEntryPage` in place of the SPA tree.
//
// Crucially the gate only swaps when BOTH conditions hold — a fresh
// bootstrap where the cookie is still good keeps rendering the SPA
// even when auth is required. The first /api/* 401 trips `saw401`
// and the gate flips on the next render.

import { useEffect, useState } from "react";
import { Route, Routes, useMatch } from "react-router-dom";
import { TopBar } from "./components/TopBar";
import { ContentSplit } from "./components/ContentSplit";
import { WorkspaceSidebar } from "./components/WorkspaceSidebar";
import Dashboard from "./pages/Dashboard";
import ProjectDetail from "./pages/ProjectDetail";
import SessionDetail from "./pages/SessionDetail";
import { TokenEntryPage } from "./components/TokenEntryPage";
import { useAuthState } from "./hooks/useAuthState";
import { fetchDashboard, type DashboardRow } from "./lib/dashboardApi";

function PlaceholderPage({ label }: { label: string }) {
  return (
    <div className="flex h-full items-center justify-center text-text-dim">
      <div className="text-xs font-mono uppercase tracking-wide">
        ccteam web — {label} not found
      </div>
    </div>
  );
}

/** When the backend says auth is required AND a 401 has been observed
 *  on any /api/* call, swap the entire SPA shell for the token entry
 *  page. Bootstrap state (probe in flight, no 401 yet) renders
 *  children so the dashboard doesn't flash a token form on every
 *  fresh load.
 *
 *  Lifted above TopBar/ContentSplit so the token page gets its full
 *  `h-dvh` real estate instead of being cropped into the left split. */
function TokenEntryGate({ children }: { children: React.ReactNode }) {
  const { authRequired, saw401 } = useAuthState();
  if (authRequired && saw401) {
    return <TokenEntryPage />;
  }
  return <>{children}</>;
}

/** Persistent Projects sidebar — mounted in App so it stays visible on
 *  every route (dashboard, project detail, session detail). Fetches
 *  `/api/v1/projects` once on mount; auth / network failures fall back
 *  to the empty-state copy the sidebar already renders. Active slug is
 *  derived from the URL via useMatch so navigation keeps the highlight
 *  in sync without prop-drilling.
 *
 *  Note: this fetch overlaps with Dashboard's own fetch (the dashboard
 *  grid still needs SSE-merged rows for last_event_label). The extra
 *  request is harmless and keeps the two surfaces decoupled. */
function PersistentSidebar() {
  const [projects, setProjects] = useState<DashboardRow[]>([]);
  useEffect(() => {
    let cancelled = false;
    fetchDashboard()
      .then((rows) => {
        if (!cancelled) setProjects(rows);
      })
      .catch(() => {
        // TokenEntryGate / Dashboard surface auth + error UX; sidebar
        // just stays empty so it doesn't double-report.
      });
    return () => {
      cancelled = true;
    };
  }, []);
  const match = useMatch("/p/:slug/*");
  const activeSlug = match?.params.slug ?? null;
  return <WorkspaceSidebar projects={projects} activeSlug={activeSlug} />;
}

export default function App() {
  return (
    <TokenEntryGate>
      <div className="h-dvh flex flex-col bg-surface-900 text-text-primary overflow-hidden safe-area-inset">
        <TopBar />
        <div className="flex flex-1 min-h-0">
          <PersistentSidebar />
          <div className="flex-1 flex flex-col min-h-0 min-w-0">
            <ContentSplit
              collapsed
              onToggleCollapse={() => {}}
              left={
                <Routes>
                  <Route path="/" element={<Dashboard />} />
                  <Route path="/p/:slug" element={<ProjectDetail />} />
                  <Route path="/p/:slug/s/:sid" element={<SessionDetail />} />
                  <Route path="*" element={<PlaceholderPage label="route" />} />
                </Routes>
              }
              right={<div />}
            />
          </div>
        </div>
      </div>
    </TokenEntryGate>
  );
}
