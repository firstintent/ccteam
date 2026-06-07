// v0.8.9 — SessionView is the per-SID view extracted from ChatConsole and
// rendered KEYED BY SID (`<SessionView key={sid} sid={sid} />`). The keying is
// THE structural fix for the "fresh session briefly shows the previous
// session's messages" bug: a new sid mounts a FRESH instance, so all per-sid
// state (rows / SSE buffer / draft / view) starts empty.
//
// This proves the mount-empty invariant: renderToString a SessionView for a
// brand-new sid whose localStorage key is empty → the transcript renders no
// message rows. EventSource + getHistory don't run under SSR (no effects),
// which is exactly the point — the INITIAL render must already be empty.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// The import chain (useSessionEvents → no DOM at module load, but the lazy
// `loadRows(sid)` initializer reads localStorage on mount). Stub a minimal
// window + localStorage BEFORE the static imports (node env, no DOM) so the
// render can't touch a real browser API. `vi.hoisted` runs above imports.
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

import SessionView from "./SessionView";
import { rowsKeyFor } from "./chatTranscript";
import type { SessionView as SessionSummary } from "../lib/sessionsApi";

const SESSION: SessionSummary = {
  sid: "s9",
  project: "demo",
  role: "cto",
  vendor: "claude",
  permission_mode: "skip",
  current: true,
  status: "live",
};

describe("SessionView mount-empty invariant (key={sid} remount)", () => {
  beforeEach(() => {
    // getHistory fetches in an effect; effects don't run under renderToString,
    // but stub fetch to a never-resolving promise so nothing can fire a real
    // request even if the runtime changes.
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
    // Belt-and-suspenders: a brand-new sid has no persisted rows.
    vi.spyOn(globalThis.localStorage, "getItem").mockReturnValue(null);
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("renders an empty transcript (no message rows) for a fresh sid", () => {
    const html = renderToString(<SessionView sid="s9" session={SESSION} />);
    // The composer is present (so the view rendered the chat surface)…
    expect(html).toContain("发消息 / 命令");
    // …and the crumb shows this session's identity.
    expect(html).toContain(">demo<");
    expect(html).toContain(">s9<");
    // But there are NO transcript bubbles: none of the row container classes
    // (assistant/user/system/approval) appear, because rows seeded empty and
    // the SSE/history seeds can't run under SSR. This is the "new session
    // mounts empty" guarantee the key={sid} remount provides.
    expect(html).not.toContain("ml-auto bg-brand-dim/40"); // user bubble
    expect(html).not.toContain("bg-surface-800 border border-surface-700/40"); // assistant bubble
    expect(html).not.toContain("bg-brand-500/10 border border-brand-500/30"); // approval
  });

  it("loads THIS sid's persisted rows on mount (the per-sid seed, not a flat key)", () => {
    // A reopened page must seed from the sid-scoped localStorage key. Prove the
    // lazy initializer reads exactly `ccteam.chat.rows.v2.s9` (and renders that
    // row), not a shared/previous-session buffer.
    const getItem = vi.spyOn(globalThis.localStorage, "getItem").mockImplementation((k) =>
      k === rowsKeyFor("s9")
        ? JSON.stringify([{ id: "x", kind: "assistant", content: "seeded-from-s9" }])
        : null,
    );
    const html = renderToString(<SessionView sid="s9" session={SESSION} />);
    expect(getItem).toHaveBeenCalledWith(rowsKeyFor("s9"));
    expect(html).toContain("seeded-from-s9");
  });
});
