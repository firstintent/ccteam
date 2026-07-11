// v0.8.24 Track A — SessionView is the Conversation view, rendered KEYED BY
// SID by the shell (`<SessionView key={sid} sid={sid} />`). The keying is THE
// structural fix for "fresh session briefly shows the previous session's
// messages": a new sid mounts a FRESH instance, so all per-sid state (rows /
// SSE buffer / draft / chat|terminal view) starts empty.
//
// SSR (renderToString) proves the mount-empty invariant + the per-sid
// localStorage seed. EventSource + getHistory don't run under SSR (no
// effects) — the INITIAL render must already be empty.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

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
import { MemoryRouter } from "react-router-dom";

import SessionView, { effortKeyOf } from "./SessionView";
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

function render(session: SessionSummary | null = SESSION) {
  // CostPill (conv-head) navigates → needs a Router context under SSR.
  return renderToString(
    <MemoryRouter>
      <SessionView sid="s9" session={session} />
    </MemoryRouter>,
  );
}

describe("SessionView mount-empty invariant (key={sid} remount)", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
    vi.spyOn(globalThis.localStorage, "getItem").mockReturnValue(null);
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("renders an empty transcript (no message rows) for a fresh sid", () => {
    const html = render();
    // The prototype conv chrome is present…
    expect(html).toContain('data-testid="conversation-view"');
    expect(html).toContain("Enter 发送"); // conv composer placeholder
    expect(html).toContain(">s9<"); // sid chip
    expect(html).toContain(">demo<"); // project chip
    expect(html).toContain(">claude<"); // vendor chip
    // …but there are NO transcript bubbles: rows seeded empty and the
    // SSE/history seeds can't run under SSR.
    expect(html).not.toContain('class="msg user');
    expect(html).not.toContain('class="msg agent');
    expect(html).not.toContain('class="msg approval');
  });

  it("loads THIS sid's persisted rows on mount (the per-sid seed, not a flat key)", () => {
    const getItem = vi.spyOn(globalThis.localStorage, "getItem").mockImplementation((k) =>
      k === rowsKeyFor("s9")
        ? JSON.stringify([{ id: "x", kind: "assistant", content: "seeded-from-s9" }])
        : null,
    );
    const html = render();
    expect(getItem).toHaveBeenCalledWith(rowsKeyFor("s9"));
    expect(html).toContain("seeded-from-s9");
  });

  it("hides the terminal tab for a stream-json session (no pane) and shows it for claude terminal", () => {
    const streamJson = render({ ...SESSION, protocol: "stream-json" });
    expect(streamJson).not.toContain('data-testid="terminal-tab"');
    const terminal = render({ ...SESSION, protocol: "terminal" });
    expect(terminal).toContain('data-testid="terminal-tab"');
    // codex never gets a terminal tab.
    const codex = render({ ...SESSION, vendor: "codex", protocol: "stream-json" });
    expect(codex).not.toContain('data-testid="terminal-tab"');
  });

  it("shows the @host chip only for a remote session", () => {
    const strip = (h: string) => h.replace(/<!-- -->/g, "");
    const local = render({ ...SESSION, host: "local" });
    expect(strip(local)).not.toContain("@ local");
    const remote = render({ ...SESSION, host: "dev04" });
    expect(strip(remote)).toContain("@ dev04");
  });
});

describe("effortKeyOf (backend effort token → dictionary key)", () => {
  it("maps the four levels", () => {
    expect(effortKeyOf("low")).toBe("effLow");
    expect(effortKeyOf("medium")).toBe("effMid");
    expect(effortKeyOf("high")).toBe("effHigh");
    expect(effortKeyOf("max")).toBe("effMax");
    expect(effortKeyOf("xhigh")).toBe("effMax");
  });

  it("returns null (hide, never fake) for unknown/absent", () => {
    expect(effortKeyOf(null)).toBeNull();
    expect(effortKeyOf(undefined)).toBeNull();
    expect(effortKeyOf("weird")).toBeNull();
  });
});
