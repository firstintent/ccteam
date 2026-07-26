// v0.8.9 Phase 4 — MarketplaceView smoke tests.
//
// No DOM env (no jsdom): use React's `renderToString` to assert the initial
// HTML shape (mirrors SettingsPage.test.tsx). The success / interactive paths
// (project picker, install POST → re-fetch, drawer body) need async fetches
// that renderToString won't await; we cover:
//   - the loading shell before any fetch resolves
//   - the seeded sub-components (PluginCard / InstallButton) render their
//     installed_status-driven affordance (已装 vs 安装/更新)
// The fetch wiring + error mapping is covered by marketplaceApi.test.ts, and
// the filter/format logic by marketplaceFormat.test.ts.

import { describe, expect, it, vi, afterEach, beforeEach } from "vitest";

vi.hoisted(() => {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const g = globalThis as any;
  // useWebSettings reads window/localStorage at module scope (node env: stub).
  if (typeof g.window === "undefined") {
    g.window = { innerWidth: 1024, addEventListener() {}, removeEventListener() {} };
  }
  if (typeof g.localStorage === "undefined") {
    g.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };
  }
});

import { renderToString } from "react-dom/server";
import { MemoryRouter } from "react-router-dom";
import MarketplaceView, { InstallButton, PluginCard } from "./MarketplaceView";
import type { HubPlugin, InstalledStatus } from "../lib/marketplaceApi";
import { CATEGORIES, needsProjectTarget } from "../lib/marketplaceFormat";

const realFetch = globalThis.fetch;

function plugin(over: Partial<HubPlugin> & { installed_status?: InstalledStatus } = {}) {
  return {
    id: "code-reviewer",
    type: "agent" as const,
    name: "Code Reviewer",
    description: "line-by-line review",
    path: "agents/code-reviewer.md",
    content_sha: "abc",
    source: "agency-agents",
    upstream: "https://github.com/x",
    license: "MIT",
    tags: ["review"],
    ...over,
  };
}

describe("MarketplaceView initial render", () => {
  beforeEach(() => {
    // Never-resolving fetch keeps the projects + catalog in their loading state.
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("renders the view + the loading placeholder before fetches resolve", () => {
    const html = renderToString(
      <MemoryRouter>
        <MarketplaceView />
      </MemoryRouter>,
    );
    expect(html).toContain('data-testid="marketplace-view"');
    expect(html).toContain('data-testid="marketplace-loading"');
    // The category seg tabs + search box render immediately (static chrome).
    expect(html).toContain("Skills");
    expect(html.indexOf("Skills")).toBeLessThan(html.indexOf("Agents / Roles"));
    expect(html).not.toContain('data-testid="market-project-picker"');
    expect(html).toContain("全部来源");
  });
});

describe("marketplace category install target semantics", () => {
  it("orders skills first and defaults the rendered tab to skill", () => {
    expect(CATEGORIES.map((c) => c.type)).toEqual(["skill", "agent", "workflow", "plugin"]);
  });

  it("shows the project target only for agent/plugin categories", () => {
    expect(needsProjectTarget("agent")).toBe(true);
    expect(needsProjectTarget("plugin")).toBe(true);
    expect(needsProjectTarget("skill")).toBe(false);
    expect(needsProjectTarget("workflow")).toBe(false);
  });
});

describe("PluginCard / InstallButton (seeded)", () => {
  it("shows an 已装 pill (not a button) when installed", () => {
    const html = renderToString(
      <InstallButton status="installed" installing={false} canInstall onInstall={() => {}} />,
    );
    expect(html).toContain("已装");
    expect(html).not.toContain("<button");
  });

  it("shows an 安装 button for not_installed", () => {
    const html = renderToString(
      <InstallButton status="not_installed" installing={false} canInstall onInstall={() => {}} />,
    );
    expect(html).toContain("<button");
    expect(html).toContain("安装");
  });

  it("shows an 更新 button for update_available", () => {
    const html = renderToString(
      <InstallButton
        status="update_available"
        installing={false}
        canInstall
        onInstall={() => {}}
      />,
    );
    expect(html).toContain("更新");
  });

  it("disables the install button (with hint) when no install target", () => {
    const html = renderToString(
      <InstallButton
        status="not_installed"
        installing={false}
        canInstall={false}
        onInstall={() => {}}
      />,
    );
    expect(html).toContain("disabled");
    expect(html).toContain("先选一个安装目标项目");
  });

  it("renders a card with the plugin name, source badge, license + tags", () => {
    const html = renderToString(
      <PluginCard
        plugin={plugin({ installed_status: "not_installed" })}
        installing={false}
        canInstall
        onOpen={() => {}}
        onInstall={() => {}}
      />,
    );
    expect(html).toContain("Code Reviewer");
    expect(html).toContain("agency-agents");
    expect(html).toContain("MIT");
    expect(html).toContain("#review");
    expect(html).toContain("安装");
  });
});

// v0.9.9 — type=skill entries install into the user-level GLOBAL LIBRARY:
// the CTA copy names the library (zh + en); agent/plugin CTAs are unchanged.
describe("skill CTA — install-to-library copy (three states, zh + en)", () => {
  it("not_installed → 安装到库 / Install to library", () => {
    const zh = renderToString(
      <InstallButton status="not_installed" installing={false} canInstall onInstall={() => {}} skill lang="zh" />,
    );
    expect(zh).toContain("<button");
    expect(zh).toContain("安装到库");
    const en = renderToString(
      <InstallButton status="not_installed" installing={false} canInstall onInstall={() => {}} skill lang="en" />,
    );
    expect(en).toContain("Install to library");
  });

  it("installed → 已在库 / In library pill (not a button)", () => {
    const zh = renderToString(
      <InstallButton status="installed" installing={false} canInstall onInstall={() => {}} skill lang="zh" />,
    );
    expect(zh).toContain("已在库");
    expect(zh).not.toContain("<button");
    const en = renderToString(
      <InstallButton status="installed" installing={false} canInstall onInstall={() => {}} skill lang="en" />,
    );
    expect(en).toContain("In library");
  });

  it("update_available → 更新库内版本 / Update in library", () => {
    const zh = renderToString(
      <InstallButton status="update_available" installing={false} canInstall onInstall={() => {}} skill lang="zh" />,
    );
    expect(zh).toContain("更新库内版本");
    const en = renderToString(
      <InstallButton status="update_available" installing={false} canInstall onInstall={() => {}} skill lang="en" />,
    );
    expect(en).toContain("Update in library");
  });

  it("a type=skill card renders the library CTA (no install-to-project button)", () => {
    const html = renderToString(
      <PluginCard
        plugin={plugin({ type: "skill", installed_status: "not_installed" })}
        installing={false}
        canInstall
        onOpen={() => {}}
        onInstall={() => {}}
      />,
    );
    expect(html).toContain("安装到库");
  });

  it("an agent card keeps the project install CTA unchanged", () => {
    const html = renderToString(
      <PluginCard
        plugin={plugin({ type: "agent", installed_status: "not_installed" })}
        installing={false}
        canInstall
        onOpen={() => {}}
        onInstall={() => {}}
      />,
    );
    expect(html).toContain("安装");
    expect(html).not.toContain("安装到库");
  });
});
