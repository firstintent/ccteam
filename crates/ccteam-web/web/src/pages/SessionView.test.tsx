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

import SessionView, { RowTime } from "./SessionView";
import { rowsKeyFor } from "./chatTranscript";
import type { SessionView as SessionSummary } from "../lib/sessionsApi";
import type { SessionEvent } from "../hooks/useSessionEvents";

const SESSION: SessionSummary = {
  sid: "s9",
  project: "demo",
  role: "cto",
  vendor: "claude",
  permission_mode: "skip",
  current: true,
  // Two axes: the rail's `status` carries the ACTIVITY word, `residency`
  // says whether ccteam is holding a process — the dot reads the latter.
  status: "idle",
  residency: "resident",
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
    // Spread the real module: SessionView imports foldSessionLiveness too.
    vi.doMock("../hooks/useSessionEvents", async () => ({
      ...(await vi.importActual<typeof import("../hooks/useSessionEvents")>(
        "../hooks/useSessionEvents",
      )),
      useSessionEvents: () => stream,
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
    // Spread the real module: SessionView imports foldSessionLiveness too.
    vi.doMock("../hooks/useSessionEvents", async () => ({
      ...(await vi.importActual<typeof import("../hooks/useSessionEvents")>(
        "../hooks/useSessionEvents",
      )),
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

  it("discards a stale load-earlier page after reconnect reseed and accepts the fresh cursor", async () => {
    const harness = createHookHarness();
    let stream = {
      events: [],
      connected: true,
      connectionEpoch: 1,
      lastError: null,
      gatewayUnavailable: false,
    };
    let resolveStale: (value: unknown) => void = () => {};
    let resolveReseed: (value: unknown) => void = () => {};
    const stalePage = new Promise((resolve) => {
      resolveStale = resolve;
    });
    const reseedPage = new Promise((resolve) => {
      resolveReseed = resolve;
    });
    const history = vi
      .fn()
      .mockResolvedValueOnce({
        sid: "s9",
        events: [
          { turn_id: "t3", ts: "new", role: "cto", user: "seed-user", assistant: "seed-answer" },
        ],
        next_before: "cursor-old",
        has_more: true,
      })
      .mockReturnValueOnce(stalePage)
      .mockReturnValueOnce(reseedPage)
      .mockResolvedValueOnce({
        sid: "s9",
        events: [
          {
            turn_id: "t1",
            ts: "old",
            role: "cto",
            user: "fresh-old-user",
            assistant: "fresh-old-answer",
          },
        ],
        next_before: null,
        has_more: false,
      });

    vi.resetModules();
    vi.doMock("react", async () => ({
      ...(await vi.importActual<typeof import("react")>("react")),
      ...harness.hooks,
    }));
    // Spread the real module: SessionView imports foldSessionLiveness too.
    vi.doMock("../hooks/useSessionEvents", async () => ({
      ...(await vi.importActual<typeof import("../hooks/useSessionEvents")>(
        "../hooks/useSessionEvents",
      )),
      useSessionEvents: () => stream,
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
      (findByTestId(tree, "load-earlier")?.props.onClick as () => void)();
      expect(history).toHaveBeenNthCalledWith(2, "s9", { before: "cursor-old" });

      stream = { ...stream, connectionEpoch: 2 };
      renderPagedView();
      resolveReseed({
        sid: "s9",
        events: [
          {
            turn_id: "t4",
            ts: "newest",
            role: "cto",
            user: "reseed-user",
            assistant: "reseed-answer",
          },
        ],
        next_before: "cursor-new",
        has_more: true,
      });
      await Promise.resolve();
      await Promise.resolve();
      tree = renderPagedView();
      expect(collectElementText(tree)).toContain("reseed-user");

      resolveStale({
        sid: "s9",
        events: [
          {
            turn_id: "stale",
            ts: "stale",
            role: "cto",
            user: "stale-user",
            assistant: "stale-answer",
          },
        ],
        next_before: "stale-cursor",
        has_more: true,
      });
      await Promise.resolve();
      await Promise.resolve();
      tree = renderPagedView();
      expect(collectElementText(tree)).not.toContain("stale-user");
      expect(collectElementText(tree)).toContain("reseed-user");

      (findByTestId(tree, "load-earlier")?.props.onClick as () => void)();
      await Promise.resolve();
      await Promise.resolve();
      tree = renderPagedView();
      expect(history).toHaveBeenNthCalledWith(4, "s9", { before: "cursor-new" });
      const text = collectElementText(tree);
      expect(text.indexOf("fresh-old-user")).toBeLessThan(text.indexOf("reseed-user"));
      expect(findByTestId(tree, "load-earlier")).toBeNull();
    } finally {
      vi.doUnmock("react");
      vi.doUnmock("../hooks/useSessionEvents");
      vi.doUnmock("../lib/sessionsApi");
      vi.resetModules();
    }
  });
});

describe("SessionView header status dot (WEB-STATUS-1)", () => {
  interface StreamState {
    events: SessionEvent[];
    connected: boolean;
    connectionEpoch: number;
    lastError: string | null;
    gatewayUnavailable: boolean;
  }

  const healthyStream = (): StreamState => ({
    events: [],
    connected: true,
    connectionEpoch: 1,
    lastError: null,
    gatewayUnavailable: false,
  });

  /** Mount SessionView against a mutable per-sid stream box; `render()`
   *  re-renders the SAME mounted instance (no reload/remount). */
  async function mountDotView(box: { stream: StreamState }, session: SessionSummary | null) {
    const harness = createHookHarness();
    vi.resetModules();
    vi.doMock("react", async () => ({
      ...(await vi.importActual<typeof import("react")>("react")),
      ...harness.hooks,
    }));
    vi.doMock("../hooks/useSessionEvents", async () => ({
      // The real foldSessionLiveness — only the stream itself is stubbed.
      ...(await vi.importActual<typeof import("../hooks/useSessionEvents")>(
        "../hooks/useSessionEvents",
      )),
      useSessionEvents: () => box.stream,
    }));
    vi.doMock("../lib/sessionsApi", async () => ({
      ...(await vi.importActual<typeof import("../lib/sessionsApi")>("../lib/sessionsApi")),
      getHistory: vi.fn().mockResolvedValue({ sid: "s9", events: [] }),
      getSessionStatus: vi.fn().mockResolvedValue({
        sid: "s9",
        model: null,
        context: null,
        status_line: null,
      }),
    }));
    const View = (await import("./SessionView")).default;
    return () => harness.render(() => View({ sid: "s9", session }));
  }

  function unmockDotView() {
    vi.doUnmock("react");
    vi.doUnmock("../hooks/useSessionEvents");
    vi.doUnmock("../lib/sessionsApi");
    vi.resetModules();
  }

  const dotClass = (tree: unknown) => findByTestId(tree, "conv-dot")?.props.className;

  it("reads SESSION state (REST base + lifecycle frames), never the SSE connection", async () => {
    const box = { stream: healthyStream() };
    try {
      const render = await mountDotView(box, SESSION); // SESSION is resident
      let tree = render();
      // Resident session + healthy stream → green. NOTE the base is
      // `residency`, not the activity word: an idle-but-resident session is
      // green, which is exactly what `status === "live"` could never say once
      // the list endpoint started reporting real activity.
      expect(dotClass(tree)).toBe("dot on");

      // A capacity-eviction lifecycle frame greys the dot IMMEDIATELY — same
      // mounted instance, no reload, no rail REST reconcile.
      box.stream = {
        ...box.stream,
        events: [
          {
            kind: "session_lifecycle",
            content: "session evicted: s9",
            state: "evicted",
            reason: "capacity",
          },
        ],
      };
      tree = render();
      expect(dotClass(tree)).toBe("dot off");

      // An opinion-less lifecycle frame (rename) must not resurrect the dot.
      box.stream = {
        ...box.stream,
        events: [...box.stream.events, { kind: "session_lifecycle", content: "", state: "renamed" }],
      };
      tree = render();
      expect(dotClass(tree)).toBe("dot off");
    } finally {
      unmockDotView();
    }
  });

  it("seeds grey from a non-resident session's REST row, with no frame at all", async () => {
    const box = { stream: healthyStream() };
    try {
      const render = await mountDotView(box, { ...SESSION, residency: "released" });
      expect(dotClass(render())).toBe("dot off");
    } finally {
      unmockDotView();
    }
  });

  it("keeps a broken stream as its OWN red dot — the session fact is untouched", async () => {
    const box = {
      stream: { ...healthyStream(), connected: false, lastError: "SSE max retries reached" },
    };
    try {
      const render = await mountDotView(box, SESSION);
      const tree = render();
      expect(dotClass(tree)).toBe("dot on"); // session is still resident
      const connDot = findByTestId(tree, "conn-dot");
      expect(connDot?.props.className).toBe("dot err");
      expect(connDot?.props.title).toBe("连接已断开");
    } finally {
      unmockDotView();
    }
  });
});

describe("RowTime date visibility", () => {
  it("shows time only for today, date within the year, full date for older", () => {
    const now = new Date();
    const today = new Date(now.getFullYear(), now.getMonth(), now.getDate(), 9, 5);
    const yesterday = new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1, 9, 5);
    const lastYear = new Date(now.getFullYear() - 1, 5, 15, 9, 5);

    const textOf = (html: string) => html.match(/>([^<]*)<\/time>/)?.[1] ?? "";

    const todayHtml = renderToString(<RowTime ts={today.toISOString()} lang="en" />);
    expect(textOf(todayHtml)).toBe("09:05");

    const yesterdayHtml = renderToString(<RowTime ts={yesterday.toISOString()} lang="en" />);
    expect(textOf(yesterdayHtml)).toMatch(/\d{2}\/\d{2}.{0,3}09:05/);
    expect(textOf(yesterdayHtml)).not.toContain(String(now.getFullYear()));

    const lastYearHtml = renderToString(<RowTime ts={lastYear.toISOString()} lang="en" />);
    expect(textOf(lastYearHtml)).toContain(String(now.getFullYear() - 1));
    expect(textOf(lastYearHtml)).toContain("09:05");

    // zh locale carries the date too.
    const zhHtml = renderToString(<RowTime ts={yesterday.toISOString()} lang="zh" />);
    expect(textOf(zhHtml)).toMatch(/\d{2}\/\d{2}/);
  });

  it("renders nothing for absent or unparseable ts", () => {
    expect(renderToString(<RowTime lang="en" />)).toBe("");
    expect(renderToString(<RowTime ts="not-a-date" lang="en" />)).toBe("");
  });
});
