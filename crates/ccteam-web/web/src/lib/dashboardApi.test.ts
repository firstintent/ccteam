import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { createProject, fetchDashboard, type DashboardRow } from "./dashboardApi";

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
        path: "/home/u/dev-foo",
        team: "dev",
        kind: "workflow",
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

describe("createProject", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn();
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("POSTs {slug,path} to /api/v1/projects with same-origin + json headers", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(201, { slug: "demo", path: "/home/u/demo" }));
    await createProject("demo", "~/demo");
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/projects", {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: JSON.stringify({ slug: "demo", path: "~/demo" }),
    });
    // team is intentionally omitted so the backend defaults it to "dev".
    const sent = JSON.parse(fetchMock.mock.calls[0][1]!.body as string);
    expect(sent).toEqual({ slug: "demo", path: "~/demo" });
  });

  it("returns the created {slug,path} on 201", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(201, { slug: "demo", path: "/home/u/demo" }),
    );
    const got = await createProject("demo", "~/demo");
    expect(got).toEqual({ slug: "demo", path: "/home/u/demo" });
  });

  it("lifts the JSON error body on 409 (not a bare 'HTTP 409')", async () => {
    // Proves we read {ok:false,error} rather than discarding it — the user
    // must see "项目已存在" rather than just the status code.
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(409, { ok: false, error: "project already exists: demo" }),
    );
    const err = await createProject("demo", "~/demo").catch((e) => e as Error);
    expect(err.message).toBe("project already exists: demo");
    expect(err.message).toContain("demo");
    expect(err.message).not.toBe("HTTP 409");
  });

  it("surfaces the backend error message on a 400 bad slug/path", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(400, { ok: false, error: "invalid slug: must match [a-z0-9-]+" }),
    );
    await expect(createProject("BAD", "~/x")).rejects.toThrow(
      "invalid slug: must match [a-z0-9-]+",
    );
  });

  it("throws UNAUTHENTICATED on 401", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(401, { error: "auth" }));
    await expect(createProject("demo", "~/demo")).rejects.toThrow("UNAUTHENTICATED");
  });

  it("includes team in the body when provided", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(201, { slug: "demo", path: "/home/u/demo" }));
    await createProject("demo", "~/demo", "qa");
    const sent = JSON.parse(fetchMock.mock.calls[0][1]!.body as string);
    expect(sent).toEqual({ slug: "demo", path: "~/demo", team: "qa" });
  });
});
