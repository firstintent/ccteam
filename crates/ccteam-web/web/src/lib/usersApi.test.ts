// v0.8.18 档1 — usersApi.ts unit tests (fetch-spy, node env).

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  createUser,
  deleteUser,
  getMyLarkOpenIdCandidates,
  listUsers,
  putMyIm,
  putMyLarkAllowedUsers,
} from "./usersApi";

const realFetch = globalThis.fetch;

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

describe("usersApi", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn();
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("listUsers GETs /api/v1/users with same-origin creds", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(
      jsonResponse(200, [
        { id: "u1", handle: "alice", linked_chat: null, created_at: "2026-06-22T00:00:00Z" },
      ]),
    );
    const got = await listUsers();
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/users", {
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
    expect(got[0].handle).toBe("alice");
  });

  it("createUser POSTs /api/v1/users with a JSON {handle} body", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(
      jsonResponse(201, {
        tenant: { id: "u2", handle: "bob", linked_chat: null, created_at: "x" },
        personal_link: "/?token=ccteam:deadbeef",
      }),
    );
    const got = await createUser("bob");
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/users",
      expect.objectContaining({
        method: "POST",
        credentials: "same-origin",
        body: JSON.stringify({ handle: "bob" }),
      }),
    );
    expect(got.personal_link).toContain("ccteam:");
    expect(got.tenant.handle).toBe("bob");
  });

  it("deleteUser DELETEs /api/v1/users/{id}", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, { removed: true }));
    const got = await deleteUser("u2");
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/users/u2",
      expect.objectContaining({ method: "DELETE", credentials: "same-origin" }),
    );
    expect(got.removed).toBe(true);
  });

  it("putMyIm sends tenant Lark allowed_user_ids", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, { ok: true }));
    await putMyIm({
      lark: {
        app_id: "cli_a",
        app_secret: "sek",
        allowed_user_ids: ["ou_me"],
        use_feishu: true,
      },
    });
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/me/im",
      expect.objectContaining({
        method: "PUT",
        credentials: "same-origin",
        body: JSON.stringify({
          lark: {
            app_id: "cli_a",
            app_secret: "sek",
            allowed_user_ids: ["ou_me"],
            use_feishu: true,
          },
        }),
      }),
    );
  });

  it("polls and saves tenant Lark open_id candidates", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock
      .mockResolvedValueOnce(
        jsonResponse(200, {
          candidates: [
            {
              open_id: "ou_me",
              seen_at: 2000,
              message_id: "om_1",
              chat_id_last4: "room",
            },
          ],
        }),
      )
      .mockResolvedValueOnce(jsonResponse(200, { ok: true, allowed_user_id_count: 1 }));
    const got = await getMyLarkOpenIdCandidates(1500);
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/v1/me/im/lark/open-id-candidates?since=1500",
      expect.objectContaining({ credentials: "same-origin" }),
    );
    expect(got.candidates[0].open_id).toBe("ou_me");
    await putMyLarkAllowedUsers(["ou_me"]);
    expect(fetchMock).toHaveBeenLastCalledWith(
      "/api/v1/me/im/lark/allowed-users",
      expect.objectContaining({
        method: "PUT",
        body: JSON.stringify({ allowed_user_ids: ["ou_me"] }),
      }),
    );
  });

  it("maps 401 → UNAUTHENTICATED, 403 → FORBIDDEN, 500 → HTTP 500", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(401, {}));
    await expect(listUsers()).rejects.toThrow("UNAUTHENTICATED");
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(403, {}));
    await expect(listUsers()).rejects.toThrow("FORBIDDEN");
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(500, {}));
    await expect(listUsers()).rejects.toThrow("HTTP 500");
  });
});
