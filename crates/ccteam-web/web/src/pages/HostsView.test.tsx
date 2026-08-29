// v0.9.13 — HostsView is the hosts & harness MANAGEMENT panel; these tests
// pin that shape: one card per machine, the FULL vendor inventory per card
// (installed / version / ready badge / MCP registration state), CTAs only
// where the backend accepts the write, satellite projects with adopt state.
//
// No DOM env: `renderToString` proves structure (the header's Team-page Link
// needs a Router context → MemoryRouter), and click wiring is exercised by
// walking the hook-free `HostManageCard` element tree and invoking `onClick`
// directly. The container's own handlers are hook-bound and cannot run under
// SSR, so the wiring tests hand the card the very same API calls the
// container makes and assert the resulting HTTP shape against a mocked
// `fetch`.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { renderToString } from "react-dom/server";
import { MemoryRouter } from "react-router-dom";
import HostsView, {
  HostManageCard,
  OfflineHostCard,
  JoinCard,
  installCtaFor,
  pendingActionsFor,
  toolSurfaceNoticesFor,
} from "./HostsView";
import { installVendor, registerMcp } from "../lib/hostsApi";
import { importProject } from "../lib/dashboardApi";
import type { AgentHealth, HostDetail, InstallJob } from "../lib/hostsApi";

const realFetch = globalThis.fetch;

function agent(over: Partial<AgentHealth> & { vendor: string }): AgentHealth {
  return {
    harness_id: over.vendor,
    installed: true,
    version: null,
    bin: over.vendor,
    mcp_registered: false,
    tool_surface: "native_mcp_config",
    status: "ready",
    hint: null,
    ...over,
  };
}

/** Local box: claude is the only actionable vendor. */
const LOCAL: HostDetail = {
  host: "local",
  hostname: "devbox",
  is_local: true,
  os: "linux",
  arch: "x86_64",
  ccteam_version: "0.9.12",
  agents: [
    // installed + registrable + unregistered → the one CTA.
    agent({ vendor: "claude", version: "claude 1.2.3", status: "needs_config" }),
    // not on PATH → shown with its remediation hint, never a CTA.
    agent({
      vendor: "codex",
      installed: false,
      status: "not_installed",
      hint: "npm install -g @openai/codex",
    }),
    // Managed bridge vendor: a native-registration CTA would be a no-op.
    agent({
      vendor: "pi",
      tool_surface: "managed_session_bridge",
      tool_surface_note:
        "Managed Pi sessions get the ccteam bridge; a plain `pi` started in a shell does not.",
      version: "pi 0.83.0",
    }),
    // already registered → state shown, nothing to do.
    agent({ vendor: "kimi", mcp_registered: true, version: "kimi 0.26.0" }),
  ],
};

/** Satellite: one adopted project, one still uncataloged. */
const SAT: HostDetail = {
  host: "sat-1",
  hostname: "gpu-box",
  is_local: false,
  os: "linux",
  arch: "aarch64",
  ccteam_version: "0.9.12",
  // Deliberately register-shaped: a satellite must still never get the CTA.
  agents: [agent({ vendor: "claude", version: "claude 1.2.3", status: "needs_config" })],
  projects: [
    { slug: "already", path: "/srv/already", cataloged: true, catalog_slug: "already-local" },
    { slug: "fresh", path: "/srv/fresh", cataloged: false, catalog_slug: null },
  ],
};

type ClickHandler = (e?: unknown) => void;

/** Collect every `onClick` prop in a (hook-free) component's element tree,
 *  in render order — the node-env stand-in for a DOM click. Hook-free child
 *  function components (the per-vendor rows) are invoked and walked too. */
function collectOnClicks(el: unknown, out: ClickHandler[] = []): ClickHandler[] {
  if (el == null || typeof el !== "object") return out;
  if (Array.isArray(el)) {
    for (const child of el) collectOnClicks(child, out);
    return out;
  }
  const node = el as {
    type?: unknown;
    props?: { onClick?: unknown; children?: unknown };
  };
  if (typeof node.type === "function" && node.props) {
    // Render the hook-free child component and walk its output.
    collectOnClicks((node.type as (props: unknown) => unknown)(node.props), out);
    return out;
  }
  if (node.props) {
    if (typeof node.props.onClick === "function") out.push(node.props.onClick as ClickHandler);
    collectOnClicks(node.props.children, out);
  }
  return out;
}

describe("pendingActionsFor (CTA eligibility single home)", () => {
  it("offers register-mcp only for installed + registrable + unregistered local vendors", () => {
    expect(pendingActionsFor(LOCAL)).toEqual([{ kind: "register", vendor: "claude" }]);
  });

  it("never offers an import on the local host (its projects are the catalog)", () => {
    const withProjects: HostDetail = {
      ...LOCAL,
      projects: [{ slug: "solo", path: "/srv/solo", cataloged: false, catalog_slug: null }],
    };
    expect(pendingActionsFor(withProjects)).toEqual([{ kind: "register", vendor: "claude" }]);
  });

  it("turns a satellite's uncataloged projects into import actions", () => {
    expect(pendingActionsFor(SAT)).toEqual([
      { kind: "import", slug: "fresh", path: "/srv/fresh" },
    ]);
  });

  it("never offers register-mcp on a satellite (the backend 404s off-local)", () => {
    expect(pendingActionsFor(SAT).some((a) => a.kind === "register")).toBe(false);
  });

  it("returns nothing for a fully provisioned host", () => {
    const done: HostDetail = {
      ...LOCAL,
      agents: [agent({ vendor: "claude", mcp_registered: true })],
    };
    expect(pendingActionsFor(done)).toEqual([]);
    expect(pendingActionsFor({ ...SAT, projects: [] })).toEqual([]);
  });
});

describe("toolSurfaceNoticesFor", () => {
  it("renders the managed-vs-plain Pi distinction from the backend", () => {
    expect(toolSurfaceNoticesFor(LOCAL)).toEqual([
      "Managed Pi sessions get the ccteam bridge; a plain `pi` started in a shell does not.",
    ]);
  });
});

describe("installCtaFor (VENDOR-INSTALL-1 button eligibility)", () => {
  const ADMIN_LOCAL = { isAdmin: true, isLocal: true, latest: null };

  it("offers Install for a missing npm-packaged vendor", () => {
    const codex = agent({ vendor: "codex", installed: false, status: "not_installed" });
    expect(installCtaFor(codex, ADMIN_LOCAL)).toEqual({ kind: "install" });
  });

  it("offers Update with the npm latest when the probe version is older", () => {
    const claude = agent({ vendor: "claude", version: "claude 2.1.200" });
    expect(installCtaFor(claude, { ...ADMIN_LOCAL, latest: "2.1.220" })).toEqual({
      kind: "update",
      latest: "2.1.220",
    });
  });

  it("offers nothing when up-to-date, non-admin, off-local, or recipe-less", () => {
    const claude = agent({ vendor: "claude", version: "claude 2.1.220" });
    // Up-to-date (equal or newer probe version, or no latest known).
    expect(installCtaFor(claude, { ...ADMIN_LOCAL, latest: "2.1.220" })).toEqual({ kind: "none" });
    expect(installCtaFor(claude, ADMIN_LOCAL)).toEqual({ kind: "none" });
    // The backend 403 is the real gate; the button simply never renders.
    const missing = agent({ vendor: "codex", installed: false, status: "not_installed" });
    expect(installCtaFor(missing, { ...ADMIN_LOCAL, isAdmin: false })).toEqual({ kind: "none" });
    expect(installCtaFor(missing, { ...ADMIN_LOCAL, isLocal: false })).toEqual({ kind: "none" });
    // kimi/pi have no recipe — manual install guidance only, never a button.
    for (const vendor of ["kimi", "pi"]) {
      const row = agent({ vendor, installed: false, status: "not_installed" });
      expect(installCtaFor(row, ADMIN_LOCAL)).toEqual({ kind: "none" });
    }
  });
});

describe("VendorManageRow install CTA (VENDOR-INSTALL-1)", () => {
  /** Local detail whose ONLY row button is the install CTA under test. */
  function installDetail(agents: AgentHealth[], npmAvailable = true): HostDetail {
    return {
      host: "local",
      hostname: "devbox",
      is_local: true,
      os: "linux",
      arch: "x86_64",
      ccteam_version: "0.10.0",
      npm_available: npmAvailable,
      agents,
    };
  }

  it("not-installed npm vendor → Install button; installed+outdated → Update; up-to-date → none", () => {
    const detail = installDetail([
      agent({ vendor: "codex", installed: false, status: "not_installed", hint: "…" }),
      agent({ vendor: "claude", version: "claude 2.1.200", mcp_registered: true }),
      agent({ vendor: "grok", version: "grok 1.0.0", mcp_registered: true }),
    ]);
    const html = renderToString(
      <HostManageCard
        detail={detail}
        busy={null}
        isAdmin
        latests={{ claude: "2.1.220", grok: "1.0.0" }}
        onRegister={() => {}}
        onImport={() => {}}
        onInstall={() => {}}
      />,
    );
    expect(html).toContain('data-testid="install-vendor-codex"');
    expect(html).toContain('data-testid="install-vendor-claude"');
    expect(html).toContain("更新 → 2.1.220");
    // Up-to-date (grok 1.0.0 vs latest 1.0.0) renders no CTA.
    expect(html).not.toContain('data-testid="install-vendor-grok"');
  });

  it("renders no install button for kimi/pi, tenants, or satellites", () => {
    const withManual = installDetail([
      agent({ vendor: "kimi", installed: false, status: "not_installed", hint: "manual" }),
      agent({ vendor: "pi", installed: false, status: "not_installed", hint: "manual" }),
    ]);
    const adminHtml = renderToString(
      <HostManageCard detail={withManual} busy={null} isAdmin onRegister={() => {}} onImport={() => {}} />,
    );
    expect(adminHtml).not.toContain("install-vendor-");
    // Same rows, tenant view: the CTA never renders (the backend would 403).
    const missing = installDetail([
      agent({ vendor: "codex", installed: false, status: "not_installed" }),
    ]);
    const tenantHtml = renderToString(
      <HostManageCard detail={missing} busy={null} onRegister={() => {}} onImport={() => {}} />,
    );
    expect(tenantHtml).not.toContain("install-vendor-");
    // A satellite never gets the CTA even for the admin.
    const sat: HostDetail = { ...SAT, agents: missing.agents };
    const satHtml = renderToString(
      <HostManageCard detail={sat} busy={null} isAdmin onRegister={() => {}} onImport={() => {}} />,
    );
    expect(satHtml).not.toContain("install-vendor-");
  });

  it("disables the button with the npm-missing hint when npm is not on PATH", () => {
    const detail = installDetail(
      [agent({ vendor: "codex", installed: false, status: "not_installed" })],
      false,
    );
    const html = renderToString(
      <HostManageCard detail={detail} busy={null} isAdmin onRegister={() => {}} onImport={() => {}} />,
    );
    expect(html).toContain('data-testid="install-vendor-codex"');
    expect(html).toContain("disabled");
    expect(html).toContain("npm 不在 PATH 上");
  });

  it("shows inline progress while running and the output tail on failure", () => {
    const detail = installDetail([
      agent({ vendor: "codex", installed: false, status: "not_installed" }),
      agent({ vendor: "grok", installed: false, status: "not_installed" }),
    ]);
    const jobs: Record<string, InstallJob> = {
      codex: {
        job_id: "j1",
        vendor: "codex",
        state: "running",
        exit_code: null,
        output_tail: "$ npm install -g @openai/codex@latest\nfetching…",
      },
      grok: {
        job_id: "j2",
        vendor: "grok",
        state: "failed",
        exit_code: 1,
        output_tail: "npm ERR! EACCES: permission denied",
      },
    };
    const html = renderToString(
      <HostManageCard
        detail={detail}
        busy={null}
        isAdmin
        installJobs={jobs}
        onRegister={() => {}}
        onImport={() => {}}
      />,
    );
    expect(html).toContain('data-testid="install-progress-codex"');
    expect(html).toContain("安装中…");
    expect(html).toContain("fetching…");
    expect(html).toContain('data-testid="install-failed-grok"');
    expect(html).toContain("安装失败");
    expect(html).toContain("exit 1");
    expect(html).toContain("EACCES");
  });

  it("install click reaches POST /hosts/local/vendors/{vendor}/install", () => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
    try {
      const detail = installDetail([
        agent({ vendor: "codex", installed: false, status: "not_installed" }),
      ]);
      const clicks = collectOnClicks(
        HostManageCard({
          detail,
          busy: null,
          isAdmin: true,
          onRegister: () => {},
          onImport: () => {},
          // Exactly what the container's onInstall does with the vendor.
          onInstall: (vendor) => void installVendor("local", vendor),
        }),
      );
      expect(clicks).toHaveLength(1);
      clicks[0]();
      expect(globalThis.fetch).toHaveBeenCalledTimes(1);
      const [url, init] = (globalThis.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
      expect(url).toBe("/api/v1/hosts/local/vendors/codex/install");
      expect((init as RequestInit).method).toBe("POST");
    } finally {
      globalThis.fetch = realFetch;
      vi.restoreAllMocks();
    }
  });
});

describe("VendorManageRow DSH plugin registration (v0.10.3 gate ①)", () => {
  const dshDetail = (over: Partial<HostDetail> = {}): HostDetail => ({
    host: "local",
    hostname: "devbox",
    is_local: true,
    os: "linux",
    arch: "x86_64",
    ccteam_version: "0.10.3",
    agents: [
      agent({
        vendor: "dsh",
        tool_surface: "managed_session_bridge",
        version: "0.1.0-rc.6",
      }),
    ],
    ...over,
  });

  it("admin on the local host sees the register CTA on the dsh row", () => {
    const html = renderToString(
      <HostManageCard
        detail={dshDetail()}
        busy={null}
        isAdmin
        onRegister={() => {}}
        onImport={() => {}}
      />,
    );
    expect(html).toContain('data-testid="register-dsh-plugin"');
    expect(html).toContain("注册 DSH 插件");
  });

  it("an already-registered dsh row shows state, not the CTA", () => {
    // The plugin is either in the operator's ~/.dsh web profile or it is not;
    // an idempotent CTA that never goes away reads as "not done yet".
    const html = renderToString(
      <HostManageCard
        detail={dshDetail({
          agents: [
            agent({
              vendor: "dsh",
              tool_surface: "managed_session_bridge",
              version: "0.1.0-rc.6",
              mcp_registered: true,
            }),
          ],
        })}
        busy={null}
        isAdmin
        onRegister={() => {}}
        onImport={() => {}}
      />,
    );
    expect(html).not.toContain("register-dsh-plugin");
    expect(html).toContain('data-testid="host-vendor-dsh-plugin-ok"');
  });

  it("tenants, satellites, and a not-installed dsh get no CTA", () => {
    const tenant = renderToString(
      <HostManageCard
        detail={dshDetail()}
        busy={null}
        isAdmin={false}
        onRegister={() => {}}
        onImport={() => {}}
      />,
    );
    expect(tenant).not.toContain("register-dsh-plugin");
    const satellite = renderToString(
      <HostManageCard
        detail={dshDetail({ host: "sat-1", is_local: false })}
        busy={null}
        isAdmin
        onRegister={() => {}}
        onImport={() => {}}
      />,
    );
    expect(satellite).not.toContain("register-dsh-plugin");
    const missing = renderToString(
      <HostManageCard
        detail={dshDetail({
          agents: [
            agent({
              vendor: "dsh",
              tool_surface: "managed_session_bridge",
              installed: false,
              status: "not_installed",
            }),
          ],
        })}
        busy={null}
        isAdmin
        onRegister={() => {}}
        onImport={() => {}}
      />,
    );
    expect(missing).not.toContain("register-dsh-plugin");
  });
});

describe("VendorManageRow quota bars (VENDOR-QUOTA-1)", () => {
  function quotaDetail(agents: AgentHealth[]): HostDetail {
    return {
      host: "local",
      hostname: "devbox",
      is_local: true,
      os: "linux",
      arch: "x86_64",
      ccteam_version: "0.10.0",
      npm_available: true,
      agents,
    };
  }

  it("available: up to two mini bars + plan badge on the vendor row", () => {
    const detail = quotaDetail([
      agent({ vendor: "claude", mcp_registered: true }),
      agent({ vendor: "kimi", mcp_registered: true }),
    ]);
    const quotas = {
      claude: {
        vendor: "claude",
        state: "available" as const,
        plan: "max",
        windows: [
          { kind: "five_hour" as const, used_percent: 42, resets_at: null },
          { kind: "weekly" as const, used_percent: 15, resets_at: null },
        ],
      },
      kimi: {
        vendor: "kimi",
        state: "available" as const,
        windows: [{ kind: "weekly" as const, used_percent: 4, resets_at: null }],
      },
    };
    const html = renderToString(
      <HostManageCard
        detail={detail}
        busy={null}
        isAdmin
        quotas={quotas}
        onRegister={() => {}}
        onImport={() => {}}
      />,
    );
    expect(html).toContain('data-testid="quota-bars-claude"');
    expect(html).toContain('data-testid="quota-plan-claude"');
    expect(html).toContain("max");
    expect(html).toContain("5h ▓▓░░░ 42%");
    expect(html).toContain("周 ▓░░░░ 15%");
    // Single-window vendor renders exactly one bar and no badge.
    expect(html).toContain('data-testid="quota-bars-kimi"');
    expect(html).toContain("周 ░░░░░ 4%");
    expect(html).not.toContain('data-testid="quota-plan-kimi"');
  });

  it("not_subscription / unavailable / absent vendors render no quota zone", () => {
    const detail = quotaDetail([
      agent({ vendor: "codex", mcp_registered: true }),
      agent({ vendor: "grok", mcp_registered: true }),
      agent({ vendor: "opencode", mcp_registered: true }),
    ]);
    const quotas = {
      codex: { vendor: "codex", state: "not_subscription" as const },
      grok: { vendor: "grok", state: "unavailable" as const },
      // opencode: absent from the map entirely (no probe surface).
    };
    const html = renderToString(
      <HostManageCard
        detail={detail}
        busy={null}
        isAdmin
        quotas={quotas}
        onRegister={() => {}}
        onImport={() => {}}
      />,
    );
    expect(html).not.toContain("quota-bars-");
    expect(html).not.toContain("quota-plan-");
  });

  it("no quota prop at all (tenant view) renders nothing and does not crash", () => {
    const detail = quotaDetail([agent({ vendor: "claude", mcp_registered: true })]);
    const html = renderToString(
      <HostManageCard detail={detail} busy={null} onRegister={() => {}} onImport={() => {}} />,
    );
    expect(html).not.toContain("quota-bars-");
  });
});

describe("HostManageCard (full vendor inventory)", () => {
  it("renders identity + os/arch/build + EVERY vendor row, not just pending ones", () => {
    const html = renderToString(
      <HostManageCard detail={LOCAL} busy={null} onRegister={() => {}} onImport={() => {}} />,
    );
    expect(html).toContain('data-testid="host-manage-local"');
    expect(html).toContain('class="dot on"');
    expect(html).toContain("devbox");
    expect(html.replace(/<!-- -->/g, "")).toContain("linux/x86_64 · ccteam 0.9.12");
    // The whole inventory shows — including the vendors with nothing to do.
    for (const vendor of ["claude", "codex", "pi", "kimi"]) {
      expect(html).toContain(`data-testid="host-vendor-local-${vendor}"`);
    }
  });

  it("per-vendor row: version / not-installed label / ready badge / hint verbatim", () => {
    const html = renderToString(
      <HostManageCard detail={LOCAL} busy={null} onRegister={() => {}} onImport={() => {}} />,
    );
    expect(html).toContain("claude 1.2.3");
    expect(html).toContain("kimi 0.26.0");
    // Not installed: label + copy-paste remediation hint, never a CTA.
    expect(html).toContain("未安装");
    expect(html).toContain("npm install -g @openai/codex");
    // Ready-state badges verbatim off the API.
    expect(html).toContain("需配置");
    expect(html).toContain("就绪");
  });

  it("MCP column: register CTA only where eligible, ✓ where registered", () => {
    const html = renderToString(
      <HostManageCard detail={LOCAL} busy={null} onRegister={() => {}} onImport={() => {}} />,
    );
    expect(html).toContain('data-testid="register-mcp-claude"');
    // Registered vendor shows the state, not a button.
    expect(html).toContain('data-testid="host-vendor-mcp-ok-kimi"');
    expect(html).toContain("MCP 已注册");
    // Not-installed / managed-bridge vendors get neither CTA nor state.
    expect(html).not.toContain('data-testid="register-mcp-codex"');
    expect(html).not.toContain('data-testid="register-mcp-pi"');
    expect(html).toContain(
      "Managed Pi sessions get the ccteam bridge; a plain `pi` started in a shell does not.",
    );
  });

  it("satellite: installed-but-unregistered shows state WITHOUT a dead-end CTA", () => {
    const html = renderToString(
      <HostManageCard detail={SAT} busy={null} onRegister={() => {}} onImport={() => {}} />,
    );
    expect(html).not.toContain('data-testid="register-mcp-claude"');
    expect(html).toContain("MCP 未注册");
  });

  it("satellite projects: adopted badge (with catalog slug) vs import CTA", () => {
    const html = renderToString(
      <HostManageCard detail={SAT} busy={null} onRegister={() => {}} onImport={() => {}} />,
    );
    expect(html).toContain('data-testid="host-projects-sat-1"');
    expect(html).toContain('data-testid="host-project-adopted-already"');
    expect(html.replace(/<!-- -->/g, "")).toContain("已接入 → already-local");
    expect(html).toContain('data-testid="import-project-fresh"');
  });

  it("local host renders no projects section (its projects ARE the catalog)", () => {
    const withProjects: HostDetail = {
      ...LOCAL,
      projects: [{ slug: "solo", path: "/srv/solo", cataloged: false, catalog_slug: null }],
    };
    const html = renderToString(
      <HostManageCard detail={withProjects} busy={null} onRegister={() => {}} onImport={() => {}} />,
    );
    expect(html).not.toContain('data-testid="host-projects-local"');
    expect(html).not.toContain('data-testid="import-project-solo"');
  });

  it("swaps to the busy label only for the exact host:vendor being registered", () => {
    const busyHere = renderToString(
      <HostManageCard detail={LOCAL} busy="local:claude" onRegister={() => {}} onImport={() => {}} />,
    );
    expect(busyHere).toContain("注册中…");
    expect(busyHere).toContain("disabled");
    // Same vendor on a different machine must not steal the spinner.
    const busyElsewhere = renderToString(
      <HostManageCard detail={LOCAL} busy="sat-1:claude" onRegister={() => {}} onImport={() => {}} />,
    );
    expect(busyElsewhere).not.toContain("注册中…");
    expect(busyElsewhere).toContain("注册 MCP");
  });

  it("renders English labels when lang='en'", () => {
    const html = renderToString(
      <HostManageCard detail={LOCAL} busy={null} lang="en" onRegister={() => {}} onImport={() => {}} />,
    );
    expect(html).toContain("Register MCP");
    expect(html).toContain("MCP registered");
    expect(html).toContain("not installed");
  });
});

describe("OfflineHostCard", () => {
  it("says an offline host cannot be probed instead of claiming it is clean", () => {
    const html = renderToString(<OfflineHostCard hostId="sat-1" hostname="gpu-box" />);
    expect(html).toContain('class="host-manage offline"');
    expect(html).toContain('class="dot off"');
    expect(html).toContain("无法探测");
  });
});

describe("HostManageCard click wiring", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("register click reaches POST /hosts/{host}/register-mcp for that vendor", () => {
    const clicks = collectOnClicks(
      HostManageCard({
        detail: LOCAL,
        busy: null,
        // Exactly what the container's onRegister does with the vendor.
        onRegister: (vendor) => void registerMcp("local", vendor),
        onImport: () => {},
      }),
    );
    expect(clicks).toHaveLength(1);
    clicks[0]();
    expect(globalThis.fetch).toHaveBeenCalledTimes(1);
    const [url, init] = (globalThis.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(url).toBe("/api/v1/hosts/local/register-mcp?vendor=claude");
    expect((init as RequestInit).method).toBe("POST");
  });

  it("import click reaches POST /projects/import with the satellite's remote slug", () => {
    const clicks = collectOnClicks(
      HostManageCard({
        detail: SAT,
        busy: null,
        onRegister: () => {},
        // Exactly what the container's onImport does with the slug.
        onImport: (remoteSlug) => void importProject("sat-1", remoteSlug),
      }),
    );
    expect(clicks).toHaveLength(1);
    clicks[0]();
    expect(globalThis.fetch).toHaveBeenCalledTimes(1);
    const [url, init] = (globalThis.fetch as ReturnType<typeof vi.fn>).mock.calls[0];
    expect(url).toBe("/api/v1/projects/import");
    expect(JSON.parse((init as RequestInit).body as string)).toEqual({
      host: "sat-1",
      remote_slug: "fresh",
    });
  });
});

describe("HostsView (management panel shell)", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("renders the panel + loading placeholder before the host probe resolves", () => {
    const html = renderToString(
      <MemoryRouter>
        <HostsView />
      </MemoryRouter>,
    );
    expect(html).toContain('data-testid="hosts-view"');
    expect(html).toContain('data-testid="hosts-loading"');
    expect(html).toContain('data-testid="hosts-refresh"');
    expect(html).toContain("主机");
  });

  it("header points at the Team page for the fleet observation surface", () => {
    const html = renderToString(
      <MemoryRouter>
        <HostsView embedded />
      </MemoryRouter>,
    );
    expect(html).toContain('data-testid="hosts-team-link"');
    expect(html).toContain('href="/agents"');
    expect(html).toContain("团队页");
    // The pointer is load-bearing, so the copy shows in embedded mode too.
    expect(html).toContain('class="hosts-head-desc"');
  });

  it("renders the English header + Team-page link", () => {
    const html = renderToString(
      <MemoryRouter>
        <HostsView lang="en" />
      </MemoryRouter>,
    );
    expect(html).toContain("Team page");
    expect(html).toContain('href="/agents"');
  });
});

describe("JoinCard", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("renders the join command with a placeholder token + mint CTA before the token loads", () => {
    const html = renderToString(<JoinCard />);
    expect(html).toContain('data-testid="join-card"');
    expect(html).toContain("ccteam host join --daemon");
    expect(html).toContain("&lt;join-token&gt;");
    expect(html).toContain('data-testid="join-mint"');
    expect(html).not.toContain('data-testid="join-copy"');
  });

  it("HostsView points to Settings · Access via a ROUTER link (SPA basename)", () => {
    // The SPA mounts under `/app` (BrowserRouter basename) — a raw anchor to
    // `/settings/access` 404s outside the app. Rendering under a basename
    // proves the pointer is a router Link that picks the prefix up.
    const html = renderToString(
      <MemoryRouter basename="/app" initialEntries={["/app/settings/ops"]}>
        <HostsView />
      </MemoryRouter>,
    );
    expect(html).not.toContain('data-testid="join-card"');
    expect(html).toContain('href="/app/settings/access"');
    expect(html).toContain("连接新主机 → 设置·接入");
  });
});
