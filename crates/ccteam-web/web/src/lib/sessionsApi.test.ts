// v0.8.7 W4 (DD.1) — sessionsApi.ts unit tests.
//
// Mirrors the listApi/dashboardApi pattern: spy on `fetch`, assert URL +
// method + body shape + error mapping. Runs under node env (no DOM).

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  createSession,
  getHistory,
  listSessions,
  resolveApproval,
  sessionUrl,
  sessionsUrl,
  stopSession,
  submitTurn,
  type SessionView,
} from "./sessionsApi";

const realFetch = globalThis.fetch;

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

describe("sessionsApi url builders", () => {
  it("targets the gateway s{n} namespace under /api/v1", () => {
    expect(sessionsUrl("dex-ui")).toBe("/api/v1/projects/dex-ui/sessions");
    expect(sessionUrl("s2")).toBe("/api/v1/sessions/s2");
    // NOT the legacy /sessions/active surface.
    expect(sessionsUrl("x")).not.toContain("/active");
  });

  it("encodes slug + sid", () => {
    expect(sessionsUrl("a b")).toBe("/api/v1/projects/a%20b/sessions");
    expect(sessionUrl("s/odd")).toBe("/api/v1/sessions/s%2Fodd");
  });
});

describe("sessionsApi", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn();
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("listSessions GETs the per-project list with same-origin creds", async () => {
    const rows: SessionView[] = [
      {
        sid: "s1",
        project: "dex-ui",
        role: "cto",
        vendor: "claude",
        permission_mode: "skip",
        current: true,
        status: "live",
      },
    ];
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, rows));
    const got = await listSessions("dex-ui");
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/projects/dex-ui/sessions", {
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
    expect(got).toEqual(rows);
  });

  it("listSessions returns [] when no live session", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(200, []));
    expect(await listSessions("empty")).toEqual([]);
  });

  it("getHistory GETs /sessions/{sid} and returns {sid,events}", async () => {
    const history = {
      sid: "s1",
      events: [
        { turn_id: "t1", ts: "2026-06-06T00:00:00Z", role: "cto", user: "hi", assistant: "yo" },
      ],
    };
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, history));
    const got = await getHistory("s1");
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/sessions/s1", {
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
    expect(got.events[0].assistant).toBe("yo");
  });

  it("submitTurn POSTs {text} to /sessions/{sid}/turn", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(202, { accepted: true }));
    const got = await submitTurn("s2", "review the diff");
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/sessions/s2/turn", {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: JSON.stringify({ text: "review the diff" }),
    });
    expect(got.accepted).toBe(true);
  });

  it("stopSession POSTs to /sessions/{sid}/stop", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, { stopped: true }));
    const got = await stopSession("s3");
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/sessions/s3/stop", {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: JSON.stringify({}),
    });
    expect(got.stopped).toBe(true);
  });

  it("resolveApproval POSTs {token,selection} to /sessions/{sid}/resolve (R-H1)", async () => {
    // The web HITL approve path — NOT a turn. It must hit /resolve with the
    // pending token + the chosen option id, so the gateway resolves the same
    // token-keyed pending an IM click does.
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, { resolved: true }));
    const got = await resolveApproval("s2", "pdeadbeef", "allow");
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/sessions/s2/resolve", {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: JSON.stringify({ token: "pdeadbeef", selection: "allow" }),
    });
    expect(got.resolved).toBe(true);
    // It must NOT be the turn endpoint (the old broken path).
    expect(fetchMock.mock.calls[0][0]).not.toContain("/turn");
  });

  it("resolveApproval maps an unknown/expired token (404) to NOT_FOUND", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(404, { error: "gone" }));
    await expect(resolveApproval("s2", "stale", "deny")).rejects.toThrow("NOT_FOUND");
  });

  it("createSession POSTs role+vendor+permission_mode to the project list", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(201, { sid: "s4" }));
    const got = await createSession("dex-ui", {
      role: "cto",
      vendor: "claude",
      permission_mode: "hitl",
    });
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/projects/dex-ui/sessions", {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: JSON.stringify({ role: "cto", vendor: "claude", permission_mode: "hitl" }),
    });
    expect(got.sid).toBe("s4");
  });

  it("createSession omits optional fields when not given", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(201, { sid: "s5" }));
    await createSession("dex-ui", { role: "cto" });
    const body = JSON.parse(vi.mocked(globalThis.fetch).mock.calls[0][1]!.body as string);
    expect(body).toEqual({ role: "cto" });
  });

  it("maps 401 → UNAUTHENTICATED and 404 → NOT_FOUND", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(401, { error: "auth" }));
    await expect(listSessions("x")).rejects.toThrow("UNAUTHENTICATED");
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(404, { error: "nope" }));
    await expect(getHistory("sX")).rejects.toThrow("NOT_FOUND");
  });
});
