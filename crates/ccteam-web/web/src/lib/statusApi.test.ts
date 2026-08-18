// v0.8.9 Phase 4 — statusApi.ts unit tests (fetch-spy, node env).

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { getStatus, type StatusSnapshot } from "./statusApi";

const realFetch = globalThis.fetch;

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

describe("getStatus", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn();
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("GETs /api/v1/status with same-origin creds and returns the snapshot", async () => {
    const snap: StatusSnapshot = {
      daemon_healthy: true,
      sessions_live: 3,
      sessions_idle: 1,
      cost_24h_usd: 2.14,
      cost_24h_by_vendor: { claude: 1.62, codex: 0.52 },
      budget_cap_24h: 20,
    };
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, snap));
    const got = await getStatus();
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/status", {
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
    expect(got.cost_24h_usd).toBe(2.14);
    expect(got.cost_24h_by_vendor.claude).toBe(1.62);
    expect(got.budget_cap_24h).toBe(20);
  });

  it("accepts a null budget cap (no project configures one)", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(200, {
        daemon_healthy: false,
        sessions_live: 0,
        sessions_idle: 0,
        cost_24h_usd: 0,
        cost_24h_by_vendor: {},
        budget_cap_24h: null,
      }),
    );
    const got = await getStatus();
    expect(got.budget_cap_24h).toBeNull();
    expect(got.daemon_healthy).toBe(false);
  });

  it("marks automatic store refreshes as background and forwards abort", async () => {
    const controller = new AbortController();
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(
      jsonResponse(200, {
        daemon_healthy: true,
        sessions_live: 0,
        sessions_idle: 0,
        cost_24h_usd: 0,
        cost_24h_by_vendor: {},
        budget_cap_24h: null,
      }),
    );

    await getStatus({ signal: controller.signal, background: true });

    const init = fetchMock.mock.calls[0]?.[1];
    expect(init?.signal).toBe(controller.signal);
    expect(new Headers(init?.headers).get("X-Ccteam-Background")).toBe("1");
  });

  it("maps 401 → UNAUTHENTICATED and a 500 → HTTP 500", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(401, { error: "auth" }));
    await expect(getStatus()).rejects.toThrow("UNAUTHENTICATED");
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(500, {}));
    await expect(getStatus()).rejects.toThrow("HTTP 500");
  });

  it("carries the per-session fleet cost rows when present", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(200, {
        daemon_healthy: true,
        sessions_live: 1,
        sessions_idle: 0,
        cost_24h_usd: 1.23,
        cost_24h_by_vendor: { claude: 1.23 },
        budget_cap_24h: null,
        sessions: [
          { sid: "s1", project: "demo", role: "cto", vendor: "claude", status: "live", cost_usd: 1.23 },
          // An unpriced session: cost_usd is null (determinism — no fabricated 0).
          { sid: "s2", project: "demo", role: "qa", vendor: "claude", status: "live", cost_usd: null, unpriced_turns: 1 },
        ],
      }),
    );
    const got = await getStatus();
    expect(got.sessions?.[0].sid).toBe("s1");
    expect(got.sessions?.[0].cost_usd).toBe(1.23);
    // The null cost survives the round-trip (not coerced to 0).
    expect(got.sessions?.[1].cost_usd).toBeNull();
    expect(got.sessions?.[1].unpriced_turns).toBe(1);
  });
});
