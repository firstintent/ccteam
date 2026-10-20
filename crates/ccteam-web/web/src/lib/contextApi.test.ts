// `GET /api/v1/sessions/{sid}/context` client — URL, auth posture, error
// mapping, and tolerance for the contract's nullable / unknown fields.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { getSessionContext, sessionContextUrl } from "./contextApi";

const realFetch = globalThis.fetch;

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

describe("contextApi", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn();
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("builds the per-session URL with an encoded sid", () => {
    expect(sessionContextUrl("s12")).toBe("/api/v1/sessions/s12/context");
    expect(sessionContextUrl("a/b")).toBe("/api/v1/sessions/a%2Fb/context");
  });

  it("GETs with same-origin creds and returns the body verbatim (unknown fields kept)", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(200, {
        sid: "s12",
        live: true,
        breakdown: { vendor: "claude", categories: [], brand_new_field: 1 },
        extra_top_level: "ignored",
      }),
    );
    const got = await getSessionContext("s12");
    expect(globalThis.fetch).toHaveBeenCalledWith("/api/v1/sessions/s12/context", {
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
    expect(got.sid).toBe("s12");
    expect(got.live).toBe(true);
    expect(got.breakdown?.vendor).toBe("claude");
  });

  it("passes a null breakdown through (no breakdown is a fact, not an error)", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(200, { sid: "s3", live: false, breakdown: null }),
    );
    const got = await getSessionContext("s3");
    expect(got.breakdown).toBeNull();
    expect(got.live).toBe(false);
  });

  it("maps 401 → UNAUTHENTICATED and 404 → NOT_FOUND", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(401, {}));
    await expect(getSessionContext("s12")).rejects.toThrow("UNAUTHENTICATED");
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(404, { error: "unknown sid" }));
    await expect(getSessionContext("s12")).rejects.toThrow("NOT_FOUND");
  });

  it("lifts the server's reason on 503 (no gateway) and other failures", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(503, { error: "no gateway (standalone web)" }),
    );
    await expect(getSessionContext("s12")).rejects.toThrow("HTTP 503: no gateway (standalone web)");
  });

  it("wraps a network failure", async () => {
    vi.mocked(globalThis.fetch).mockRejectedValueOnce(new TypeError("Failed to fetch"));
    await expect(getSessionContext("s12")).rejects.toThrow("network: Failed to fetch");
  });

  it("forwards an AbortSignal when given", async () => {
    const controller = new AbortController();
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(200, { sid: "s12", live: true, breakdown: null }),
    );
    await getSessionContext("s12", { signal: controller.signal });
    expect(globalThis.fetch).toHaveBeenCalledWith(
      "/api/v1/sessions/s12/context",
      expect.objectContaining({ signal: controller.signal }),
    );
  });
});
