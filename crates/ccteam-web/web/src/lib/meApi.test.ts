// v0.8.18 档1 — meApi.ts unit tests (fetch-spy, node env).

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { getMe, resetToken } from "./meApi";

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

  it("resetToken POSTs /api/v1/me/reset-token and returns the NEW wire token", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(200, { wire_token: "ccteam:deadbeef" }),
    );
    const got = await resetToken();
    expect(globalThis.fetch).toHaveBeenCalledWith(
      "/api/v1/me/reset-token",
      expect.objectContaining({ method: "POST", credentials: "same-origin" }),
    );
    expect(got.wire_token).toBe("ccteam:deadbeef");
  });

  it("resetToken lifts the server error body (tenant 403 / auth-off 400)", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(400, { error: "auth is disabled (loopback / --no-auth) — no web token in use" }),
    );
    await expect(resetToken()).rejects.toThrow("auth is disabled");
  });
});
