import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fetchDashboard, type DashboardRow } from "./dashboardApi";

// Spy on the global `fetch` per-test so the production code keeps using
// the platform API verbatim. We restore in afterEach so cross-file test
// ordering can't leak a fake into other suites.
const realFetch = globalThis.fetch;

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

describe("fetchDashboard", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn();
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("hits /api/v1/projects with same-origin credentials", async () => {
    const rows: DashboardRow[] = [
      {
        slug: "dev-foo",
        team: "dev",
        kind: "workflow",
        current_phase: "plan-eng",
        last_event_label: "5s ago",
        badge_class: "bg-status-running",
        badge_label: "Running",
        cost_label: "$0.42",
      },
    ];
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, rows));
    const result = await fetchDashboard();
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/projects", {
      credentials: "same-origin",
    });
    expect(result).toEqual(rows);
  });

  it("throws UNAUTHENTICATED on 401", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(401, { error: "x" }));
    await expect(fetchDashboard()).rejects.toThrow("UNAUTHENTICATED");
  });

  it("throws generic error on other non-2xx", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(503, { error: "down" }));
    await expect(fetchDashboard()).rejects.toThrow("/api/v1/projects: 503");
  });
});
