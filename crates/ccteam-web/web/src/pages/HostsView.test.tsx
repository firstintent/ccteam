// v0.9.11 TEAM-9 — HostsView is an ACTION panel; these tests pin that shape.
//
// No DOM env: `renderToString` proves structure (the header's Team-page Link
// needs a Router context → MemoryRouter), and click wiring is exercised by
// walking the hook-free `HostActionRow` element tree and invoking `onClick`
// directly. The container's own handlers are hook-bound and cannot run under
// SSR, so the wiring tests hand the row the very same API calls the container
// makes and assert the resulting HTTP shape against a mocked `fetch`.
//
// What this file no longer covers, by design: the per-host × per-vendor
// health grid (versions / installed / MCP badges / project catalog listing)
// — that observation surface moved to the Team page's charter roster and is
// covered by CharterPanel.test.tsx.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { renderToString } from "react-dom/server";
import { MemoryRouter } from "react-router-dom";
import HostsView, {
  HostActionRow,
  JoinCard,
  pendingActionsFor,
  toolSurfaceNoticesFor,
} from "./HostsView";
import { registerMcp } from "../lib/hostsApi";
import { importProject } from "../lib/dashboardApi";
import type { AgentHealth, HostDetail } from "../lib/hostsApi";

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
  ccteam_version: "0.9.11",
  agents: [
    // installed + registrable + unregistered → the one CTA.
    agent({ vendor: "claude", version: "claude 1.2.3", status: "needs_config" }),
    // not on PATH → never a CTA (ccteam does not install CLIs).
    agent({ vendor: "codex", installed: false, status: "not_installed" }),
    // Managed bridge vendor: a native-registration CTA would be a no-op.
    agent({
      vendor: "pi",
      tool_surface: "managed_session_bridge",
      tool_surface_note:
        "Managed Pi sessions get the ccteam bridge; a plain `pi` started in a shell does not.",
      version: "pi 0.83.0",
    }),
    // already registered → nothing to do.
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
  ccteam_version: "0.9.11",
  // Deliberately register-shaped: a satellite must still never get the CTA.
  agents: [agent({ vendor: "claude", version: "claude 1.2.3", status: "needs_config" })],
  projects: [
    { slug: "already", path: "/srv/already", cataloged: true, catalog_slug: "already-local" },
    { slug: "fresh", path: "/srv/fresh", cataloged: false, catalog_slug: null },
  ],
};

type ClickHandler = (e?: unknown) => void;

/** Collect every `onClick` prop in a (hook-free) component's element tree,
 *  in render order — the node-env stand-in for a DOM click. */
function collectOnClicks(el: unknown, out: ClickHandler[] = []): ClickHandler[] {
  if (el == null || typeof el !== "object") return out;
  if (Array.isArray(el)) {
    for (const child of el) collectOnClicks(child, out);
    return out;
  }
  const props = (el as { props?: { onClick?: unknown; children?: unknown } }).props;
  if (props) {
    if (typeof props.onClick === "function") out.push(props.onClick as ClickHandler);
    collectOnClicks(props.children, out);
  }
  return out;
}

describe("pendingActionsFor", () => {
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

describe("HostActionRow", () => {
  it("renders identity (online dot · hostname · mono host id) + one register CTA", () => {
    const html = renderToString(
      <HostActionRow
        hostId="local"
        hostname="devbox"
        online
        actions={pendingActionsFor(LOCAL)}
        notices={toolSurfaceNoticesFor(LOCAL)}
        busy={null}
        onRegister={() => {}}
        onImport={() => {}}
      />,
    );
    expect(html).toContain('data-testid="host-actions-local"');
    expect(html).toContain('class="dot on"');
    expect(html).toContain("devbox");
    expect(html).toContain('class="host-actions-id mono"');
    expect(html).toContain("注册 MCP");
    expect(html).toContain('data-testid="register-mcp-claude"');
    // The non-actionable vendors never reach the panel at all.
    expect(html).not.toContain('data-testid="register-mcp-codex"');
    expect(html).not.toContain('data-testid="register-mcp-grok"');
    expect(html).not.toContain('data-testid="register-mcp-kimi"');
    expect(html).toContain(
      "Managed Pi sessions get the ccteam bridge; a plain `pi` started in a shell does not.",
    );
  });

  it("renders an import CTA per uncataloged satellite project, cataloged ones omitted", () => {
    const html = renderToString(
      <HostActionRow
        hostId="sat-1"
        hostname="gpu-box"
        online
        actions={pendingActionsFor(SAT)}
        busy={null}
        onRegister={() => {}}
        onImport={() => {}}
      />,
    );
    expect(html).toContain('data-testid="import-project-fresh"');
    expect(html).toContain("fresh");
    expect(html).not.toContain('data-testid="import-project-already"');
    expect(html).not.toContain("already");
  });

  it("says 无待办 when a reachable host has nothing pending", () => {
    const html = renderToString(
      <HostActionRow
        hostId="local"
        hostname="devbox"
        online
        actions={[]}
        busy={null}
        onRegister={() => {}}
        onImport={() => {}}
      />,
    );
    expect(html).toContain('data-testid="host-idle-local"');
    expect(html).toContain("无待办");
    expect(html).not.toContain("<button");
  });

  it("says an offline host cannot be probed instead of claiming it is clean", () => {
    const html = renderToString(
      <HostActionRow
        hostId="sat-1"
        hostname="gpu-box"
        online={false}
        actions={[]}
        busy={null}
        onRegister={() => {}}
        onImport={() => {}}
      />,
    );
    expect(html).toContain('class="host-actions offline"');
    expect(html).toContain('class="dot off"');
    expect(html).toContain("无法探测");
    expect(html).not.toContain("无待办");
  });

  it("swaps to the busy label only for the exact host:vendor being registered", () => {
    const busyHere = renderToString(
      <HostActionRow
        hostId="local"
        hostname="devbox"
        online
        actions={pendingActionsFor(LOCAL)}
        busy="local:claude"
        onRegister={() => {}}
        onImport={() => {}}
      />,
    );
    expect(busyHere).toContain("注册中…");
    expect(busyHere).toContain("disabled");
    // Same vendor on a different machine must not steal the spinner.
    const busyElsewhere = renderToString(
      <HostActionRow
        hostId="local"
        hostname="devbox"
        online
        actions={pendingActionsFor(LOCAL)}
        busy="sat-1:claude"
        onRegister={() => {}}
        onImport={() => {}}
      />,
    );
    expect(busyElsewhere).not.toContain("注册中…");
    expect(busyElsewhere).toContain("注册 MCP");
  });

  it("renders English labels when lang='en'", () => {
    const html = renderToString(
      <HostActionRow
        hostId="local"
        hostname="devbox"
        online
        actions={[]}
        busy={null}
        lang="en"
        onRegister={() => {}}
        onImport={() => {}}
      />,
    );
    expect(html).toContain("Nothing to do");
  });
});

describe("HostActionRow click wiring", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("register click reaches POST /hosts/{host}/register-mcp for that vendor", () => {
    const clicks = collectOnClicks(
      HostActionRow({
        hostId: "local",
        hostname: "devbox",
        online: true,
        actions: pendingActionsFor(LOCAL),
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
      HostActionRow({
        hostId: "sat-1",
        hostname: "gpu-box",
        online: true,
        actions: pendingActionsFor(SAT),
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

describe("HostsView (action panel shell)", () => {
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

  it("no longer renders the per-vendor observation grid", () => {
    const html = renderToString(
      <MemoryRouter>
        <HostsView />
      </MemoryRouter>,
    );
    expect(html).not.toContain("host-card");
    expect(html).not.toContain("agent-row");
    expect(html).not.toContain("agents-absent-row");
    expect(html).not.toContain("host-projects");
    expect(html).not.toContain("agent-version-");
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

  it("HostsView points to Settings · Access instead of embedding the join card", () => {
    const html = renderToString(
      <MemoryRouter>
        <HostsView />
      </MemoryRouter>,
    );
    expect(html).not.toContain('data-testid="join-card"');
    expect(html).toContain('href="/settings/access"');
    expect(html).toContain("连接新主机 → 设置·接入");
  });
});
