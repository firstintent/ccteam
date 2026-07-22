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
    // Vendor chip is an icon-only mark (no text label) with data-vendor.
    expect(html).toContain('class="chip claude vendor-chip"');
    expect(html).toContain('data-vendor="claude"');
    expect(html).toContain('aria-label="claude"');
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

type HookStateSetter = (value: unknown | ((previous: unknown) => unknown)) => void;

function createHookHarness() {
  const slots: unknown[] = [];
  const dependencies: Array<readonly unknown[] | undefined> = [];
  let cursor = 0;
  let pendingEffects: Array<() => void | (() => void)> = [];

  const changed = (index: number, next: readonly unknown[] | undefined) => {
    const previous = dependencies[index];
    dependencies[index] = next;
    if (!next || !previous || next.length !== previous.length) return true;
    return next.some((value, offset) => !Object.is(value, previous[offset]));
  };

  const useState = (initial: unknown) => {
    const index = cursor++;
    if (!(index in slots)) slots[index] = typeof initial === "function" ? initial() : initial;
    const setState: HookStateSetter = (value) => {
      slots[index] = typeof value === "function" ? value(slots[index]) : value;
    };
    return [slots[index], setState];
  };
  const useRef = (initial: unknown) => {
    const index = cursor++;
    if (!(index in slots)) slots[index] = { current: initial };
    return slots[index];
  };
  const useEffect = (effect: () => void | (() => void), deps?: readonly unknown[]) => {
    const index = cursor++;
    if (changed(index, deps)) pendingEffects.push(effect);
  };
  const useMemo = (factory: () => unknown, deps?: readonly unknown[]) => {
    const index = cursor++;
    if (changed(index, deps)) slots[index] = factory();
    return slots[index];
  };
  const useCallback = (callback: unknown, deps?: readonly unknown[]) => {
    const index = cursor++;
    if (changed(index, deps)) slots[index] = callback;
    return slots[index];
  };

  return {
    hooks: {
      useState,
      useRef,
      useEffect,
      useLayoutEffect: useEffect,
      useMemo,
      useCallback,
    },
    render<T>(component: () => T): T {
      cursor = 0;
      pendingEffects = [];
      const tree = component();
      const effects = pendingEffects;
      pendingEffects = [];
      for (const effect of effects) effect();
      return tree;
    },
  };
}

function collectElementText(value: unknown): string[] {
  if (typeof value === "string" || typeof value === "number") return [String(value)];
  if (Array.isArray(value)) return value.flatMap(collectElementText);
  if (!value || typeof value !== "object") return [];
  const props = (value as { props?: Record<string, unknown> }).props;
  if (!props) return [];
  const ownContent = typeof props.content === "string" ? [props.content] : [];
  return [...ownContent, ...collectElementText(props.children)];
}

describe("SessionView reconnect history reseed", () => {
  it("refetches authoritative history and restores an answer never delivered by SSE", async () => {
    const harness = createHookHarness();
    let stream = {
      events: [{ id: "seen", kind: "answer" as const, content: "already-seen" }],
      connected: true,
      connectionEpoch: 1,
      lastError: null,
      gatewayUnavailable: false,
    };
    const history = vi
      .fn()
      .mockResolvedValueOnce({
        sid: "s9",
        events: [
          { turn_id: "t1", ts: "now", role: "cto", user: "prompt", assistant: "already-seen" },
        ],
      })
      .mockResolvedValueOnce({
        sid: "s9",
        events: [
          { turn_id: "t1", ts: "now", role: "cto", user: "prompt", assistant: "already-seen" },
          {
            turn_id: "t2",
            ts: "later",
            role: "cto",
            user: "internal wakeup",
            assistant: "never-delivered-via-sse",
          },
        ],
      });

    vi.resetModules();
    vi.doMock("react", async () => ({
      ...(await vi.importActual<typeof import("react")>("react")),
      ...harness.hooks,
    }));
    vi.doMock("../hooks/useSessionEvents", () => ({ useSessionEvents: () => stream }));
    vi.doMock("../lib/sessionsApi", async () => ({
      ...(await vi.importActual<typeof import("../lib/sessionsApi")>("../lib/sessionsApi")),
      getHistory: history,
      getSessionStatus: vi.fn().mockResolvedValue({
        sid: "s9",
        model: null,
        context: null,
        status_line: null,
      }),
    }));

    try {
      const ReconnectSessionView = (await import("./SessionView")).default;
      const renderReconnectView = () =>
        harness.render(() => ReconnectSessionView({ sid: "s9", session: SESSION }));

      renderReconnectView();
      await Promise.resolve();
      let tree = renderReconnectView();
      expect(history).toHaveBeenCalledTimes(1);
      expect(collectElementText(tree)).toContain("already-seen");

      stream = { ...stream, connectionEpoch: 2 };
      renderReconnectView();
      await Promise.resolve();
      tree = renderReconnectView();

      expect(history).toHaveBeenCalledTimes(2);
      expect(collectElementText(tree)).toContain("never-delivered-via-sse");

      stream = {
        ...stream,
        events: [...stream.events, { id: "live", kind: "answer", content: "live-after-reseed" }],
      };
      renderReconnectView();
      tree = renderReconnectView();
      expect(collectElementText(tree).filter((text) => text === "already-seen")).toHaveLength(1);
      expect(collectElementText(tree).filter((text) => text === "live-after-reseed")).toHaveLength(1);
    } finally {
      vi.doUnmock("react");
      vi.doUnmock("../hooks/useSessionEvents");
      vi.doUnmock("../lib/sessionsApi");
      vi.resetModules();
    }
  });
});
