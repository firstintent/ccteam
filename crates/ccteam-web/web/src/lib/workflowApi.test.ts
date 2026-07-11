// v0.8.24 gap-fill — workflowApi client: mcp-servers + compare history
// (fetch-spy, node env; same pattern as hostsApi.test.ts).

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { getCompareHistory, getMcpServers, registerMcpServer } from "./workflowApi";

const realFetch = globalThis.fetch;

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

describe("workflowApi mcp-servers + compare history", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn();
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("getMcpServers GETs the project mcp-servers path", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(
      jsonResponse(200, {
        servers: [
          { name: "ccteam", kind: "stdio", command: "/usr/local/bin/ccteam", is_ccteam: true },
          { name: "context7", kind: "http", url: "https://mcp.context7.com/mcp", is_ccteam: false },
        ],
        ccteam_registered: true,
      }),
    );
    const got = await getMcpServers("demo");
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/projects/demo/mcp-servers",
      expect.objectContaining({ credentials: "same-origin" }),
    );
    expect(got.ccteam_registered).toBe(true);
    expect(got.servers[1].url).toContain("context7");
  });

  it("registerMcpServer POSTs the form body (url XOR command)", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(
      jsonResponse(201, { ok: true, name: "playwright", path: "/p/.mcp.json" }),
    );
    const got = await registerMcpServer("demo", {
      name: "playwright",
      command: "npx",
      args: ["@playwright/mcp@latest"],
    });
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/projects/demo/mcp-servers",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({
          name: "playwright",
          command: "npx",
          args: ["@playwright/mcp@latest"],
        }),
      }),
    );
    expect(got.ok).toBe(true);
  });

  it("registerMcpServer lifts the server's human error body", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(400, { error: "`ccteam` is reserved for ccteam's own server" }),
    );
    await expect(registerMcpServer("demo", { name: "ccteam", url: "https://x" })).rejects.toThrow(
      "reserved",
    );
  });

  it("getCompareHistory GETs the history path and returns groups", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(
      jsonResponse(200, {
        groups: [
          {
            group: "cmp-1",
            created_at: "2026-07-10T10:00:00Z",
            prompt: "why flaky?",
            members: [
              { sid: "s1", vendor: "claude", cost_usd: 0.02 },
              { sid: "s2", vendor: "codex", cost_usd: null },
            ],
            cost_subtotal_usd: 0.02,
          },
        ],
      }),
    );
    const got = await getCompareHistory("demo");
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/projects/demo/compare/history",
      expect.objectContaining({ credentials: "same-origin" }),
    );
    expect(got.groups[0].members.map((m) => m.vendor)).toEqual(["claude", "codex"]);
  });
});
