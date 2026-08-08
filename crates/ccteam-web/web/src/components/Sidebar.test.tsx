// v0.8.24 Track A — sidebar logic + SSR structure (prototype `.side`).

import { describe, expect, it } from "vitest";
import { renderToString } from "react-dom/server";

import {
  filterRows,
  groupRows,
  RemoveProjectDialog,
  rowStoppable,
  Sidebar,
  WS_SHOW,
  type RailRow,
} from "./Sidebar";

const row = (over: Partial<RailRow> = {}): RailRow => ({
  sid: "s1",
  project: "demo",
  label: "修复 SSE 断线重连",
  vendor: "claude",
  status: "live",
  ...over,
});

describe("filterRows (⌘K search haystack)", () => {
  const rows = [
    row({ sid: "s1", label: "修复 SSE 断线重连", model: "fable-5" }),
    row({ sid: "s2", label: "购物车结算 bug", project: "shop", vendor: "codex" }),
  ];

  it("matches on title / sid / project / model / vendor", () => {
    expect(filterRows(rows, "SSE").map((r) => r.sid)).toEqual(["s1"]);
    expect(filterRows(rows, "s2").map((r) => r.sid)).toEqual(["s2"]);
    expect(filterRows(rows, "shop").map((r) => r.sid)).toEqual(["s2"]);
    expect(filterRows(rows, "fable").map((r) => r.sid)).toEqual(["s1"]);
    expect(filterRows(rows, "codex").map((r) => r.sid)).toEqual(["s2"]);
  });

  it("returns everything for a blank query and nothing for a miss", () => {
    expect(filterRows(rows, "  ")).toHaveLength(2);
    expect(filterRows(rows, "zzz")).toHaveLength(0);
  });
});

describe("groupRows", () => {
  it("keeps a registered project with no sessions (chicken-and-egg fix)", () => {
    const groups = groupRows(["demo", "empty-proj"], [row()]);
    expect(groups.map((g) => g.project)).toEqual(["demo", "empty-proj"]);
    expect(groups[1]?.rows).toHaveLength(0);
  });

  it("adds an unregistered project that has live sessions", () => {
    const groups = groupRows(["demo"], [row({ project: "live-only" })]);
    expect(groups.map((g) => g.project)).toContain("live-only");
  });
});

describe("rowStoppable", () => {
  it("live rows get the hover stop; history rows don't", () => {
    expect(rowStoppable({ history: undefined })).toBe(true);
    expect(rowStoppable({ history: true })).toBe(false);
  });
});

function renderSidebar(rows: RailRow[], over: Partial<React.ComponentProps<typeof Sidebar>> = {}) {
  return renderToString(
    <Sidebar
      lang="zh"
      collapsed={false}
      mobileOpen={false}
      activeSid={null}
      projects={["demo"]}
      rows={rows}
      query=""
      flowActive={false}
      settingsActive={false}
      userName="rob"
      userInitial="R"
      onQuery={() => {}}
      onCollapse={() => {}}
      onNewSession={() => {}}
      onNewInProject={() => {}}
      onOpenFlow={() => {}}
      onOpenSettings={() => {}}
      onOpenRow={() => {}}
      onStopRow={() => {}}
      {...over}
    />,
  );
}

describe("Sidebar SSR structure", () => {
  it("renders all vendor chips on live and history session rows", () => {
    const html = renderSidebar(
      [
        row({ sid: "s1", vendor: "claude" }),
        row({ sid: "s2", vendor: "codex" }),
        row({ sid: "s3", vendor: "grok", history: true }),
        row({ sid: "s4", vendor: "opencode", history: true }),
        // Second workspace so the WS_SHOW row cap never folds this row away.
        row({ sid: "s5", vendor: "kimi", project: "demo2" }),
        row({ sid: "s6", vendor: "pi", project: "demo2" }),
      ],
      { projects: ["demo", "demo2"] },
    );
    for (const vendor of ["claude", "codex", "grok", "opencode", "kimi", "pi"]) {
      expect(html).toContain(`data-vendor="${vendor}"`);
      expect(html).toContain(`chip ${vendor} vendor-chip`);
    }
    const historyRows = html.match(/<div class="srow [^"]*hist[^"]*"[\s\S]*?<\/div>/g) ?? [];
    expect(historyRows).toHaveLength(2);
    expect(historyRows.join(" ")).toContain('data-vendor="grok"');
    expect(historyRows.join(" ")).toContain('data-vendor="opencode"');
  });

  it("caps each workspace at WS_SHOW rows and offers 展开显示(还有 N 个)", () => {
    const rows = Array.from({ length: WS_SHOW + 3 }, (_, i) =>
      row({ sid: `s${i}`, label: `任务 ${i}` }),
    );
    const html = renderSidebar(rows);
    expect(html).toContain("展开显示");
    expect(html).toContain("还有 3 个");
    // Only WS_SHOW rows rendered.
    expect(html).toContain("任务 0");
    expect(html).toContain(`任务 ${WS_SHOW - 1}`);
    expect(html).not.toContain(`任务 ${WS_SHOW}`);
  });

  it("marks the active row + renders the hover stop for live rows only", () => {
    const html = renderSidebar([row({ sid: "s1" }), row({ sid: "s2", history: true })], {
      activeSid: "s1",
    });
    expect(html).toContain("srow active");
    expect(html).toContain("停止(状态保留");
    // history row is dimmed (`hist`) and offers no stop button.
    expect(html).toContain("srow  hist");
  });

  it("offers rename on every row — live AND stopped — when the shell wires it", () => {
    const html = renderSidebar([row({ sid: "s1" }), row({ sid: "s2", history: true })], {
      onRenameRow: () => {},
    });
    // One pencil per row (a stopped session is renameable too).
    expect(html.match(/aria-label="重命名\(双击标题也可以\) s\d"/g) ?? []).toHaveLength(2);
    // The row tooltip advertises the double-click shortcut.
    expect(html).toContain("重命名(双击标题也可以)");
  });

  it("renders no rename affordance when the shell does not wire one", () => {
    const html = renderSidebar([row()]);
    expect(html).not.toContain("重命名");
  });

  it("puts the project-bound remote host on the group header and marks offline", () => {
    const html = renderSidebar([row()], {
      projectHosts: { demo: { host: "claude-dev-04", online: false } },
    });
    expect(html).toContain('class="project-host offline"');
    expect(html.replace(/<!-- -->/g, "")).toContain("@ claude-dev-04");
    expect(html).toContain("离线");
    expect(html).not.toContain("shost");
  });

  it("does not add a host chip for a local project", () => {
    const html = renderSidebar([row()], {
      projectHosts: { demo: { host: "local", online: true } },
    });
    expect(html).not.toContain("project-host");
  });

  it("renders the empty-workspace row (无会话)", () => {
    const html = renderSidebar([]);
    expect(html).toContain("无会话");
  });

  it("always shows the 团队/Team nav entry in the expanded sidebar", () => {
    const html = renderSidebar([]);
    expect(html).toContain('data-testid="side-team"');
    expect(html).toContain("团队");
  });

  it("always shows the rail entry and reflects the active team route", () => {
    const html = renderSidebar([], { teamActive: true });
    expect(html).toContain('data-testid="side-team"');
    expect(html).toContain('data-testid="side-team-rail"');
    expect(html).toContain("团队");
    // Active state reflected on the expanded button.
    expect(html).toMatch(/class="sflow active"[^>]*data-testid="side-team"/);
  });

  it("every workspace header carries the ⋯ menu button (closed by default)", () => {
    const html = renderSidebar([row()], { onRemoveProject: async () => true });
    expect(html).toContain('data-testid="ws-menu-btn-demo"');
    // The dropdown itself only mounts on click.
    expect(html).not.toContain('data-testid="ws-menu-demo"');
    expect(html).not.toContain('data-testid="project-remove-dialog-demo"');
  });
});

describe("RemoveProjectDialog (⋯ → 从 ccteam 移除)", () => {
  it("states catalog-only semantics and gates 确认 on the typed slug", () => {
    const mistyped = renderToString(
      <RemoveProjectDialog
        lang="zh"
        project="demo"
        path="/home/u/demo"
        typed="dem"
        busy={false}
        onTyped={() => {}}
        onCancel={() => {}}
        onConfirm={() => {}}
      />,
    );
    expect(mistyped).toContain('data-testid="project-remove-dialog-demo"');
    // The whole safety story: disk files stay, sessions stop, re-addable.
    expect(mistyped).toContain("磁盘上的目录与代码不动");
    expect(mistyped).toContain("/home/u/demo");
    const goBtn = mistyped.slice(mistyped.indexOf('data-testid="project-remove-go"'));
    expect(goBtn.slice(0, goBtn.indexOf(">"))).toContain("disabled");

    const exact = renderToString(
      <RemoveProjectDialog
        lang="zh"
        project="demo"
        typed="demo"
        busy={false}
        onTyped={() => {}}
        onCancel={() => {}}
        onConfirm={() => {}}
      />,
    );
    const goBtn2 = exact.slice(exact.indexOf('data-testid="project-remove-go"'));
    expect(goBtn2.slice(0, goBtn2.indexOf(">"))).not.toContain("disabled");
  });
});
