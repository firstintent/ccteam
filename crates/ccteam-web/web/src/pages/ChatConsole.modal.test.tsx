// v0.8.8 bug — a project created via CLI `ccteam init` (registered in
// config.yaml, NO session yet) must still appear in the web new-session
// modal's project dropdown, so its FIRST session can be created. Before the
// fix the project list was derived PURELY from sessions, so a session-less
// registered project was invisible (chicken-and-egg).
//
// Two layers, mirroring the existing sessionsApi/StatusView test patterns:
//   1. unit-test the pure `mergeProjectSlugs` union helper (no DOM/React);
//   2. renderToString the `NewSessionModal` (no DOM env) and assert a
//      session-less registered project is listed as a <select> <option>.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

// ChatConsole's import chain reaches useWebSettings, which reads
// `window.innerWidth` at module-eval time. These tests run under the node env
// (no DOM, mirroring the other renderToString tests), so stub a minimal
// `window` BEFORE the static imports load. `vi.hoisted` runs above imports.
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

import { NewSessionModal, relativeTimeZh } from "./ChatConsole";
import { mergeProjectSlugs } from "./projectList";
import type { SessionView } from "../lib/sessionsApi";

// ---- relativeTimeZh (v0.8.22 P0-3/P0-4 — history rail + IM `/sessions`
// share this phrasing; mirrors `ccteam-im::gateway::relative_time_zh`) ------
describe("relativeTimeZh", () => {
  it("renders an em-dash for missing/unparseable input", () => {
    expect(relativeTimeZh(undefined)).toBe("—");
    expect(relativeTimeZh(null)).toBe("—");
    expect(relativeTimeZh("")).toBe("—");
    expect(relativeTimeZh("not-a-timestamp")).toBe("—");
  });

  it("buckets recent timestamps into 刚刚 / N分钟前 / N小时前", () => {
    const secondsAgo = (s: number) => new Date(Date.now() - s * 1000).toISOString();
    expect(relativeTimeZh(secondsAgo(10))).toBe("刚刚");
    expect(relativeTimeZh(secondsAgo(5 * 60))).toBe("5分钟前");
    expect(relativeTimeZh(secondsAgo(3 * 3600))).toBe("3小时前");
  });

  it("special-cases yesterday and buckets multi-day/week spans", () => {
    const secondsAgo = (s: number) => new Date(Date.now() - s * 1000).toISOString();
    expect(relativeTimeZh(secondsAgo(24 * 3600))).toBe("昨天");
    expect(relativeTimeZh(secondsAgo(3 * 24 * 3600))).toBe("3天前");
    expect(relativeTimeZh(secondsAgo(14 * 24 * 3600))).toBe("2周前");
  });

  it("falls back to an absolute date at >= 5 weeks", () => {
    const old = new Date(Date.now() - 40 * 24 * 3600 * 1000);
    expect(relativeTimeZh(old.toISOString())).toBe(old.toISOString().slice(0, 10));
  });
});

// ---- mergeProjectSlugs (the union that fixes the chicken-and-egg bug) ------
describe("mergeProjectSlugs", () => {
  it("lists a registered project even with NO sessions (the bug)", () => {
    // demo2 was just `ccteam init`-ed: registered, but no session yet.
    expect(mergeProjectSlugs(["demo2"], [])).toEqual(["demo2"]);
  });

  it("unions registered projects with session projects, sorted + de-duped", () => {
    const sessions: Pick<SessionView, "project">[] = [
      { project: "alpha" }, // registered AND has a session
      { project: "zeta" }, // has a session but is NOT registered
      { project: "alpha" }, // dup (a second session in alpha)
    ];
    expect(mergeProjectSlugs(["alpha", "demo2"], sessions)).toEqual([
      "alpha",
      "demo2",
      "zeta",
    ]);
  });

  it("returns [] when nothing is registered and there are no sessions", () => {
    expect(mergeProjectSlugs([], [])).toEqual([]);
  });

  it("falls back to session projects when the registered list is empty", () => {
    // Defensive: if /api/v1/projects hasn't resolved yet, sessions still show.
    expect(mergeProjectSlugs([], [{ project: "live-only" }])).toEqual(["live-only"]);
  });
});

// ---- NewSessionModal lists a session-less registered project ---------------
describe("NewSessionModal project dropdown", () => {
  beforeEach(() => {
    // The modal fetches roles in a useEffect; effects don't run under
    // renderToString, but stub fetch to a never-resolving promise so nothing
    // can fire a real request even if the runtime changes.
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("renders an <option> for a session-less registered project", () => {
    // `projects` is the merged list the parent now passes (registered ∪
    // sessions). A freshly-init'ed `demo2` with no session is included.
    const html = renderToString(
      <NewSessionModal
        projects={["demo", "demo2"]}
        fallbackRoles={["cto"]}
        defaultProject="demo2"
        isAdmin={true}
        onCancel={() => {}}
        onCreate={async () => true}
      />,
    );
    // The session-less project is selectable in the dropdown → the user can
    // pick it + create its first session.
    expect(html).toContain('value="demo2"');
    expect(html).toContain(">demo2<");
    // The "create a new project" sentinel is still offered alongside it.
    expect(html).toContain("＋ 新建项目…");
    // The "no existing projects" placeholder must NOT show when we have one.
    expect(html).not.toContain("（暂无已有项目）");
  });

  it("shows ALL runtimes + the role picker to the admin", () => {
    const html = renderToString(
      <NewSessionModal
        projects={["demo"]}
        fallbackRoles={["cto"]}
        defaultProject="demo"
        isAdmin={true}
        onCancel={() => {}}
        onCreate={async () => true}
      />,
    );

    expect(html).toContain("运行时");
    expect(html).toContain("Claude · stream-json");
    expect(html).toContain("Claude · terminal");
    expect(html).toContain("Codex · app-server");
    expect(html).toContain("Codex · terminal");
    // The admin gets the role picker (the `<label>Role</label>` renders).
    expect(html).toContain(">Role<");
    expect(html).toContain("permission=");
    expect(html).not.toContain("Claude Code · tmux");
    expect(html).not.toContain(" mode=");
  });

  // v0.8.20 F4 — beta-gating (UI only): a tenant sees only the production-stable
  // claude/codex stream-json runtimes and creates roleless sessions.
  it("hides terminal runtimes + the role picker from a tenant", () => {
    const html = renderToString(
      <NewSessionModal
        projects={["demo"]}
        fallbackRoles={["cto"]}
        defaultProject="demo"
        isAdmin={false}
        onCancel={() => {}}
        onCreate={async () => true}
      />,
    );

    // Production-stable runtimes (claude + codex, both stream-json).
    expect(html).toContain("Claude · stream-json");
    expect(html).toContain("Codex · app-server");
    // Terminal/rmux runtimes are admin-only.
    expect(html).not.toContain("Claude · terminal");
    expect(html).not.toContain("Codex · terminal");
    // No role picker → the tenant always creates a roleless session.
    expect(html).not.toContain(">Role<");
  });
});
