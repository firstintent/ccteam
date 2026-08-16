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

function render(
  session: SessionSummary | null = SESSION,
  onRename?: (sid: string, title: string) => void,
) {
  // CostPill (conv-head) navigates → needs a Router context under SSR.
  return renderToString(
    <MemoryRouter>
      <SessionView sid="s9" session={session} onRename={onRename} />
    </MemoryRouter>,
  );
}

describe("conversation header rename affordance", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
    vi.spyOn(globalThis.localStorage, "getItem").mockReturnValue(null);
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("offers the rename control (and the keyboard hint) when the shell wires it", () => {
    const html = render({ ...SESSION, title: "Fix the login bug" }, () => {});
    expect(html).toContain('data-testid="conv-rename"');
    expect(html).toContain("Fix the login bug");
    // The title itself advertises the double-click + key hints.
    expect(html).toContain("回车保存,Esc 取消");
  });

  it("renders a plain title with no control when renaming is not wired", () => {
    const html = render({ ...SESSION, title: "Fix the login bug" });
    expect(html).toContain('data-testid="conv-title"');
    expect(html).not.toContain('data-testid="conv-rename"');
  });
});

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

  it("renders attachment refs through the fixed project URL without data/blob URLs", () => {
    vi.spyOn(globalThis.localStorage, "getItem").mockImplementation((k) =>
      k === rowsKeyFor("s9")
        ? JSON.stringify([
            {
              id: "file-row",
              kind: "assistant",
              content: "",
              attachments: [
                {
                  id: "1780000000000-chart.png",
                  name: "chart.png",
                  kind: "image",
                  size: 4,
                },
              ],
            },
          ])
        : null,
    );
    const html = render();
    expect(html).toContain(
      "/api/v1/projects/demo/uploads/1780000000000-chart.png",
    );
    expect(html).not.toContain('src="data:');
    expect(html).not.toContain('src="blob:');
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

  it("composer effort pill reads `default` until the session reports a token", () => {
    // The conversation composer is LOCKED: its pill echoes what the session
    // reports (`GET /sessions/{sid}/status` → the `effortLabel` prop, printed
    // verbatim in zh and en). Nothing reported yet ⇒ `default`, never a
    // made-up rung — and never a ccteam-invented ladder word.
    expect(render()).toContain('<span class="eff">default</span>');
  });

  it("renders Pi's vendor identity, current /role state, and strict-HITL notice", () => {
    const html = render({
      ...SESSION,
      vendor: "pi",
      role: "reviewer",
      permission_mode: "hitl",
      protocol: "stream-json",
    });
    expect(html).toContain('data-vendor="pi"');
    expect(html).toContain('data-testid="session-role"');
    expect(html).toContain("reviewer");
    expect(html).toContain('data-testid="pi-hitl-tradeoff"');
  });

  it("shows the @host chip only for a remote session", () => {
    const strip = (h: string) => h.replace(/<!-- -->/g, "");
    const local = render({ ...SESSION, host: "local" });
    expect(strip(local)).not.toContain("@ local");
    const remote = render({ ...SESSION, host: "dev04" });
    expect(strip(remote)).toContain("@ dev04");
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

function findByTestId(value: unknown, testId: string): { props: Record<string, unknown> } | null {
  if (Array.isArray(value)) {
    for (const child of value) {
      const found = findByTestId(child, testId);
      if (found) return found;
    }
    return null;
  }
  if (!value || typeof value !== "object") return null;
  const props = (value as { props?: Record<string, unknown> }).props;
  if (!props) return null;
  if (props["data-testid"] === testId) return { props };
  return findByTestId(props.children, testId);
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

describe("SessionView paged history", () => {
  it("renders load-earlier, prepends the cursor page in order, then hides the affordance", async () => {
    const harness = createHookHarness();
    const history = vi
      .fn()
      .mockResolvedValueOnce({
        sid: "s9",
        events: [
          { turn_id: "t2", ts: "later", role: "cto", user: "new-user", assistant: "new-answer" },
        ],
        next_before: "cursor-1",
        has_more: true,
      })
      .mockResolvedValueOnce({
        sid: "s9",
        events: [
          { turn_id: "t1", ts: "earlier", role: "cto", user: "old-user", assistant: "old-answer" },
        ],
        next_before: null,
        has_more: false,
      });

    vi.resetModules();
    vi.doMock("react", async () => ({
      ...(await vi.importActual<typeof import("react")>("react")),
      ...harness.hooks,
    }));
    vi.doMock("../hooks/useSessionEvents", () => ({
      useSessionEvents: () => ({
        events: [],
        connected: true,
        connectionEpoch: 1,
        lastError: null,
        gatewayUnavailable: false,
      }),
    }));
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
      const PagedSessionView = (await import("./SessionView")).default;
      const renderPagedView = () =>
        harness.render(() => PagedSessionView({ sid: "s9", session: SESSION }));

      renderPagedView();
      await Promise.resolve();
      let tree = renderPagedView();
      const loadEarlier = findByTestId(tree, "load-earlier");
      expect(loadEarlier).not.toBeNull();
      expect(history).toHaveBeenNthCalledWith(1, "s9");

      (loadEarlier?.props.onClick as () => void)();
      await Promise.resolve();
      await Promise.resolve();
      tree = renderPagedView();

      expect(history).toHaveBeenNthCalledWith(2, "s9", { before: "cursor-1" });
      expect(findByTestId(tree, "load-earlier")).toBeNull();
      const text = collectElementText(tree);
      expect(text.indexOf("old-user")).toBeLessThan(text.indexOf("old-answer"));
      expect(text.indexOf("old-answer")).toBeLessThan(text.indexOf("new-user"));
      expect(text.indexOf("new-user")).toBeLessThan(text.indexOf("new-answer"));
    } finally {
      vi.doUnmock("react");
      vi.doUnmock("../hooks/useSessionEvents");
      vi.doUnmock("../lib/sessionsApi");
      vi.resetModules();
    }
  });
});
