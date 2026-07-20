// v0.8.24 gap-fill — workflowApi MCP-server client.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { getMcpServers, registerMcpServer } from "./workflowApi";

const realFetch = globalThis.fetch;

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

describe("workflowApi mcp-servers", () => {
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
});
