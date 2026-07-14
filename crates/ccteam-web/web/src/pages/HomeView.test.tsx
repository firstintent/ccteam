// v0.8.24 Track A — Home landing page (prototype `#view-home`): SSR smoke.
// The lazy-create funnel's pure pieces (slugFromPath / wireProtocol /
// modelSwitchFor) are unit-tested in lib/vendors.test.ts; here we prove the
// page structure: 开工吧! + ctx-bar (项目/角色; 主机 hidden until real host
// data resolves; 分支 hidden — no backend data, never mocked) + composer +
// 最近会话 grid.

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

import HomeView, { MAX_ACTIVE_SESSIONS, type RecentEntry } from "./HomeView";

const RECENTS: RecentEntry[] = [
  {
    sid: "s41",
    label: "修复 SSE 断线重连",
    project: "ccteam",
    vendor: "claude",
    host: "dev01",
    status: "working",
    lastActive: new Date(Date.now() - 12 * 60 * 1000).toISOString(),
  },
  {
    sid: "s35",
    label: "grok ACP 冒烟",
    project: "ccteam",
    vendor: "grok",
    history: true,
    lastActive: new Date(Date.now() - 3 * 24 * 3600 * 1000).toISOString(),
  },
];

function render(recents: RecentEntry[] = RECENTS, isAdmin = true) {
  return renderToString(
    <HomeView
      lang="zh"
      isAdmin={isAdmin}
      projects={["ccteam", "demo"]}
      projectPaths={{ ccteam: "~/rob/ccteam" }}
      liveCount={1}
      recents={recents}
      onLaunched={() => {}}
      onOpenRecent={() => {}}
      onOpenSettings={() => {}}
    />,
  );
}

describe("HomeView (landing page)", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("renders 开工吧! + the lazy-create subtitle", () => {
    const html = render();
    expect(html).toContain('data-testid="home-view"');
    expect(html).toContain("开工吧!");
    expect(html).toContain("会话在第一条消息发出时创建");
  });

  it("ctx-bar: 项目 + 角色 render; 主机 hidden before host data; 分支 hidden without data", () => {
    const html = render();
    expect(html).toContain('data-testid="ctx-project"');
    expect(html).toContain('data-testid="ctx-role"');
    // getHosts() hasn't resolved (never-resolving fetch) → dimension hidden.
    expect(html).not.toContain('data-testid="ctx-host"');
    // v0.8.24 Q7 — 分支 renders ONLY from real backend data (current_branch);
    // without it the dimension stays hidden (never mocked).
    expect(html).not.toContain('data-testid="ctx-branch"');
  });

  it("ctx-bar: 分支 shows READ-ONLY when the project reports current_branch", () => {
    const html = renderToString(
      <HomeView
        lang="zh"
        isAdmin
        projects={["ccteam"]}
        projectPaths={{ ccteam: "~/rob/ccteam" }}
        projectBranches={{ ccteam: "dev" }}
        liveCount={0}
        recents={[]}
        onLaunched={() => {}}
        onOpenRecent={() => {}}
        onOpenSettings={() => {}}
      />,
    );
    expect(html).toContain('data-testid="ctx-branch"');
    const seg = html.slice(html.indexOf('data-testid="ctx-branch"'));
    expect(seg).toContain("dev");
    // Display-only: a <span>, not a dropdown trigger button.
    expect(seg.slice(0, 200)).not.toContain("<button");
  });

  it("角色 picker is an admin-only beta surface (tenant launches roleless)", () => {
    const tenant = render(RECENTS, false);
    expect(tenant).not.toContain('data-testid="ctx-role"');
    expect(tenant).toContain('data-testid="ctx-project"');
  });

  it("composer carries the HITL pill + model button + 随心输入 placeholder", () => {
    const html = render();
    expect(html).toContain('data-testid="hitl-toggle"');
    expect(html).toContain("请求批准");
    expect(html).toContain('data-testid="model-btn"');
    expect(html).toContain("随心输入");
    expect(html).toContain('data-testid="home-send"');
  });

  it("shows the inline 新建项目 row (hidden until opened) with the path input", () => {
    const html = render();
    expect(html).toContain('data-testid="newproj"');
    expect(html).toContain("新建项目路径");
    expect(html).toContain('id="newproj-path"');
  });

  it("renders 最近会话 cards (4-way vendor chips + sid + host·time)", () => {
    const html = render();
    expect(html).toContain('data-testid="recent-grid"');
    expect(html).toContain("修复 SSE 断线重连");
    expect(html).toContain("chip claude");
    expect(html).toContain("chip grok");
    expect(html).toContain('data-vendor="claude"');
    expect(html).toContain('data-vendor="grok"');
    expect(html).toContain(">s41<");
    expect(html).toContain("dev01");
    // A stopped session card shows the off dot.
    expect(html).toContain("dot off");
  });

  it("caps the grid at 4 cards", () => {
    const many = Array.from({ length: 6 }, (_, i) => ({
      ...RECENTS[0]!,
      sid: `s${i}`,
      label: `卡片 ${i}`,
    }));
    const html = render(many);
    expect(html).toContain("卡片 0");
    expect(html).toContain("卡片 3");
    expect(html).not.toContain("卡片 4");
  });

  it("keeps the soft session cap exported for the launch gate", () => {
    expect(MAX_ACTIVE_SESSIONS).toBe(10);
  });
});
