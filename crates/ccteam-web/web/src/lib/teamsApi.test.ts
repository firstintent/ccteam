// V0.5.0 F96 — unit tests for the Agent Teams API client + pure helpers.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  colorClasses,
  deriveMemberState,
  fetchMemberDefinition,
  fetchTeamDetail,
  fetchTeamInbox,
  fetchTeamTasks,
  fetchTeams,
  relativeFromEpoch,
  teamEventsUrl,
  type TeamListEntry,
} from "./teamsApi";

const realFetch = globalThis.fetch;

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

describe("teamsApi fetchers", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn();
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("fetchTeams hits /api/v1/teams with same-origin credentials", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    const rows: TeamListEntry[] = [
      {
        name: "roblog",
        description: "blog",
        member_count: 5,
        last_activity: null,
      },
    ];
    fetchMock.mockResolvedValueOnce(jsonResponse(200, rows));
    const got = await fetchTeams();
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/teams", {
      credentials: "same-origin",
    });
    expect(got).toEqual(rows);
  });

  it("fetchTeams throws UNAUTHENTICATED on 401", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(401, { error: "x" }));
    await expect(fetchTeams()).rejects.toThrow("UNAUTHENTICATED");
  });

  it("fetchTeamDetail throws NOT_FOUND on 404", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(404, { error: "ghost" }));
    await expect(fetchTeamDetail("ghost")).rejects.toThrow("NOT_FOUND");
  });

  it("fetchTeamTasks encodes the team name in the URL", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, []));
    await fetchTeamTasks("audit loop");
    const url = fetchMock.mock.calls[0]?.[0];
    expect(url).toBe("/api/v1/teams/audit%20loop/tasks");
  });

  it("fetchTeamInbox builds query string with optional fields", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, []));
    await fetchTeamInbox("roblog", { teammate: "researcher", since: "2026-05-16T00:00:00Z" });
    const url = fetchMock.mock.calls[0]?.[0] as string;
    expect(url).toMatch(/^\/api\/v1\/teams\/roblog\/inbox\?/);
    expect(url).toContain("teammate=researcher");
    expect(url).toContain("since=2026-05-16T00%3A00%3A00Z");
  });

  it("fetchTeamInbox omits query string when no filter passed", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, []));
    await fetchTeamInbox("roblog");
    expect(fetchMock.mock.calls[0]?.[0]).toBe("/api/v1/teams/roblog/inbox");
  });

  it("fetchMemberDefinition propagates 404 (ad-hoc gives NOT_FOUND)", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(
      jsonResponse(404, { ad_hoc: true, error: "x" }),
    );
    await expect(
      fetchMemberDefinition("roblog", "researcher"),
    ).rejects.toThrow("NOT_FOUND");
  });
});

describe("teamEventsUrl", () => {
  it("encodes team name", () => {
    expect(teamEventsUrl("audit loop")).toBe(
      "/api/v1/teams/audit%20loop/events",
    );
  });
});

describe("colorClasses", () => {
  it("returns specific tailwind classes for known Anthropic colors", () => {
    expect(colorClasses("blue").bg).toContain("bg-blue");
    expect(colorClasses("green").bg).toContain("bg-emerald");
    expect(colorClasses("yellow").bg).toContain("bg-amber");
    expect(colorClasses("red").bg).toContain("bg-rose");
    expect(colorClasses("purple").bg).toContain("bg-violet");
  });
  it("falls back to neutral classes for unknown / null colors", () => {
    expect(colorClasses(null).bg).toContain("surface");
    expect(colorClasses("teal-puce").bg).toContain("surface");
  });
});

describe("deriveMemberState", () => {
  it("returns idle when isIdle even if backend is set", () => {
    expect(deriveMemberState("in-process", true)).toBe("idle");
    expect(deriveMemberState("tmux", true)).toBe("idle");
  });
  it("maps backendType to in-process / tmux", () => {
    expect(deriveMemberState("in-process", false)).toBe("in-process");
    expect(deriveMemberState("tmux", false)).toBe("tmux");
  });
  it("returns missing for empty or unknown backend", () => {
    expect(deriveMemberState("", false)).toBe("missing");
    expect(deriveMemberState(null, false)).toBe("missing");
    expect(deriveMemberState("vapor", false)).toBe("missing");
  });
});

describe("relativeFromEpoch", () => {
  it("formats recent times with seconds granularity", () => {
    const now = Date.parse("2026-05-17T12:00:00Z");
    // 30 seconds ago
    const out = relativeFromEpoch(now - 30_000, now);
    expect(out).toMatch(/30/);
  });
  it("returns em-dash for null input", () => {
    expect(relativeFromEpoch(null)).toBe("—");
  });
});
