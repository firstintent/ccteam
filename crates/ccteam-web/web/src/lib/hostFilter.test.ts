import { describe, expect, it } from "vitest";
import { allowedVendorsFor, eligibleHosts } from "./hostFilter";
import type { AgentHealth, HostDetail, HostProjectView, HostSummary } from "./hostsApi";

function agent(vendor: string, installed: boolean): AgentHealth {
  return {
    vendor,
    harness_id: vendor,
    installed,
    version: installed ? "1.0.0" : null,
    bin: vendor,
    mcp_registered: false,
    tool_surface: "managed_session_bridge",
    status: installed ? "ready" : "not_installed",
    hint: null,
  };
}

function summary(host: string, isLocal: boolean, status = "online"): HostSummary {
  return { host, hostname: host.toUpperCase(), is_local: isLocal, status, agent_count: 4, agents_ready: 1 };
}

function detail(
  host: string,
  agents: AgentHealth[],
  projects: HostProjectView[] = [],
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

  it("a new project gets every online host with an installed agent, local first", () => {
    const online = summary("online-sat", false);
    const offline = summary("offline-sat", false, "offline");
    const empty = summary("empty-sat", false);
    const details = {
      "online-sat": detail("online-sat", [agent("claude", true)]),
      "offline-sat": detail("offline-sat", [agent("claude", true)]),
      "empty-sat": detail("empty-sat", [agent("claude", false)]),
    };
    expect(eligibleHosts([online, offline, local, empty], details, "", true).map((h) => h.host)).toEqual([
      "local",
      "online-sat",
    ]);
  });

  it("an existing project gets exactly its bound host, even when offline", () => {
    const offlineSat = summary("dxa347", false, "offline");
    expect(eligibleHosts([local, offlineSat], {}, "dxa347", false)).toEqual([offlineSat]);
    expect(eligibleHosts([local, sat], {}, "local", false)).toEqual([local]);
  });

  it("synthesizes a readable offline identity when a bound host is absent", () => {
    expect(eligibleHosts([local], {}, "missing-sat", false)[0]).toMatchObject({
      host: "missing-sat",
      status: "offline",
      is_local: false,
    });
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
