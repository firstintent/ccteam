import { describe, expect, it } from "vitest";
import { allowedVendorsFor, eligibleHosts } from "./hostFilter";
import type { AgentHealth, HostDetail, HostSummary } from "./hostsApi";

function agent(vendor: string, installed: boolean): AgentHealth {
  return {
    vendor,
    harness_id: vendor,
    installed,
    version: installed ? "1.0.0" : null,
    bin: vendor,
    mcp_registered: false,
    mcp_registrable: false,
    status: installed ? "ready" : "not_installed",
    hint: null,
  };
}

function summary(host: string, isLocal: boolean): HostSummary {
  return { host, hostname: host.toUpperCase(), is_local: isLocal, agent_count: 4, agents_ready: 1 };
}

function detail(
  host: string,
  agents: AgentHealth[],
  projects: { slug: string; path: string }[],
): HostDetail {
  return {
    host,
    hostname: host.toUpperCase(),
    is_local: false,
    os: "macos",
    arch: "aarch64",
    ccteam_version: "0.9.0",
    agents,
    projects,
  };
}

describe("eligibleHosts (项目绑定主机)", () => {
  const local = summary("local", true);
  const sat = summary("dxa347", false);

  it("local is always eligible; a remote needs the slug + an installed agent", () => {
    const details = {
      dxa347: detail("dxa347", [agent("claude", true)], [{ slug: "demo", path: "/w/demo" }]),
    };
    expect(eligibleHosts([local, sat], details, "demo", false).map((h) => h.host)).toEqual([
      "local",
      "dxa347",
    ]);
    // Same satellite, different project → local only.
    expect(eligibleHosts([local, sat], details, "other", false).map((h) => h.host)).toEqual([
      "local",
    ]);
  });

  it("a new-project path pins the host to local", () => {
    const details = {
      dxa347: detail("dxa347", [agent("claude", true)], [{ slug: "demo", path: "/w/demo" }]),
    };
    expect(eligibleHosts([local, sat], details, "demo", true).map((h) => h.host)).toEqual([
      "local",
    ]);
  });

  it("a remote with no detail (offline) or no installed agent is not spawnable", () => {
    expect(eligibleHosts([local, sat], {}, "demo", false).map((h) => h.host)).toEqual(["local"]);
    const noAgents = {
      dxa347: detail("dxa347", [agent("claude", false)], [{ slug: "demo", path: "/w/demo" }]),
    };
    expect(eligibleHosts([local, sat], noAgents, "demo", false).map((h) => h.host)).toEqual([
      "local",
    ]);
  });
});

describe("allowedVendorsFor (主机绑定 vendor)", () => {
  it("returns installed vendors in menu order", () => {
    const d = detail(
      "dxa347",
      [agent("grok", false), agent("codex", true), agent("claude", true)],
      [],
    );
    expect(allowedVendorsFor(d)).toEqual(["claude", "codex"]);
  });

  it("fails open (null) when the detail is unknown or nothing is installed", () => {
    expect(allowedVendorsFor(null)).toBeNull();
    expect(allowedVendorsFor(undefined)).toBeNull();
    const none = detail("dxa347", [agent("claude", false)], []);
    expect(allowedVendorsFor(none)).toBeNull();
  });
});
