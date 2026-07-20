// v0.8.7 W4 (DD.1) — sessionsApi.ts unit tests.
//
// Mirrors the listApi/dashboardApi pattern: spy on `fetch`, assert URL +
// method + body shape + error mapping. Runs under node env (no DOM).

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  createSession,
  getHistory,
  getRoleDetail,
  getSessionStatus,
  importExternalSession,
  interruptSession,
  listExternalSessions,
  listHistorySessions,
  listProjectRoles,
  listSessions,
  resolveApproval,
  resumeSession,
  sessionUrl,
  sessionsUrl,
  stopSession,
  submitTurn,
  type ExternalSessionView,
  type HistorySessionView,
  type RoleDetail,
  type RoleSummary,
  type SessionStatus,
  type SessionView,
} from "./sessionsApi";

const realFetch = globalThis.fetch;

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function textResponse(status: number, body: string): Response {
  return new Response(body, {
    status,
    headers: { "content-type": "text/html" },
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

  it("getSessionStatus GETs /sessions/{sid}/status and returns the payload", async () => {
    const payload: SessionStatus = {
      sid: "s8",
      model: "claude-opus-4-8[1m]",
      context: { used_tokens: 188000, window_tokens: 1000000, pct: 18.8 },
      status_line: "claude-opus-4-8[1m] · ctx 188k / 1M (19%)",
    };
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, payload));
    const got = await getSessionStatus("s8");
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/sessions/s8/status", {
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
    expect(got).toEqual(payload);
  });

  it("getSessionStatus returns all-null for a brand-new session", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(200, { sid: "s9", model: null, context: null, status_line: null }),
    );
    const got = await getSessionStatus("s9");
    expect(got.model).toBeNull();
    expect(got.context).toBeNull();
    expect(got.status_line).toBeNull();
  });

  it("getSessionStatus surfaces 404 (unknown sid) and 503 (no live gateway)", async () => {
    // The SessionView caller catches either and hides the bar.
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(404, { error: "unknown session: sZ" }),
    );
    await expect(getSessionStatus("sZ")).rejects.toThrow("NOT_FOUND");
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(503, { error: "no live gateway: standalone web" }),
    );
    await expect(getSessionStatus("s8")).rejects.toThrow("no live gateway");
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

  it("submitTurn rides attachments[] on the body when present", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(202, { accepted: true }));
    await submitTurn("s2", "look", [
      { kind: "image", path: "/p/.ccteam/uploads/1-a.png", name: "a.png" },
      { kind: "skill", name: "deep-research" },
    ]);
    const [, init] = fetchMock.mock.calls[0]!;
    expect(JSON.parse(String(init?.body))).toEqual({
      text: "look",
      attachments: [
        { kind: "image", path: "/p/.ccteam/uploads/1-a.png", name: "a.png" },
        { kind: "skill", name: "deep-research" },
      ],
    });
    // Empty attachments stay OFF the wire (older servers see the same body).
    fetchMock.mockResolvedValueOnce(jsonResponse(202, { accepted: true }));
    await submitTurn("s2", "plain", []);
    const [, init2] = fetchMock.mock.calls[1]!;
    expect(JSON.parse(String(init2?.body))).toEqual({ text: "plain" });
  });

  it("submitTurn lifts the server human error body", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(502, {
        error: "发送失败: tmux session missing。下一步: 请重试；如果仍失败，刷新会话列表或重新 /new。",
      }),
    );
    await expect(submitTurn("s2", "review")).rejects.toThrow(
      "发送失败: tmux session missing",
    );
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

  it("interruptSession POSTs to /sessions/{sid}/interrupt (non-destructive)", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, { interrupted: true }));
    const got = await interruptSession("s3");
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/sessions/s3/interrupt", {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: JSON.stringify({}),
    });
    expect(got.interrupted).toBe(true);
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

  it("createSession carries explicit model/effort in the body (A-U3)", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(201, { sid: "s6" }));
    await createSession("dex-ui", {
      role: "",
      vendor: "codex",
      model: "gpt-5.1",
      effort: "xhigh",
    });
    const body = JSON.parse(vi.mocked(globalThis.fetch).mock.calls[0][1]!.body as string);
    expect(body).toEqual({ role: "", vendor: "codex", model: "gpt-5.1", effort: "xhigh" });
  });

  it("createSession omits optional fields when not given", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(201, { sid: "s5" }));
    await createSession("dex-ui", { role: "cto" });
    const body = JSON.parse(vi.mocked(globalThis.fetch).mock.calls[0][1]!.body as string);
    expect(body).toEqual({ role: "cto" });
  });

  it("createSession lifts the server human error body", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(500, {
        ok: false,
        error: "会话启动失败: simulated start failure。下一步: 请检查项目和角色后重试。",
      }),
    );
    await expect(createSession("dex-ui", { role: "cto" })).rejects.toThrow(
      "会话启动失败: simulated start failure",
    );
  });

  it("caps non-JSON error bodies and prefixes the HTTP status", async () => {
    const html = `<html>${"x".repeat(1000)}</html>`;
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(textResponse(500, html));
    try {
      await listSessions("dex-ui");
      throw new Error("expected listSessions to fail");
    } catch (e) {
      expect(e).toBeInstanceOf(Error);
      const message = (e as Error).message;
      expect(message).toMatch(/^HTTP 500: <html>x+/);
      expect(message.length).toBeLessThanOrEqual(210);
    }
  });

  it("keeps structured JSON error text verbatim", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(500, { error: "x" }));
    await expect(listSessions("dex-ui")).rejects.toThrow("x");
  });

  it("maps 401 → UNAUTHENTICATED and 404 → NOT_FOUND", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(401, { error: "auth" }));
    await expect(listSessions("x")).rejects.toThrow("UNAUTHENTICATED");
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(404, { error: "nope" }));
    await expect(getHistory("sX")).rejects.toThrow("NOT_FOUND");
  });
});

describe("listProjectRoles", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn();
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("GETs /api/v1/projects/{slug}/roles with same-origin creds + encoded slug", async () => {
    const roles: RoleSummary[] = [
      { role: "cto", description: "chat-first manager", model: "" },
      { role: "reviewer", description: "", model: "sonnet" },
    ];
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, roles));
    const got = await listProjectRoles("a b");
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/projects/a%20b/roles", {
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
    expect(got).toEqual(roles);
  });

  it("returns [] for a project with no agents/", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(200, []));
    expect(await listProjectRoles("empty")).toEqual([]);
  });

  it("maps 404 (unknown project) → NOT_FOUND and 401 → UNAUTHENTICATED", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(404, { error: "no project" }));
    await expect(listProjectRoles("ghost")).rejects.toThrow("NOT_FOUND");
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(401, { error: "auth" }));
    await expect(listProjectRoles("x")).rejects.toThrow("UNAUTHENTICATED");
  });
});

describe("getRoleDetail", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn();
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("GETs /api/v1/projects/{slug}/roles/{role} with encoded slug + role", async () => {
    const detail: RoleDetail = {
      role: "code-reviewer",
      frontmatter: { description: "reviews diffs", model: "sonnet" },
      body: "# Reviewer\nYou review code.",
    };
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, detail));
    const got = await getRoleDetail("a b", "code-reviewer");
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/projects/a%20b/roles/code-reviewer",
      { headers: { Accept: "application/json" }, credentials: "same-origin" },
    );
    expect(got).toEqual(detail);
    expect(got.frontmatter.model).toBe("sonnet");
  });

  it("maps 404 (unknown role) → NOT_FOUND and 401 → UNAUTHENTICATED", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(404, { error: "no role" }));
    await expect(getRoleDetail("p", "ghost")).rejects.toThrow("NOT_FOUND");
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(401, { error: "auth" }));
    await expect(getRoleDetail("p", "x")).rejects.toThrow("UNAUTHENTICATED");
  });
});

// ── v0.8.21 history / resume / external-import ───────────────────────────────
describe("sessionsApi history / resume / external-import (v0.8.21)", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn();
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("listHistorySessions GETs .../sessions/history and returns stopped rows", async () => {
    const rows: HistorySessionView[] = [
      {
        sid: "s7",
        slug: "dex-ui",
        vendor: "claude",
        protocol: "stream-json",
        role: "cto",
        permission_mode: "skip",
        owner: "user:web-api",
        vendor_uuid: "11111111-1111-1111-1111-111111111111",
        created_at: "2026-06-29T00:00:00Z",
        last_active: "2026-06-29T01:00:00Z",
        origin: "ccteam",
        transcript_present: true,
      },
    ];
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, rows));
    const got = await listHistorySessions("dex-ui");
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/projects/dex-ui/sessions/history", {
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
    expect(got).toEqual(rows);
    expect(got[0].transcript_present).toBe(true);
    expect(got[0].origin).toBe("ccteam");
  });

  it("listHistorySessions returns [] when no stopped sessions", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(200, []));
    expect(await listHistorySessions("empty")).toEqual([]);
  });

  it("resumeSession POSTs {} to .../sessions/{sid}/resume and returns {sid}", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, { sid: "s7" }));
    const got = await resumeSession("dex-ui", "s7");
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/projects/dex-ui/sessions/s7/resume", {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: JSON.stringify({}),
    });
    expect(got.sid).toBe("s7");
  });

  it("resumeSession encodes the sid", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(200, { sid: "s/odd" }));
    await resumeSession("dex-ui", "s/odd");
    expect(vi.mocked(globalThis.fetch).mock.calls[0][0]).toBe(
      "/api/v1/projects/dex-ui/sessions/s%2Fodd/resume",
    );
  });

  it("listExternalSessions GETs .../external-sessions and returns discovered rows", async () => {
    const rows: ExternalSessionView[] = [
      {
        vendor: "claude",
        vendor_uuid: "22222222-2222-2222-2222-222222222222",
        title: "refactor gateway",
        last_active: "2026-06-28T00:00:00Z",
        cwd: "/home/u/proj",
        adoptable: true,
      },
    ];
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, rows));
    const got = await listExternalSessions("dex-ui");
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/projects/dex-ui/external-sessions", {
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
    expect(got).toEqual(rows);
    expect(got[0].adoptable).toBe(true);
  });

  it("importExternalSession POSTs {vendor,vendor_uuid} to .../sessions/import", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(201, { sid: "s8" }));
    const got = await importExternalSession(
      "dex-ui",
      "33333333-3333-3333-3333-333333333333",
    );
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/projects/dex-ui/sessions/import", {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: JSON.stringify({
        vendor: "claude",
        vendor_uuid: "33333333-3333-3333-3333-333333333333",
      }),
    });
    expect(got.sid).toBe("s8");
  });

  it("importExternalSession lifts the server error body (uuid not adoptable)", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(400, {
        error: "vendor_uuid 33333333 is not an adoptable session for project dex-ui",
      }),
    );
    await expect(
      importExternalSession("dex-ui", "33333333-3333-3333-3333-333333333333"),
    ).rejects.toThrow("is not an adoptable session");
  });

  it("history/resume map 401 → UNAUTHENTICATED and 404 → NOT_FOUND", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(401, { error: "auth" }));
    await expect(listHistorySessions("x")).rejects.toThrow("UNAUTHENTICATED");
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(404, { error: "nope" }));
    await expect(resumeSession("x", "sZ")).rejects.toThrow("NOT_FOUND");
  });
});
