// v0.9.11 TEAM-2 — routingApi.ts unit tests (fetch-spy, node env; mirrors
// hostsApi.test.ts conventions).

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { getRouting, putRouting, routingUrl } from "./routingApi";

const realFetch = globalThis.fetch;

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

describe("routingApi", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn();
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("routingUrl encodes the slug", () => {
    expect(routingUrl("demo")).toBe("/api/v1/projects/demo/routing");
    expect(routingUrl("a b")).toBe("/api/v1/projects/a%20b/routing");
  });

  it("getRouting GETs the charter with same-origin creds", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(
      jsonResponse(200, {
        exists: true,
        source: "global",
        path: "/srv/demo/.ccteam/routing.md",
        fallback_path: "/home/u/.ccteam/routing.md",
        content: "# charter",
        sha256: "abc",
        updated_at: "2026-07-29T00:00:00+00:00",
      }),
    );
    const got = await getRouting("demo");
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/projects/demo/routing", {
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
    expect(got.source).toBe("global");
    expect(got.fallback_path).toBe("/home/u/.ccteam/routing.md");
  });

  it("putRouting PUTs {content} as JSON and returns the save receipt", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(
      jsonResponse(200, { sha256: "deadbeef", updated_at: "2026-07-29T00:00:00+00:00" }),
    );
    const got = await putRouting("demo", "# 分工\n");
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/projects/demo/routing",
      expect.objectContaining({
        method: "PUT",
        body: JSON.stringify({ content: "# 分工\n" }),
        headers: expect.objectContaining({ "Content-Type": "application/json" }),
        credentials: "same-origin",
      }),
    );
    expect(got.sha256).toBe("deadbeef");
  });

  it("maps 401 → UNAUTHENTICATED, 413 → HTTP 413, 404 → HTTP 404", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(401, {}));
    await expect(getRouting("demo")).rejects.toThrow("UNAUTHENTICATED");
    fetchMock.mockResolvedValueOnce(jsonResponse(413, { error: "too big" }));
    await expect(putRouting("demo", "x")).rejects.toThrow("HTTP 413");
    fetchMock.mockResolvedValueOnce(jsonResponse(404, {}));
    await expect(getRouting("ghost")).rejects.toThrow("HTTP 404");
  });
});
