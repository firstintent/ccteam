// v0.8.24 Track A — sidebar logic + SSR structure (prototype `.side`).

import { describe, expect, it } from "vitest";
import { renderToString } from "react-dom/server";

import {
  filterRows,
  groupRows,
  rowStoppable,
  runningHosts,
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
    row({ sid: "s1", label: "修复 SSE 断线重连", model: "fable-5", host: "dev01" }),
    row({ sid: "s2", label: "购物车结算 bug", project: "shop", vendor: "codex" }),
  ];

  it("matches on title / sid / project / model / host / vendor", () => {
    expect(filterRows(rows, "SSE").map((r) => r.sid)).toEqual(["s1"]);
    expect(filterRows(rows, "s2").map((r) => r.sid)).toEqual(["s2"]);
    expect(filterRows(rows, "shop").map((r) => r.sid)).toEqual(["s2"]);
    expect(filterRows(rows, "fable").map((r) => r.sid)).toEqual(["s1"]);
    expect(filterRows(rows, "dev01").map((r) => r.sid)).toEqual(["s1"]);
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

describe("runningHosts", () => {
  it("collects distinct hosts of live rows, defaulting to local", () => {
    const rows = [
      row({ sid: "s1" }), // no host → local
      row({ sid: "s2", host: "dev01" }),
      row({ sid: "s3", host: "dev01" }),
    ];
    expect(runningHosts(rows).sort()).toEqual(["dev01", "local"]);
  });

  it("ignores history rows (a stopped session runs nowhere)", () => {
    const rows = [row({ sid: "s1", host: "dev01" }), row({ sid: "s2", host: "gpu02", history: true })];
    expect(runningHosts(rows)).toEqual(["dev01"]);
    expect(runningHosts([row({ sid: "s9", history: true })])).toEqual([]);
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

  it("shows the workspace header with host + running dot", () => {
    const html = renderSidebar([row({ host: "dev01" })]);
    expect(html).toContain("wagent");
    expect(html).toContain("dev01");
    expect(html).toContain('class="wname"');
  });

  // A project is ONE logical unit whose sessions may run on several hosts —
  // the header must not claim the first row's host for the whole group.
  it("mixed-host group: header shows a count, rows carry host badges", () => {
    const html = renderSidebar([
      row({ sid: "s1" }), // local
      row({ sid: "s2", host: "dev01", label: "远程任务" }),
    ]);
    expect(html).toContain("2 台主机");
    // Per-row badge only for the non-local row.
    expect(html).toContain('class="shost"');
    expect(html).toContain("dev01");
  });

  it("all-local group: header says local, no per-row badges", () => {
    const html = renderSidebar([row({ sid: "s1" }), row({ sid: "s2" })]);
    expect(html).toContain("local");
    expect(html).not.toContain('class="shost"');
  });

  it("renders the empty-workspace row (无会话)", () => {
    const html = renderSidebar([]);
    expect(html).toContain("无会话");
  });

  // v0.9.0 W4 — 团队/Team nav entry is admin-only (beta-gate); absent by
  // default (a tenant / not-yet-resolved `useMe` never sees it).
  it("hides the 团队/Team nav entry by default (showTeam unset)", () => {
    const html = renderSidebar([]);
    expect(html).not.toContain('data-testid="side-team"');
    expect(html).not.toContain('data-testid="side-team-rail"');
  });

  it("shows the 团队/Team nav entry (expanded + rail) when showTeam is set", () => {
    const html = renderSidebar([], { showTeam: true, teamActive: true });
    expect(html).toContain('data-testid="side-team"');
    expect(html).toContain('data-testid="side-team-rail"');
    expect(html).toContain("团队");
    // Active state reflected on the expanded button.
    expect(html).toMatch(/class="sflow active"[^>]*data-testid="side-team"/);
  });
});
