// v0.8.18 档1 — meApi.ts unit tests (fetch-spy, node env).

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { getMe } from "./meApi";

const realFetch = globalThis.fetch;

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

describe("meApi", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn();
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("getMe GETs /api/v1/me with same-origin creds", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(200, { id: "admin", handle: "owner", is_admin: true }),
    );
    const me = await getMe();
    expect(globalThis.fetch).toHaveBeenCalledWith("/api/v1/me", {
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
    expect(me.is_admin).toBe(true);
    expect(me.handle).toBe("owner");
  });

  it("maps 401 → UNAUTHENTICATED, 500 → HTTP 500", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(401, {}));
    await expect(getMe()).rejects.toThrow("UNAUTHENTICATED");
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(500, {}));
    await expect(getMe()).rejects.toThrow("HTTP 500");
  });
});
