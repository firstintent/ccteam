// v0.8.18 柱1 — HostsView smoke tests.
//
// No DOM env: renderToString for the loading shell + the seeded
// HostDetailCards sub-component. The hostsApi fetch/mapping is covered by
// hostsApi.test.ts.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { renderToString } from "react-dom/server";
import HostsView, { HostDetailCards, JoinCard } from "./HostsView";
import type { HostDetail } from "../lib/hostsApi";

const realFetch = globalThis.fetch;

/** Strip SSR comment markers so interpolated `{a}/{b}` text is contiguous. */
function visibleText(html: string): string {
  return html.replace(/<!-- -->/g, "");
}

const HOST: HostDetail = {
  host: "local",
  hostname: "devbox",
  is_local: true,
  os: "linux",
  arch: "x86_64",
  ccteam_version: "0.8.18",
  agents: [
    {
      vendor: "claude",
      harness_id: "claude-code",
      installed: true,
      version: "claude 1.2.3",
      bin: "claude",
      mcp_registered: false,
      mcp_registrable: true,
      status: "needs_config",
      hint: "register the ccteam MCP server: POST /api/v1/hosts/local/register-mcp?vendor=claude",
    },
    {
      vendor: "codex",
      harness_id: "codex",
      installed: false,
      version: null,
      bin: "codex",
      mcp_registered: false,
      mcp_registrable: true,
      status: "not_installed",
      hint: "codex not found on PATH",
    },
    {
      // Retained non-registrable shape models satellite rows, whose host-detail
      // fold forces mcp_registrable=false even for globally registrable vendors.
      vendor: "grok",
      harness_id: "grok",
      installed: true,
      version: "grok 0.2.93",
      bin: "grok",
      mcp_registered: false,
      mcp_registrable: false,
      status: "ready",
      hint: null,
    },
    {
      // Same satellite-fold shape for OpenCode.
      vendor: "opencode",
      harness_id: "opencode",
      installed: true,
      version: "opencode 0.6.4",
      bin: "opencode",
      mcp_registered: false,
      mcp_registrable: false,
      status: "ready",
      hint: null,
    },
    {
      // kimi (5th vendor): config-file MCP seam ($KIMI_CODE_HOME/mcp.json),
      // registered → ready, no CTA.
      vendor: "kimi",
      harness_id: "kimi",
      installed: true,
      version: "kimi 0.26.0",
      bin: "kimi",
      mcp_registered: true,
      mcp_registrable: true,
      status: "ready",
      hint: null,
    },
  ],
};

describe("HostsView initial render", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("renders the view + loading placeholder before the host probe resolves", () => {
    const html = renderToString(<HostsView />);
    expect(html).toContain('data-testid="hosts-view"');
    expect(html).toContain('data-testid="hosts-loading"');
    expect(html).toContain("主机");
  });
});

describe("HostDetailCards (seeded)", () => {
  it("renders the hostname bar + per-agent cards + their statuses", () => {
    const html = renderToString(<HostDetailCards host={HOST} busy={null} onRegister={() => {}} onImport={() => {}} />);
    expect(html).toContain('data-testid="host-bar"');
    expect(html).toContain("devbox");
    expect(visibleText(html)).toContain("linux/x86_64");
    expect(html).toContain('data-testid="agent-card-claude"');
    expect(html).toContain('data-testid="agent-card-codex"');
    expect(html).toContain('data-testid="agent-card-grok"');
    expect(html).toContain('data-testid="agent-card-opencode"');
    expect(html).toContain('data-testid="agent-card-kimi"');
    expect(html).toContain("需配置"); // claude needs_config
    expect(html).toContain("未安装"); // codex not_installed
    expect(html).toContain("claude 1.2.3"); // captured version string
  });

  it("shows the register-MCP button only for an installed-but-unregistered registrable agent", () => {
    const html = renderToString(<HostDetailCards host={HOST} busy={null} onRegister={() => {}} onImport={() => {}} />);
    // claude is installed + MCP not registered → button present.
    expect(html).toContain('data-testid="register-mcp-claude"');
    // codex is not installed → no register button (ccteam never installs a CLI).
    expect(html).not.toContain('data-testid="register-mcp-codex"');
    // Satellite-shaped non-registrable row → never a no-op CTA.
    expect(html).not.toContain('data-testid="register-mcp-grok"');
    expect(html).toContain("随会话协议");
  });

  it("shows the register-MCP CTA for local Grok when its config is missing", () => {
    const localGrok: HostDetail = {
      ...HOST,
      agents: [
        {
          vendor: "grok",
          harness_id: "grok",
          installed: true,
          version: "grok 0.2.112",
          bin: "grok",
          mcp_registered: false,
          mcp_registrable: true,
          status: "needs_config",
          hint: "register the ccteam MCP server",
        },
      ],
    };
    const html = renderToString(
      <HostDetailCards host={localGrok} busy={null} onRegister={() => {}} onImport={() => {}} />,
    );
    expect(html).toContain('data-testid="register-mcp-grok"');
  });

  it("renders a ready agent with the 就绪 badge and no register button", () => {
    const ready: HostDetail = {
      ...HOST,
      agents: [
        {
          vendor: "claude",
          harness_id: "claude-code",
          installed: true,
          version: "claude 1.2.3",
          bin: "claude",
          mcp_registered: true,
          mcp_registrable: true,
          status: "ready",
          hint: null,
        },
      ],
    };
    const html = renderToString(<HostDetailCards host={ready} busy={null} onRegister={() => {}} onImport={() => {}} />);
    expect(html).toContain("就绪");
    expect(html).not.toContain('data-testid="register-mcp-claude"');
  });

  it("shows catalog state/alias and an import CTA for uncataloged satellite projects", () => {
    const remote: HostDetail = {
      ...HOST,
      host: "sat-1",
      hostname: "sat-1",
      is_local: false,
      projects: [
        { slug: "already", path: "/srv/already", cataloged: true, catalog_slug: "already-local" },
        { slug: "fresh", path: "/srv/fresh", cataloged: false, catalog_slug: null },
      ],
    };
    const html = renderToString(
      <HostDetailCards host={remote} busy={null} onRegister={() => {}} onImport={() => {}} />,
    );
    expect(html).toContain("已接入");
    expect(html).toContain("already-local");
    expect(html).toContain('data-testid="import-project-fresh"');
    expect(html).not.toContain('data-testid="import-project-already"');
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
    const html = renderToString(<HostsView />);
    expect(html).not.toContain('data-testid="join-card"');
    expect(html).toContain('href="/settings/access"');
    expect(html).toContain("连接新主机 → 设置·接入");
  });
});
