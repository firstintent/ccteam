// v0.8.9 — ChatConsole is now the persistent SHELL: it keeps the sidebar /
// bottom-nav / cost pill / new-session modal / cross-project rail, and renders
// the per-SID view as a KEYED child — `<SessionView key={sid} sid={sid} />`.
// The key is THE structural fix: React remounts a fresh SessionView on every
// session switch, so no per-sid state survives a switch.
//
// `key` isn't serialized into SSR HTML, so we prove the delegation behaviorally:
// routed to `/chat/s/<sid>`, the shell renders SessionView's distinctive
// surface (the composer placeholder, which ONLY SessionView emits) for that
// sid; routed to `/chat` (no sid) it renders the no-session empty state and NOT
// the composer. (The atomic-remount guarantee itself is unit-proved by
// SessionView.test.tsx's mount-empty invariant.)

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// ChatConsole's import chain reaches useWebSettings, which reads
// `window.innerWidth` / `localStorage` at runtime. Node env (no DOM) → stub a
// minimal window + localStorage BEFORE the static imports. `vi.hoisted` runs
// above imports.
vi.hoisted(() => {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const g = globalThis as any;
  if (typeof g.window === "undefined") {
    g.window = { innerWidth: 1024, addEventListener() {}, removeEventListener() {} };
  }
  if (typeof g.localStorage === "undefined") {
    g.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };
  }
});

import { renderToString } from "react-dom/server";
import { MemoryRouter, Route, Routes } from "react-router-dom";

import ChatConsole from "./ChatConsole";

// Mirror App.tsx's routing so `useParams` inside ChatConsole resolves `:sid`.
function routed(path: string) {
  return renderToString(
    <MemoryRouter initialEntries={[path]}>
      <Routes>
        <Route path="/chat" element={<ChatConsole />} />
        <Route path="/chat/s/:sid" element={<ChatConsole />} />
      </Routes>
    </MemoryRouter>,
  );
}

describe("ChatConsole shell delegates the per-sid view to SessionView", () => {
  beforeEach(() => {
    // refreshSessions + getHistory fetch in effects (don't run under SSR), but
    // stub fetch to a never-resolving promise so nothing fires a real request.
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("mounts the SessionView surface (composer) when routed to /chat/s/:sid", () => {
    const html = routed("/chat/s/s9");
    // The composer placeholder is emitted ONLY by SessionView — its presence
    // proves the shell rendered <SessionView sid="s9"> in its main area.
    expect(html).toContain("发消息 / 命令");
    // The shell chrome is still there (it's the persistent shell).
    expect(html).toContain("所有 session");
  });

  it("renders the no-session empty state (NOT the composer) at /chat", () => {
    const html = routed("/chat");
    // No sid → empty state, and the SessionView composer must be absent.
    expect(html).toContain("从左侧选一个 session");
    expect(html).not.toContain("发消息 / 命令");
  });
});
