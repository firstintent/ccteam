// v0.8.18 柱1 — hostsApi.ts unit tests (fetch-spy, node env).

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { deleteHost, getHostDetail, getHosts, getJoinToken, mintJoinToken, registerMcp } from "./hostsApi";

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
        projects: [
          { slug: "demo", path: "/srv/demo", cataloged: true, catalog_slug: "demo2" },
        ],
      }),
    );
    const got = await getHostDetail("local", true);
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/hosts/local?refresh=true",
      expect.objectContaining({ credentials: "same-origin" }),
    );
    expect(got.ccteam_version).toBe("0.8.18");
    expect(got.projects?.[0]).toEqual({
      slug: "demo",
      path: "/srv/demo",
      cataloged: true,
      catalog_slug: "demo2",
    });
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

  it("getJoinToken GETs /api/v1/hosts/join-token (token may be null)", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, { token: null }));
    const got = await getJoinToken();
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/hosts/join-token",
      expect.objectContaining({ credentials: "same-origin" }),
    );
    expect(got.token).toBeNull();
  });

  it("mintJoinToken POSTs a JSON body and returns the minted token", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(
      jsonResponse(201, {
        token: "deadbeef",
        label: "lab",
        command: "ccteam host join --daemon <daemon-url> --token deadbeef",
      }),
    );
    const got = await mintJoinToken("lab");
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/hosts/join-token",
      expect.objectContaining({
        method: "POST",
        body: JSON.stringify({ label: "lab" }),
        headers: expect.objectContaining({ "Content-Type": "application/json" }),
      }),
    );
    expect(got.token).toBe("deadbeef");
  });

  it("getJoinToken maps 403 → HTTP 403 (tenant fail-closed)", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(403, {}));
    await expect(getJoinToken()).rejects.toThrow("HTTP 403");
  });

  it("maps 401 → UNAUTHENTICATED and a 500 → HTTP 500", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(401, {}));
    await expect(getHosts()).rejects.toThrow("UNAUTHENTICATED");
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(500, {}));
    await expect(getHosts()).rejects.toThrow("HTTP 500");
  });

  it("deleteHost DELETEs /api/v1/hosts/{host} with no query by default", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, { host: "dxa347" }));
    const got = await deleteHost("dxa347");
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/hosts/dxa347", {
      method: "DELETE",
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
    expect(got).toEqual({ host: "dxa347" });
  });

  it("deleteHost appends ?force=true when opts.force is set (and omits it when false)", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, { host: "smoke-self" }));
    await deleteHost("smoke-self", { force: true });
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/hosts/smoke-self?force=true",
      expect.objectContaining({ method: "DELETE" }),
    );

    fetchMock.mockResolvedValueOnce(jsonResponse(200, { host: "smoke-self" }));
    await deleteHost("smoke-self", { force: false });
    expect(fetchMock).toHaveBeenLastCalledWith(
      "/api/v1/hosts/smoke-self",
      expect.objectContaining({ method: "DELETE" }),
    );
  });

  it("deleteHost surfaces the server's {error} body on a non-2xx (e.g. 409 online-without-force)", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(409, { error: "host dxa347 is online; pass ?force=true to remove a live satellite" }),
    );
    await expect(deleteHost("dxa347")).rejects.toThrow(
      "host dxa347 is online; pass ?force=true to remove a live satellite",
    );
  });

  it("deleteHost falls back to HTTP <status> when the error body has no usable message", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(404, {}));
    await expect(deleteHost("nope")).rejects.toThrow("HTTP 404");
  });

  it("deleteHost maps 401 → UNAUTHENTICATED", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(401, {}));
    await expect(deleteHost("dxa347")).rejects.toThrow("UNAUTHENTICATED");
  });
});
