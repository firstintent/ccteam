import { describe, expect, it, vi, beforeEach } from "vitest";

import {
  listEnrollments,
  mintProjectEnrollment,
  mintUserEnrollment,
  orderSnippets,
  revokeEnrollment,
  type EnrollSnippet,
} from "./enrollApi";

function snippet(vendor: string): EnrollSnippet {
  return { vendor, format: "json", path: `~/.${vendor}`, body: "{}" };
}

function jsonResponse(body: unknown, status = 200): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    json: () => Promise.resolve(body),
    text: () => Promise.resolve(JSON.stringify(body)),
    headers: new Map(),
  } as unknown as Response;
}

describe("enrollApi", () => {
  beforeEach(() => {
    vi.restoreAllMocks();
  });

  it("groups the copy buttons by config family, unknown vendors last", () => {
    const ordered = orderSnippets([
      snippet("opencode"),
      snippet("codex"),
      snippet("mystery"),
      snippet("claude"),
      snippet("grok"),
      snippet("kimi"),
    ]).map((s) => s.vendor);
    expect(ordered).toEqual(["claude", "kimi", "codex", "grok", "opencode", "mystery"]);
  });

  // The scope lives in the ROUTE, never in the body: a project-scoped mint is
  // addressed as `/api/v1/projects/<slug>/enroll` so the daemon's single
  // project-ACL choke point gates it by path shape.
  it("addresses a project-scoped mint by path, with no project field in the body", async () => {
    const minted = {
      credential: {
        id: "abc123",
        scope: "project",
        project: "alpha",
        owner: "user:web-api",
        created_at: "2026-01-01T00:00:00Z",
        bearer_prefix: "ccteam-enroll:abc123:",
      },
      bearer: "ccteam-enroll:abc123:sekrit",
      url: "http://box.example:7331/mcp",
      snippets: [snippet("claude")],
      insecure_transport: true,
    };
    const fetchMock = vi.fn().mockResolvedValue(jsonResponse(minted));
    globalThis.fetch = fetchMock as unknown as typeof fetch;

    const res = await mintProjectEnrollment("alpha", { label: "laptop" });
    expect(res.bearer).toBe("ccteam-enroll:abc123:sekrit");
    expect(res.insecure_transport).toBe(true);
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/v1/projects/alpha/enroll");
    expect(init.method).toBe("POST");
    expect(JSON.parse(init.body as string)).toEqual({ label: "laptop" });

    // A slug with URL-significant characters cannot break out of the path.
    await mintProjectEnrollment("a/b", {});
    expect((fetchMock.mock.calls[1] as [string])[0]).toBe("/api/v1/projects/a%2Fb/enroll");
  });

  it("mints a machine-user credential on the flat route, project-free", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      jsonResponse({
        credential: {
          id: "def456",
          scope: "user",
          owner: "user:web-api",
          created_at: "2026-01-01T00:00:00Z",
          bearer_prefix: "ccteam-enroll:def456:",
        },
        bearer: "ccteam-enroll:def456:sekrit",
        url: "http://box.example:7331/mcp",
        snippets: [],
        insecure_transport: true,
      }),
    );
    globalThis.fetch = fetchMock as unknown as typeof fetch;

    // The label is not decoration on this route: the unlabelled machine-user
    // slot is the daemon's own credential, so every mint here names its slot.
    const res = await mintUserEnrollment({ label: "ci runner" });
    expect(res.credential.scope).toBe("user");
    const [url, init] = fetchMock.mock.calls[0] as [string, RequestInit];
    expect(url).toBe("/api/v1/enroll");
    expect(JSON.parse(init.body as string)).toEqual({ label: "ci runner" });
  });

  it("lists credentials and tolerates a body with no credentials key", async () => {
    globalThis.fetch = vi
      .fn()
      .mockResolvedValue(jsonResponse({})) as unknown as typeof fetch;
    expect(await listEnrollments()).toEqual([]);
  });

  it("throws UNAUTHENTICATED on 401 so the global token gate takes over", async () => {
    globalThis.fetch = vi
      .fn()
      .mockResolvedValue(jsonResponse({ error: "auth required" }, 401)) as unknown as typeof fetch;
    await expect(listEnrollments()).rejects.toThrow("UNAUTHENTICATED");
  });

  it("surfaces the server's own reason on a failed revoke", async () => {
    globalThis.fetch = vi.fn().mockResolvedValue(
      jsonResponse({ error: "no such enrollment credential: zz" }, 404),
    ) as unknown as typeof fetch;
    await expect(revokeEnrollment("zz")).rejects.toThrow(
      "HTTP 404: no such enrollment credential: zz",
    );
  });
});
