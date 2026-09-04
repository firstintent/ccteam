// v0.8.24 Track A — Home landing page (prototype `#view-home`): SSR smoke.
// The lazy-create funnel's pure pieces (slugFromPath / wireProtocol /
// modelSwitchFor) are unit-tested in lib/vendors.test.ts; here we prove the
// page structure: 开工吧! + ctx-bar (项目/角色; 主机 hidden until real host
// data resolves; 分支 hidden — no backend data, never mocked) + composer +
// the 快速开始 template grid (recents live in the sidebar rail).
//
// v0.9.11 TEAM-3: the grid renders the shared 编队起手 formation playbooks
// (HomeView now reads router state → MemoryRouter wraps every render); the
// Team→Home handoff's applied patch is pure-helper-tested in
// lib/playbooks.test.ts (`applyPlaybook` / `playbookFromState`) because SSR
// renderToString never runs effects.

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

import HomeView, { NewProjectFields } from "./HomeView";
import type { HostSummary } from "../lib/hostsApi";

function render() {
  return renderToString(
    <MemoryRouter>
      <HomeView
        lang="zh"
        projects={["ccteam", "demo"]}
        projectPaths={{ ccteam: "~/rob/ccteam" }}
        onLaunched={() => {}}
        onOpenSettings={() => {}}
      />
    </MemoryRouter>,
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

  it("ctx-bar: 项目 + bound 主机 + 角色 render; 分支 hides without data", () => {
    const html = render();
    expect(html).toContain('data-testid="ctx-project"');
    expect(html).toContain('data-testid="ctx-role"');
    // Project identity is available before the shared host probe resolves.
    expect(html).toContain('data-testid="ctx-host"');
    // v0.8.24 Q7 — 分支 renders ONLY from real backend data (current_branch);
    // without it the dimension stays hidden (never mocked).
    expect(html).not.toContain('data-testid="ctx-branch"');
  });

  it("ctx-bar: 分支 shows READ-ONLY when the project reports current_branch", () => {
    const html = renderToString(
      <MemoryRouter>
        <HomeView
          lang="zh"
          projects={["ccteam"]}
          projectPaths={{ ccteam: "~/rob/ccteam" }}
          projectBranches={{ ccteam: "dev" }}
          onLaunched={() => {}}
          onOpenSettings={() => {}}
        />
      </MemoryRouter>,
    );
    expect(html).toContain('data-testid="ctx-branch"');
    const seg = html.slice(html.indexOf('data-testid="ctx-branch"'));
    expect(seg).toContain("dev");
    // Display-only: a <span>, not a dropdown trigger button.
    expect(seg.slice(0, 200)).not.toContain("<button");
  });

  it("角色 picker is available to tenants", () => {
    const tenant = render(false);
    expect(tenant).toContain('data-testid="ctx-role"');
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
    expect(html).toContain('data-testid="newproj-host"');
  });

  it("new-project fields render every eligible host and a remote absolute-path hint", () => {
    const hosts: HostSummary[] = [
      { host: "local", hostname: "box", is_local: true, status: "online", agent_count: 1, agents_ready: 1 },
      { host: "claude-dev-04", hostname: "dev04", is_local: false, status: "online", agent_count: 1, agents_ready: 1 },
    ];
    const html = renderToString(
      <NewProjectFields
        lang="en"
        open
        hosts={hosts}
        host="claude-dev-04"
        onHostChange={() => {}}
        onPathChange={() => {}}
        onCancel={() => {}}
      />,
    );
    expect(html).toContain('value="local"');
    expect(html).toContain('value="claude-dev-04" selected=""');
    expect(html).toContain('placeholder="Absolute path on claude-dev-04"');
  });

  it("project options wear their remote host and offline state", () => {
    const html = renderToString(
      <MemoryRouter>
        <HomeView
          lang="en"
          projects={["remote-proj"]}
          projectPaths={{ "remote-proj": "/srv/remote-proj" }}
          projectHosts={{ "remote-proj": { host: "sat-2", online: false } }}
          onLaunched={() => {}}
          onOpenSettings={() => {}}
        />
      </MemoryRouter>,
    );
    expect(html.replace(/<!-- -->/g, "")).toContain("@ sat-2");
    expect(html).toContain("offline");
    expect(html).toContain('disabled=""');
  });

  it("renders the 快速开始 grid: the 6 shared 编队起手 formation playbooks", () => {
    const html = render();
    expect(html).toContain('data-testid="template-grid"');
    expect(html).toContain("快速开始");
    for (const id of ["commander", "advisor", "crossreview", "bakeoff", "triangulate", "pyramid"]) {
      expect(html).toContain(`data-testid="tpl-${id}"`);
    }
    expect(html).toContain("总控-工班");
    expect(html).toContain("主力-顾问");
    expect(html).toContain("交叉互审");
    expect(html).toContain("并行竞标");
    expect(html).toContain("调研三角");
    expect(html).toContain("金字塔用工");
    // The card carries its composer prompt as the hover title; the commander
    // flagship's prompt drives real A2A delegation (the `agent` tool).
    expect(html).toContain("用 agent 派工");
    expect(html).toContain("择优合并成最终答案");
    // The old recents grid is gone (recents live in the sidebar rail), and
    // the retired single-vendor cards (code/fast/bulk era) don't resurface.
    expect(html).not.toContain('data-testid="recent-grid"');
    expect(html).not.toContain('data-testid="tpl-team"');
    expect(html).not.toContain('data-testid="tpl-code"');
  });

  it("formation cards wear their harness brand chips", () => {
    const html = render();
    const grid = html.slice(html.indexOf('data-testid="template-grid"'));
    // The deck spans all five harnesses.
    for (const vendor of ["claude", "codex", "grok", "kimi", "opencode"]) {
      expect(grid).toContain(`data-vendor="${vendor}"`);
    }
    // The 总控-工班 flagship fields the claude brain + codex/grok crews.
    const commander = grid.slice(
      grid.indexOf('data-testid="tpl-commander"'),
      grid.indexOf('data-testid="tpl-advisor"'),
    );
    for (const vendor of ["claude", "codex", "grok"]) {
      expect(commander).toContain(`data-vendor="${vendor}"`);
    }
    // 金字塔用工 leads cheap (kimi/opencode) and escalates to claude.
    const pyramid = grid.slice(grid.indexOf('data-testid="tpl-pyramid"'));
    for (const vendor of ["kimi", "opencode", "claude"]) {
      expect(pyramid).toContain(`data-vendor="${vendor}"`);
    }
  });

  it("template cards speak the shell language (en)", () => {
    const html = renderToString(
      <MemoryRouter>
        <HomeView
          lang="en"
          projects={["ccteam"]}
          projectPaths={{ ccteam: "~/rob/ccteam" }}
          onLaunched={() => {}}
          onOpenSettings={() => {}}
        />
      </MemoryRouter>,
    );
    expect(html).toContain("Quick start");
    expect(html).toContain("Commander + crews");
    expect(html).toContain("Driver + advisor");
    expect(html).toContain("Cross review");
    expect(html).toContain("Pyramid staffing");
  });

  it("mounts under the Team page 起手 handoff state (one-shot router state)", () => {
    // SSR never runs effects, so the applied composer patch itself is
    // pure-helper-tested in lib/playbooks.test.ts (applyPlaybook /
    // playbookFromState); this proves the page accepts the handoff entry.
    const html = renderToString(
      <MemoryRouter initialEntries={[{ pathname: "/", state: { playbook: "commander" } }]}>
        <HomeView
          lang="zh"
          projects={["ccteam"]}
          projectPaths={{ ccteam: "~/rob/ccteam" }}
          onLaunched={() => {}}
          onOpenSettings={() => {}}
        />
      </MemoryRouter>,
    );
    expect(html).toContain('data-testid="home-view"');
    expect(html).toContain('data-testid="tpl-commander"');
  });
});
