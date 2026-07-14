// v0.8.24 Track A — 设置 view (set-nav five sub-pages) + the tenant ACL gate
// (红线 §1.6-3: fail-closed via useMe — a tenant NEVER sees 运维总览/IM).

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { readFileSync } from "node:fs";

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

import SettingsView, {
  AccountPanel,
  GeneralPanel,
  OpsPanel,
  maskToken,
  resolveSettingsTab,
  visibleSettingsItems,
} from "./SettingsView";

describe("visibleSettingsItems (fail-closed ACL)", () => {
  it("tenant sees ONLY 插件市场 / 通用 / 账号", () => {
    expect(visibleSettingsItems(false)).toEqual(["market", "general", "account"]);
  });

  it("admin sees the five tabs, with Status + Hosts merged into ops", () => {
    expect(visibleSettingsItems(true)).toEqual([
      "ops",
      "market",
      "im",
      "general",
      "account",
    ]);
  });
});

describe("resolveSettingsTab", () => {
  it("honors a visible routed tab", () => {
    expect(resolveSettingsTab("market", false)).toBe("market");
    expect(resolveSettingsTab("im", true)).toBe("im");
  });

  it("denies an admin-only tab to a tenant (falls back to market)", () => {
    expect(resolveSettingsTab("ops", false)).toBe("market");
    expect(resolveSettingsTab("hosts", false)).toBe("market");
    expect(resolveSettingsTab("im", false)).toBe("market");
    expect(resolveSettingsTab("status", false)).toBe("market");
  });

  it("maps both legacy tabs to ops for admins", () => {
    expect(resolveSettingsTab("hosts", true)).toBe("ops");
    expect(resolveSettingsTab("status", true)).toBe("ops");
  });

  it("defaults: admin → ops, tenant → market", () => {
    expect(resolveSettingsTab(undefined, true)).toBe("ops");
    expect(resolveSettingsTab(undefined, false)).toBe("market");
  });
});

describe("SettingsView SSR (identity unresolved = fail-closed tenant view)", () => {
  beforeEach(() => {
    // useMe never resolves under SSR → isAdmin stays false (fail-closed).
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("renders the set-nav with ONLY the tenant items before /me resolves", () => {
    const html = renderToString(
      <MemoryRouter>
        <SettingsView />
      </MemoryRouter>,
    );
    expect(html).toContain('data-testid="settings-view"');
    expect(html).toContain('data-testid="set-item-market"');
    expect(html).toContain('data-testid="set-item-general"');
    expect(html).toContain('data-testid="set-item-account"');
    // Admin-only panels must never flash to a tenant.
    expect(html).not.toContain('data-testid="set-item-ops"');
    expect(html).not.toContain('data-testid="set-item-im"');
  });

  it("defaults an unresolved identity to the 插件市场 panel", () => {
    const html = renderToString(
      <MemoryRouter>
        <SettingsView />
      </MemoryRouter>,
    );
    expect(html).toContain('data-testid="marketplace-view"');
  });
});

describe("OpsPanel (merged Status + Hosts)", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("renders both existing panels in one grid without changing their test ids", () => {
    const html = renderToString(<OpsPanel lang="zh" rail={[]} />);
    expect(html).toContain('data-testid="ops-view"');
    expect(html).toContain('class="ops-grid"');
    expect(html).toContain('data-testid="status-view"');
    expect(html).toContain('data-testid="hosts-view"');
  });

  it("uses two columns on wide screens and one column at the narrow breakpoint", () => {
    const css = readFileSync(new URL("../index.css", import.meta.url), "utf8");
    expect(css).toMatch(/\.ops-grid\s*\{[^}]*grid-template-columns:\s*repeat\(2,/s);
    expect(css).toMatch(
      /@media \(max-width:\s*1280px\)\s*\{\s*\.ops-grid\s*\{[^}]*grid-template-columns:\s*minmax\(0,\s*1fr\)/s,
    );
  });
});

describe("GeneralPanel (语言 + 主题 segs)", () => {
  it("renders both segs with the active choice highlighted", () => {
    const html = renderToString(
      <GeneralPanel lang="zh" theme="light" onLang={() => {}} onTheme={() => {}} />,
    );
    expect(html).toContain('data-testid="lang-seg"');
    expect(html).toContain('data-testid="theme-seg"');
    expect(html).toContain("中文");
    expect(html).toContain("English");
    expect(html).toContain("浅色");
    expect(html).toContain("深色");
    // light is active.
    const themeSeg = html.slice(html.indexOf('data-testid="theme-seg"'));
    expect(themeSeg.indexOf('class="active"')).toBeGreaterThan(-1);
  });
});

describe("AccountPanel (absorbs the old AvatarMenu)", () => {
  it("renders avatar swatches + name + masked token + logout", () => {
    const html = renderToString(
      <AccountPanel
        lang="zh"
        isAdmin
        handle="owner"
        displayName="rob"
        avatar="#f59e0b"
        onName={() => {}}
        onAvatar={() => {}}
      />,
    );
    expect(html).toContain('data-testid="settings-account"');
    expect(html).toContain('data-testid="account-name"');
    expect(html).toContain('data-testid="account-token"');
    expect(html).toContain('data-testid="account-logout"');
    expect(html.replace(/<!-- -->/g, "")).toContain("@owner");
    // The token input is a masked password field, never the raw secret.
    expect(html).toContain('type="password"');
  });

  it("admin sees the web-token 重置 button; tenant does not (own token = admin-managed)", () => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
    const admin = renderToString(
      <AccountPanel
        lang="zh"
        isAdmin
        handle="owner"
        displayName=""
        avatar="#f59e0b"
        onName={() => {}}
        onAvatar={() => {}}
      />,
    );
    expect(admin).toContain('data-testid="account-reset-token"');
    expect(admin).toContain("重置 web token");

    const tenant = renderToString(
      <AccountPanel
        lang="zh"
        isAdmin={false}
        handle="alice"
        displayName=""
        avatar="#3b82f6"
        onName={() => {}}
        onAvatar={() => {}}
      />,
    );
    expect(tenant).not.toContain('data-testid="account-reset-token"');
  });

  it("tenant account panel embeds the self-serve 我的 IM bot section", () => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
    const html = renderToString(
      <AccountPanel
        lang="zh"
        isAdmin={false}
        handle="alice"
        displayName=""
        avatar="#3b82f6"
        onName={() => {}}
        onAvatar={() => {}}
      />,
    );
    expect(html).toContain('data-testid="settings-my-im"');
  });
});

describe("maskToken", () => {
  it("masks to the last 4 chars and never echoes the secret", () => {
    expect(maskToken("ccteam:deadbeefcafe")).toBe("••••••••cafe");
    expect(maskToken(null)).toBe("—");
  });
});
