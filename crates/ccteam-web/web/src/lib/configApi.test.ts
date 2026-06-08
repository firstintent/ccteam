// v0.8.8 F4 — configApi.ts unit tests.
//
// Mirrors sessionsApi.test.ts: spy on `fetch`, assert URL + method + body +
// same-origin creds + error mapping. Runs under node env (no DOM).
//
// 红线(red line) check: getImConfig's response carries ONLY masked
// fingerprints — we assert the parsed result has no `bot_token` / `app_secret`
// key (a second guard on top of the type-level guarantee).

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  getImConfig,
  pollTelegramChatId,
  saveLark,
  saveTelegramToken,
  startTelegramChatId,
} from "./configApi";

const realFetch = globalThis.fetch;

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

describe("configApi", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn();
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("getImConfig GETs /config/im with same-origin creds and returns masked status", async () => {
    const masked = {
      telegram: { configured: true, bot_token_last4: "…wxyz", chat_id_count: 1 },
      lark: {
        configured: true,
        app_id_last4: "…cli9",
        use_feishu: true,
        allowed_user_id_count: 2,
      },
      transport_warning: "no TLS",
    };
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, masked));
    const got = await getImConfig();
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/config/im", {
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
    expect(got).toEqual(masked);
    // Red line: no plaintext secret keys (only the *_last4 fingerprints).
    // `bot_token_last4` legitimately contains the substring "bot_token", so
    // we assert the exact keys are absent rather than a substring scan.
    expect(got.telegram).not.toHaveProperty("bot_token");
    expect(got.lark).not.toHaveProperty("app_secret");
    expect(Object.keys(got.telegram ?? {})).toEqual([
      "configured",
      "bot_token_last4",
      "chat_id_count",
    ]);
    expect(Object.keys(got.lark ?? {})).toEqual([
      "configured",
      "app_id_last4",
      "use_feishu",
      "allowed_user_id_count",
    ]);
  });

  it("getImConfig tolerates null provider blocks", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(200, { telegram: null, lark: null, transport_warning: "" }),
    );
    const got = await getImConfig();
    expect(got.telegram).toBeNull();
    expect(got.lark).toBeNull();
  });

  it("saveTelegramToken PUTs {bot_token} to /config/im/telegram", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(
      jsonResponse(200, {
        ok: true,
        restart_required: true,
        bot_username: "mybot",
        note: "restart",
      }),
    );
    const got = await saveTelegramToken("123:ABC");
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/config/im/telegram", {
      method: "PUT",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: JSON.stringify({ bot_token: "123:ABC" }),
    });
    expect(got.bot_username).toBe("mybot");
    expect(got.restart_required).toBe(true);
  });

  it("saveTelegramToken surfaces the server {error} text on 400 (not 'HTTP 400')", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(400, { error: "Telegram token rejected: 401 Unauthorized" }),
    );
    await expect(saveTelegramToken("bad")).rejects.toThrow(
      "Telegram token rejected: 401 Unauthorized",
    );
  });

  it("startTelegramChatId POSTs {} to /config/im/telegram/chat-id/start", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(
      jsonResponse(200, { started: true, poll_seconds: 90 }),
    );
    const got = await startTelegramChatId();
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/config/im/telegram/chat-id/start",
      {
        method: "POST",
        credentials: "same-origin",
        headers: { "Content-Type": "application/json", Accept: "application/json" },
        body: JSON.stringify({}),
      },
    );
    expect(got.started).toBe(true);
    expect(got.poll_seconds).toBe(90);
  });

  it("startTelegramChatId surfaces the server {error} on 400 (no token yet)", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(400, { error: "no Telegram token configured" }),
    );
    await expect(startTelegramChatId()).rejects.toThrow(
      "no Telegram token configured",
    );
  });

  it("pollTelegramChatId GETs /config/im/telegram/chat-id and returns the poll state", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(
      jsonResponse(200, {
        status: "captured",
        chat_id_last4: "…7890",
        restart_required: true,
        note: "restart",
      }),
    );
    const got = await pollTelegramChatId();
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/config/im/telegram/chat-id", {
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
    expect(got.status).toBe("captured");
    expect(got.chat_id_last4).toBe("…7890");
    // Red line: poll result carries no raw chat_id.
    expect(got).not.toHaveProperty("chat_id");
  });

  it("pollTelegramChatId reports pending/timeout/error transparently", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, { status: "pending" }));
    expect((await pollTelegramChatId()).status).toBe("pending");
    fetchMock.mockResolvedValueOnce(jsonResponse(200, { status: "timeout" }));
    expect((await pollTelegramChatId()).status).toBe("timeout");
    fetchMock.mockResolvedValueOnce(
      jsonResponse(200, { status: "error", error: "boom" }),
    );
    const errd = await pollTelegramChatId();
    expect(errd.status).toBe("error");
    expect(errd.error).toBe("boom");
  });

  it("saveLark PUTs the full body to /config/im/lark", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(
      jsonResponse(200, { ok: true, restart_required: true, note: "restart" }),
    );
    const got = await saveLark({
      app_id: "cli_x",
      app_secret: "sec",
      allowed_user_ids: ["ou_a", "ou_b"],
      use_feishu: false,
    });
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/config/im/lark", {
      method: "PUT",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: JSON.stringify({
        app_id: "cli_x",
        app_secret: "sec",
        allowed_user_ids: ["ou_a", "ou_b"],
        use_feishu: false,
      }),
    });
    expect(got.ok).toBe(true);
  });

  it("saveLark surfaces the server {error} on 400 (rejected creds)", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(400, { error: "Lark credentials rejected: bad app_secret" }),
    );
    await expect(
      saveLark({
        app_id: "cli_x",
        app_secret: "bad",
        allowed_user_ids: [],
        use_feishu: true,
      }),
    ).rejects.toThrow("Lark credentials rejected: bad app_secret");
  });

  it("maps 401 → UNAUTHENTICATED across read + write helpers", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(401, { error: "auth" }));
    await expect(getImConfig()).rejects.toThrow("UNAUTHENTICATED");
    fetchMock.mockResolvedValueOnce(jsonResponse(401, { error: "auth" }));
    await expect(saveTelegramToken("x")).rejects.toThrow("UNAUTHENTICATED");
    fetchMock.mockResolvedValueOnce(jsonResponse(401, { error: "auth" }));
    await expect(
      saveLark({ app_id: "a", app_secret: "b", allowed_user_ids: [], use_feishu: true }),
    ).rejects.toThrow("UNAUTHENTICATED");
  });

  it("falls back to 'HTTP <status>' when the error body has no {error}", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(500, {}));
    await expect(getImConfig()).rejects.toThrow("HTTP 500");
  });
});
