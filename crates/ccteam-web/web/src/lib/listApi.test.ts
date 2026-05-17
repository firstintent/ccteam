// V0.5.1 F103a — listApi.ts unit tests.
//
// Mirrors the dashboardApi / teamsApi shape: spy on `fetch`, assert
// URL + headers + body shape, plus error mapping for 401 → throw.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  fetchAllActiveSessions,
  type ActiveSessionWithSlug,
} from "./listApi";

const realFetch = globalThis.fetch;

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

describe("fetchAllActiveSessions", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn();
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("hits /api/v1/sessions/active with same-origin credentials", async () => {
    const rows: ActiveSessionWithSlug[] = [
      {
        slug: "dex-ui",
        role: "planner",
        session_id: "planner-1",
        job_id: "j1",
        cwd: "/tmp/dex-ui",
        started_at: "2026-05-17T09:00:00Z",
        cost_usd: 0.42,
        model: "deepseek",
        context_remaining_pct: 97,
      },
    ];
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, rows));
    const got = await fetchAllActiveSessions();
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/sessions/active", {
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
    expect(got).toEqual(rows);
  });

  it("returns an empty array when no sessions are running", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(200, []));
    const got = await fetchAllActiveSessions();
    expect(got).toEqual([]);
  });

  it("throws UNAUTHENTICATED on 401", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(401, { error: "auth" }),
    );
    await expect(fetchAllActiveSessions()).rejects.toThrow("UNAUTHENTICATED");
  });

  it("throws a generic HTTP error on 500", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(500, { error: "boom" }),
    );
    await expect(fetchAllActiveSessions()).rejects.toThrow("HTTP 500");
  });
});
