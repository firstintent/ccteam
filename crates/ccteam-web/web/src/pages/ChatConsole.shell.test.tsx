// v0.8.24 Track A — the prototype shell (`.app` = sidebar + main, NO
// full-width top bar; four mutually-exclusive views). SSR smoke via
// renderToString (no DOM harness in this repo): route → the right view, and
// the sidebar carries the prototype structure (logo → search → 新建 → 工作流 →
// 工作区 → 设置 → user), including the collapsed icon-rail whose ORDER matches
// the expanded column (the prototype CSS comment is the acceptance).

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
import { MemoryRouter, Route, Routes } from "react-router-dom";

import ChatConsole, { shellViewFor } from "./ChatConsole";

function routed(path: string) {
  return renderToString(
    <MemoryRouter initialEntries={[path]}>
      <Routes>
        <Route path="/" element={<ChatConsole />} />
        <Route path="/chat/s/:sid" element={<ChatConsole />} />
        <Route path="/flow" element={<ChatConsole />} />
        <Route path="/flow/:tab" element={<ChatConsole />} />
        <Route path="/settings" element={<ChatConsole />} />
        <Route path="/settings/:tab" element={<ChatConsole />} />
      </Routes>
    </MemoryRouter>,
  );
}

describe("shellViewFor routes the four views", () => {
  it("maps paths to home / conv / flow / settings", () => {
    expect(shellViewFor("/")).toBe("home");
    expect(shellViewFor("/chat/s/s9")).toBe("conv");
    expect(shellViewFor("/flow")).toBe("flow");
    expect(shellViewFor("/flow/evolution")).toBe("flow");
    expect(shellViewFor("/settings")).toBe("settings");
    expect(shellViewFor("/settings/market")).toBe("settings");
    expect(shellViewFor("/anything-else")).toBe("home");
  });
});

describe("ChatConsole shell (prototype .app layout)", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("renders the Home landing at / (lazy-create composer, no modal)", () => {
    const html = routed("/");
    expect(html).toContain('data-testid="app-shell"');
    expect(html).toContain('data-testid="home-view"');
    // 开工吧! + the ctx-bar + the composer's 随心输入 placeholder.
    expect(html).toMatch(/开工吧|Let's build/);
    expect(html).toContain('data-testid="ctx-bar"');
    expect(html).toContain("随心输入");
    // The retired NewSessionModal must be gone.
    expect(html).not.toContain("创建并切过去");
    expect(html).not.toContain("新建 session");
  });

  it("mounts the Conversation view (keyed SessionView) at /chat/s/:sid", () => {
    const html = routed("/chat/s/s9");
    expect(html).toContain('data-testid="conversation-view"');
    // conv composer placeholder (only the conversation view emits it).
    expect(html).toContain("Enter 发送");
    // sid chip in the conv-head meta.
    expect(html).toContain(">s9<");
    // Home's landing is NOT rendered simultaneously (views are exclusive).
    expect(html).not.toContain('data-testid="home-view"');
  });

  it("mounts 工作流 at /flow and 设置 at /settings (set-nav second column)", () => {
    const flow = routed("/flow");
    expect(flow).toContain('data-testid="workflow-view"');
    expect(flow).toContain('data-testid="workflow-tab-skills"');
    const settings = routed("/settings");
    expect(settings).toContain('data-testid="settings-view"');
    expect(settings).toContain('data-testid="set-item-market"');
  });

  it("has NO full-width top bar — nav lives in the sidebar", () => {
    const html = routed("/");
    expect(html).toContain('data-testid="sidebar"');
    expect(html).toContain('id="side-search"');
    expect(html).toContain("⌘K");
    expect(html).toContain('data-testid="side-new"');
    expect(html).toContain('data-testid="side-flow"');
    expect(html).toContain('data-testid="side-settings"');
    // Old top-bar remnants must not exist.
    expect(html).not.toContain("ccteam chat");
  });

  it("renders the mobile drawer chrome (hamburger + backdrop)", () => {
    const html = routed("/");
    expect(html).toContain('data-testid="hamb"');
    expect(html).toContain('data-testid="side-backdrop"');
  });

  it("collapsed icon rail keeps the expanded order: logo→expand→search→new→flow→blank→settings→avatar", () => {
    const html = routed("/");
    const mini = html.slice(html.indexOf('data-testid="side-mini"'));
    expect(mini.length).toBeGreaterThan(0);
    const order = [
      'data-testid="side-expand"',
      "搜索会话",
      "新建会话",
      "工作流",
      "mini-blank",
      "设置",
      'class="avatar"',
    ];
    let cursor = 0;
    for (const needle of order) {
      const at = mini.indexOf(needle, cursor);
      expect(at, `expected ${needle} in rail order`).toBeGreaterThan(-1);
      cursor = at;
    }
  });
});
