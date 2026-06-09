// v0.8.7 W4 (DD.1) — sessionsApi.ts unit tests.
//
// Mirrors the listApi/dashboardApi pattern: spy on `fetch`, assert URL +
// method + body shape + error mapping. Runs under node env (no DOM).

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  createSession,
  getHistory,
  getRoleDetail,
  listProjectRoles,
  listSessions,
  resolveApproval,
  sessionUrl,
  sessionsUrl,
  stopSession,
  submitTurn,
  type RoleDetail,
  type RoleSummary,
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
    fetchMock.mockResolvedValueOnce(
      jsonResponse(201, { sid: "s4", model_warning: "模型提示: deepseek" }),
    );
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
    expect(got.model_warning).toContain("deepseek");
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
