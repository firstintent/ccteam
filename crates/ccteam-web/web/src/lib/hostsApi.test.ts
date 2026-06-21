// v0.8.18 柱1 — hostsApi.ts unit tests (fetch-spy, node env).

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { getHostDetail, getHosts, registerMcp } from "./hostsApi";

const realFetch = globalThis.fetch;

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

describe("hostsApi", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn();
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("getHosts GETs /api/v1/hosts with same-origin creds", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(
      jsonResponse(200, {
        hosts: [
          { host: "local", hostname: "box", is_local: true, agent_count: 2, agents_ready: 1 },
        ],
      }),
    );
    const got = await getHosts();
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/hosts", {
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
    expect(got.hosts[0].hostname).toBe("box");
    expect(got.hosts[0].agents_ready).toBe(1);
  });

  it("getHostDetail GETs /api/v1/hosts/{host} and passes ?refresh=true", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(
      jsonResponse(200, {
        host: "local",
        hostname: "box",
        is_local: true,
        os: "linux",
        arch: "x86_64",
        ccteam_version: "0.8.18",
        agents: [],
      }),
    );
    const got = await getHostDetail("local", true);
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/hosts/local?refresh=true",
      expect.objectContaining({ credentials: "same-origin" }),
    );
    expect(got.ccteam_version).toBe("0.8.18");
  });

  it("getHostDetail omits the refresh query by default", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(
      jsonResponse(200, {
        host: "local",
        hostname: "box",
        is_local: true,
        os: "linux",
        arch: "x86_64",
        ccteam_version: "0.8.18",
        agents: [],
      }),
    );
    await getHostDetail("local");
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/hosts/local", expect.anything());
  });

  it("registerMcp POSTs with the optional vendor query", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(
      jsonResponse(200, { registered: ["claude"], paths: { claude: "/home/u/.claude.json" } }),
    );
    const got = await registerMcp("local", "claude");
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/hosts/local/register-mcp?vendor=claude",
      expect.objectContaining({ method: "POST", credentials: "same-origin" }),
    );
    expect(got.registered).toContain("claude");
  });

  it("registerMcp without a vendor registers all (no query)", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(
      jsonResponse(200, { registered: ["claude", "codex"], paths: {} }),
    );
    await registerMcp("local");
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/hosts/local/register-mcp",
      expect.objectContaining({ method: "POST" }),
    );
  });

  it("maps 401 → UNAUTHENTICATED and a 500 → HTTP 500", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(401, {}));
    await expect(getHosts()).rejects.toThrow("UNAUTHENTICATED");
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(500, {}));
    await expect(getHosts()).rejects.toThrow("HTTP 500");
  });
});
