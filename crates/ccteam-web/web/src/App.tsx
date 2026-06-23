// v0.8.9 Phase 4 — unified chat-style shell.
//
// The old forked layout (operator shell = TopBar + ContentSplit + the
// Dashboard/ProjectDetail/SessionDetail/Sessions/Teams/Roles operator pages
// vs the standalone `/chat` ChatConsole) is gone. There is now ONE shell:
// `ChatConsole`, which carries its own sidebar (project→session tree + a
// bottom global-nav: 插件市场 / Status / Settings), top bar (crumb +
// connection + cost-pill slot + Chat|终端 tabs) and main area.
//
//   - `/` and `/chat`        → the shell with no session selected (empty
//                              state) — ready to pick a session or 新建.
//   - `/chat/s/:sid`         → the shell driving that one gateway `s{n}`
//                              session's chat / terminal.
//   - `/marketplace`         → the shell hosting the plugin-marketplace
//                              global view (placeholder this phase).
//   - `/status`              → the shell hosting the lightweight Status
//                              global view (placeholder this phase).
//   - `/hosts`               → the shell hosting the 主机/Hosts host-keyed
//                              agent report (install/MCP status per machine).
//   - `/settings`            → the shell hosting the IM-config Settings view.
//
// On a global route the sidebar persists, the main area shows the global
// view, and the Chat|终端 tabs hide (you're out of session context). The
// token-gate (`TokenEntryGate` + `useAuthState`) still wraps the whole tree
// so a 401 swaps the SPA for `TokenEntryPage` at full real estate.

import { useEffect } from "react";
import { Route, Routes } from "react-router-dom";
import ChatConsole from "./pages/ChatConsole";
import { TokenEntryPage } from "./components/TokenEntryPage";
import { useAuthState } from "./hooks/useAuthState";
import { useWebSettings } from "./hooks/useWebSettings";

/** Keep the `<html>` `.light` class in sync with the chosen theme. The inline
 *  script in index.html sets the initial class before first paint (no flash);
 *  this syncs it whenever the avatar-menu toggle changes it. */
function useThemeClass() {
  const { settings } = useWebSettings();
  useEffect(() => {
    const root = document.documentElement;
    root.classList.add("theme-switching");
    root.classList.toggle("light", settings.theme === "light");
    const t = window.setTimeout(() => root.classList.remove("theme-switching"), 1);
    return () => window.clearTimeout(t);
  }, [settings.theme]);
}

/** When the backend says auth is required AND a 401 has been observed on any
 *  /api/* call, swap the entire SPA shell for the token entry page. Bootstrap
 *  state (probe in flight, no 401 yet) renders children so the shell doesn't
 *  flash a token form on every fresh load.
 *
 *  Lifted above the shell so the token page gets its full `h-dvh` real estate
 *  instead of being cropped into a split. */
function TokenEntryGate({ children }: { children: React.ReactNode }) {
  const { authRequired, saw401 } = useAuthState();
  if (authRequired && saw401) {
    return <TokenEntryPage />;
  }
  return <>{children}</>;
}

export default function App() {
  useThemeClass();
  return (
    <TokenEntryGate>
      <div className="h-dvh flex flex-col bg-surface-900 text-text-primary overflow-hidden safe-area-inset">
        <Routes>
          {/* The single chat-style shell hosts every surface: a selected
              session's chat/terminal, the empty state, and the three global
              views. ChatConsole reads the location to decide what to render
              in its main area. */}
          <Route path="/" element={<ChatConsole />} />
          <Route path="/chat" element={<ChatConsole />} />
          <Route path="/chat/s/:sid" element={<ChatConsole />} />
          <Route path="/marketplace" element={<ChatConsole />} />
          <Route path="/status" element={<ChatConsole />} />
          <Route path="/hosts" element={<ChatConsole />} />
          <Route path="/settings" element={<ChatConsole />} />
          {/* Unknown routes fall back to the empty shell. */}
          <Route path="*" element={<ChatConsole />} />
        </Routes>
      </div>
    </TokenEntryGate>
  );
}
