// v0.8.24 Track A — 设置 view (set-nav sub-pages) + the tenant ACL gate
// (fail-closed via useMe — a tenant never sees 用户管理 / Admin).

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
  AdminPanel,
  GeneralPanel,
  OpsPanel,
  maskToken,
  resolveSettingsTab,
  settingsDetailWidthClass,
  visibleSettingsItems,
} from "./SettingsView";

describe("visibleSettingsItems (fail-closed ACL)", () => {
  it("tenant sees every settings surface except 用户管理", () => {
    expect(visibleSettingsItems(false)).toEqual(["ops", "access", "general", "account"]);
  });

  it("admin sees ops + the tenant tabs + 管理员 (no standalone IM tab)", () => {
    expect(visibleSettingsItems(true)).toEqual([
      "ops",
      "access",
      "general",
      "account",
      "admin",
    ]);
  });

  // The whole of the owner ruling in one assertion: 管理员 (user management) is
  // the ONLY surface a normal user does not get. Everything else is open, and
  // what a user may actually reach inside it is decided by identity × project
  // ownership on the backend, not by hiding UI.
  it("the admin tab is the ONLY difference between the two menus", () => {
    const tenant = visibleSettingsItems(false);
    const admin = visibleSettingsItems(true);
    expect(admin.filter((id) => !tenant.includes(id))).toEqual(["admin"]);
    expect(tenant.filter((id) => !admin.includes(id))).toEqual([]);
  });
});

describe("resolveSettingsTab", () => {
  it("honors a visible routed tab", () => {
    expect(resolveSettingsTab("general", false)).toBe("general");
    expect(resolveSettingsTab("access", true)).toBe("access");
    expect(resolveSettingsTab("admin", true)).toBe("admin");
    expect(resolveSettingsTab("ops", true)).toBe("ops");
  });

  it("keeps only the admin tab fail-closed and falls back to ops", () => {
    expect(resolveSettingsTab("ops", false)).toBe("ops");
    expect(resolveSettingsTab("access", false)).toBe("access");
    expect(resolveSettingsTab("hosts", false)).toBe("ops");
    expect(resolveSettingsTab("admin", false)).toBe("ops");
    expect(resolveSettingsTab("status", false)).toBe("ops");
  });

  it("maps both legacy tabs to ops for admins", () => {
    expect(resolveSettingsTab("hosts", true)).toBe("ops");
    expect(resolveSettingsTab("status", true)).toBe("ops");
  });

  it("defaults every identity to ops", () => {
    expect(resolveSettingsTab(undefined, true)).toBe("ops");
    expect(resolveSettingsTab(undefined, false)).toBe("ops");
  });
});

describe("settings detail width", () => {
  it("widens Access independently from the wider Ops surface", () => {
    expect(settingsDetailWidthClass("access")).toBe("access-wide");
    expect(settingsDetailWidthClass("ops")).toBe("ops-wide");
    expect(settingsDetailWidthClass("account")).toBe("");

    const css = readFileSync(new URL("../index.css", import.meta.url), "utf8");
    expect(css).toMatch(
      /\.set-detail-inner\.access-wide\s*\{[^}]*width:\s*100%;[^}]*max-width:\s*1200px/s,
    );
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

  it("renders shared items but not Admin before /me resolves", () => {
    const html = renderToString(
      <MemoryRouter>
        <SettingsView />
      </MemoryRouter>,
    );
    expect(html).toContain('data-testid="settings-view"');
    expect(html).toContain('data-testid="set-item-ops"');
    expect(html).toContain('data-testid="set-item-access"');
    expect(html).toContain('data-testid="set-item-general"');
    expect(html).toContain('data-testid="set-item-account"');
    // User management must never flash to a tenant.
    expect(html).not.toContain('data-testid="set-item-admin"');
  });

  it("defaults an unresolved identity to the shared Ops panel", () => {
    const html = renderToString(
      <MemoryRouter>
        <SettingsView />
      </MemoryRouter>,
    );
    expect(html).toContain('data-testid="ops-view"');
  });

  it("renders both shared ops and tenant-shaped access routes", () => {
    const ops = renderToString(
      <MemoryRouter>
        <SettingsView tab="ops" />
      </MemoryRouter>,
    );
    const access = renderToString(
      <MemoryRouter>
        <SettingsView tab="access" />
      </MemoryRouter>,
    );
    expect(ops).toContain('data-testid="ops-view"');
    expect(access).toContain('data-testid="settings-access"');
    expect(access).toContain('data-testid="access-my-im"');
  });
});

describe("OpsPanel (merged Status + Hosts)", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
  });
  afterEach(() => {
    vi.restoreAllMocks();
  });

  it("stacks daemon status above hosts (single column) without changing test ids", () => {
    // Router context: the hosts panel header links to the Team page (TEAM-9).
    const html = renderToString(
      <MemoryRouter>
        <OpsPanel lang="zh" />
      </MemoryRouter>,
    );
    expect(html).toContain('data-testid="ops-view"');
    expect(html).toContain('class="ops-stack"');
    expect(html).toContain('data-testid="status-view"');
    expect(html).toContain('data-testid="hosts-view"');
    // Daemon strip is the first status surface; hosts follow below. Project
    // catalog management lives on the SIDEBAR workspace menus, not here.
    expect(html.indexOf('data-testid="status-view"')).toBeLessThan(
      html.indexOf('data-testid="hosts-view"'),
    );
    expect(html).not.toContain('data-testid="projects-panel"');
  });

  it("uses a vertical ops stack (no side-by-side status/hosts columns)", () => {
    const css = readFileSync(new URL("../index.css", import.meta.url), "utf8");
    expect(css).toMatch(/\.ops-stack\s*\{[^}]*flex-direction:\s*column/s);
    expect(css).toMatch(/\.daemon-strip\s*\{/);
    // Retired two-column layout must not sneak back.
    expect(css).not.toMatch(/\.ops-grid\s*\{[^}]*grid-template-columns:\s*repeat\(2,/s);
  });
});

describe("AdminPanel (管理员 · Admin — user management only)", () => {
  it("renders the UserManagementSection as its only content", () => {
    const html = renderToString(<AdminPanel lang="zh" />);
    expect(html).toContain('data-testid="settings-admin"');
    expect(html).toContain("管理员");
    // The panel carries the 用户管理 · Users table (loading until the fetch
    // resolves — effects don't run under renderToString).
    expect(html).toContain('data-testid="settings-users"');
    expect(html).toContain("用户管理 · Users");
    expect(html).not.toContain('data-testid="settings-page"');
    expect(html).not.toContain('data-testid="settings-my-im"');
  });

  it("renders the English heading in en", () => {
    const html = renderToString(<AdminPanel lang="en" />);
    expect(html).toContain("Admin");
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

  it("every caller sees the two-step web-token reset button", () => {
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
    expect(tenant).toContain('data-testid="account-reset-token"');
  });

  it("tenant account no longer embeds 我的 IM bot (it moved to Access)", () => {
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
    expect(html).not.toContain('data-testid="settings-my-im"');
    // The admin-only global credentials panel is NOT rendered for a tenant.
    expect(html).not.toContain('data-testid="settings-loading"');
    expect(html).not.toContain('data-testid="settings-page"');
  });

  it("admin account panel no longer embeds global Telegram/Lark credentials", () => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
    const html = renderToString(
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
    expect(html).not.toContain('data-testid="settings-loading"');
    // …and NOT the tenant self-serve bot, nor user management (管理员 tab).
    expect(html).not.toContain('data-testid="settings-my-im"');
    expect(html).not.toContain('data-testid="settings-users"');
  });
});

describe("maskToken", () => {
  it("masks to the last 4 chars and never echoes the secret", () => {
    expect(maskToken("ccteam:deadbeefcafe")).toBe("••••••••cafe");
    expect(maskToken(null)).toBe("—");
  });
});
