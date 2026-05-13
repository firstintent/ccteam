// V0.3.2 F53 — minimal SPA shell.
//
// The AoE-derived App.tsx was a 700-line orchestrator that wired up
// cockpit, session wizard, settings, projects, command palette,
// per-session diff viewer, and routing for AoE's specific tmux-only
// workflow. None of that survives V0.3.2 cleanly — ccteam has its
// own state model (projects + sessions out of `~/.ccteam/state.json`),
// its own auth surface, and its own write-action endpoints.
//
// F53 ships ONLY the chrome (TopBar + ContentSplit) and a react-router
// scaffold with three empty routes. F54 reintroduces the dashboard,
// F55 reintroduces project / session detail, F57 wires the WS PTY
// terminal. Until then, navigating to any route shows a placeholder
// telling the operator which milestone owns the next slice.

import { Route, Routes } from "react-router-dom";
import { TopBar } from "./components/TopBar";
import { ContentSplit } from "./components/ContentSplit";

function PlaceholderPage({ label }: { label: string }) {
  return (
    <div className="flex h-full items-center justify-center text-text-dim">
      <div className="text-xs font-mono uppercase tracking-wide">
        ccteam web — {label} pending (F54+)
      </div>
    </div>
  );
}

export default function App() {
  return (
    <div className="h-dvh flex flex-col bg-surface-900 text-text-primary overflow-hidden safe-area-inset">
      <TopBar />
      <div className="flex flex-1 min-h-0">
        <div className="flex-1 flex flex-col min-h-0 min-w-0">
          <ContentSplit
            collapsed
            onToggleCollapse={() => {}}
            left={
              <Routes>
                <Route path="/" element={<PlaceholderPage label="dashboard" />} />
                <Route
                  path="/p/:slug"
                  element={<PlaceholderPage label="project detail" />}
                />
                <Route
                  path="/p/:slug/s/:sid"
                  element={<PlaceholderPage label="session detail" />}
                />
                <Route path="*" element={<PlaceholderPage label="route" />} />
              </Routes>
            }
            right={<div />}
          />
        </div>
      </div>
    </div>
  );
}
