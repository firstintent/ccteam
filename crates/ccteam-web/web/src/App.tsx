// v0.8.24 Track A — routes for the prototype shell. ONE shell (ChatConsole)
// hosts four mutually-exclusive views:
//   /                → Home (landing; lazy-create on first message)
//   /chat/s/:sid     → Conversation
//   /flow/:tab?      → 工作流 (Skills/Roles/MCP/自进化)
//   /settings/:tab?  → 设置 (运维总览/插件市场/通用/账号/管理员)
// Legacy flat routes (marketplace/status/hosts/workflow) redirect into the
// new IA so old deep links keep working.

import { useEffect } from "react";
import { Navigate, Route, Routes } from "react-router-dom";
import ChatConsole from "./pages/ChatConsole";
import { TokenEntryPage } from "./components/TokenEntryPage";
import { useAuthState } from "./hooks/useAuthState";
import { useWebSettings } from "./hooks/useWebSettings";

/** Keep the `<html>` `.dark` class in sync with the chosen theme (light is
 *  the product default — `:root` tokens). The inline script in index.html
 *  applies the stored choice before first paint; this syncs later changes. */
function useThemeClass() {
  const { settings } = useWebSettings();
  useEffect(() => {
    const root = document.documentElement;
    root.classList.add("theme-switching");
    root.classList.toggle("dark", settings.theme === "dark");
    const t = window.setTimeout(() => root.classList.remove("theme-switching"), 1);
    return () => window.clearTimeout(t);
  }, [settings.theme]);
}

/** When the backend says auth is required AND a 401 has been observed on any
 *  /api/* call, swap the entire SPA shell for the token entry page. */
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
      <Routes>
        <Route path="/" element={<ChatConsole />} />
        <Route path="/chat" element={<Navigate to="/" replace />} />
        <Route path="/chat/s/:sid" element={<ChatConsole />} />
        <Route path="/flow" element={<ChatConsole />} />
        <Route path="/flow/:tab" element={<ChatConsole />} />
        <Route path="/settings" element={<ChatConsole />} />
        <Route path="/settings/status" element={<Navigate to="/settings/ops" replace />} />
        <Route path="/settings/hosts" element={<Navigate to="/settings/ops" replace />} />
        <Route path="/settings/:tab" element={<ChatConsole />} />
        {/* v0.9.0 W4 — 团队/Team view (admin-only nav entry; beta-gate). */}
        <Route path="/agents" element={<ChatConsole />} />
        {/* legacy flat routes → the new IA */}
        <Route path="/marketplace" element={<Navigate to="/settings/market" replace />} />
        <Route path="/status" element={<Navigate to="/settings/ops" replace />} />
        <Route path="/hosts" element={<Navigate to="/settings/ops" replace />} />
        <Route path="/workflow" element={<Navigate to="/flow" replace />} />
        <Route path="*" element={<ChatConsole />} />
      </Routes>
    </TokenEntryGate>
  );
}
