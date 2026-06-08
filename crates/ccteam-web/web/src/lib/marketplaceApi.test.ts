// v0.8.9 Phase 4 — marketplaceApi.ts unit tests.
//
// Mirrors sessionsApi.test.ts: spy on `fetch`, assert URL + method + body
// shape + error mapping (incl. the lifted 409 already-installed envelope).
// Runs under node env (no DOM).

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  getMarketplace,
  getPluginBody,
  getProjectMarketplace,
  installPlugin,
  marketplaceBodyUrl,
  marketplaceUrl,
  projectInstallUrl,
  projectMarketplaceUrl,
  type DecoratedIndex,
  type HubIndex,
} from "./marketplaceApi";

const realFetch = globalThis.fetch;

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

const SAMPLE_PLUGIN = {
  id: "code-reviewer",
  type: "agent" as const,
  name: "Code Reviewer",
  description: "line-by-line review",
  path: "agents/code-reviewer.md",
  content_sha: "abc",
  source: "agency-agents",
  upstream: "https://github.com/x/code-reviewer.md",
  license: "MIT",
  tags: ["review", "security"],
};

describe("marketplaceApi url builders", () => {
  it("builds the global catalog + refresh URLs", () => {
    expect(marketplaceUrl()).toBe("/api/v1/marketplace");
    expect(marketplaceUrl(true)).toBe("/api/v1/marketplace?refresh=true");
  });

  it("builds + encodes the body URL", () => {
    expect(marketplaceBodyUrl("code-reviewer")).toBe(
      "/api/v1/marketplace/code-reviewer/body",
    );
    expect(marketplaceBodyUrl("a b")).toBe("/api/v1/marketplace/a%20b/body");
  });

  it("builds + encodes the per-project catalog + install URLs", () => {
    expect(projectMarketplaceUrl("dex-ui")).toBe("/api/v1/projects/dex-ui/marketplace");
    expect(projectMarketplaceUrl("dex-ui", true)).toBe(
      "/api/v1/projects/dex-ui/marketplace?refresh=true",
    );
    expect(projectMarketplaceUrl("a b")).toBe("/api/v1/projects/a%20b/marketplace");
    expect(projectInstallUrl("dex-ui")).toBe("/api/v1/projects/dex-ui/marketplace/install");
  });
});

describe("getMarketplace", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn();
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("GETs the global catalog with same-origin creds", async () => {
    const index: HubIndex = {
      version: 1,
      name: "ccteam-hub",
      description: "curated",
      generated_at: "2026-06-07T00:00:00Z",
      plugins: [SAMPLE_PLUGIN],
    };
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, index));
    const got = await getMarketplace();
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/marketplace", {
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
    expect(got.plugins[0].id).toBe("code-reviewer");
  });

  it("passes ?refresh=true when asked", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(
      jsonResponse(200, { version: 1, name: "", description: "", generated_at: "", plugins: [] }),
    );
    await getMarketplace(true);
    expect(fetchMock.mock.calls[0][0]).toBe("/api/v1/marketplace?refresh=true");
  });

  it("lifts a 502 upstream error envelope to its message", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(502, { error: "hub fetch failed: timeout" }),
    );
    await expect(getMarketplace()).rejects.toThrow("hub fetch failed: timeout");
  });
});

describe("getProjectMarketplace", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn();
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("GETs the decorated per-project catalog (installed_status present)", async () => {
    const decorated: DecoratedIndex = {
      version: 1,
      name: "",
      description: "",
      generated_at: "",
      plugins: [{ ...SAMPLE_PLUGIN, installed_status: "installed" }],
    };
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, decorated));
    const got = await getProjectMarketplace("dex-ui");
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/projects/dex-ui/marketplace", {
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
    expect(got.plugins[0].installed_status).toBe("installed");
  });

  it("maps 404 (unknown project) → NOT_FOUND and 401 → UNAUTHENTICATED", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(404, { error: "no project" }));
    await expect(getProjectMarketplace("ghost")).rejects.toThrow("NOT_FOUND");
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(401, { error: "auth" }));
    await expect(getProjectMarketplace("x")).rejects.toThrow("UNAUTHENTICATED");
  });
});

describe("getPluginBody", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn();
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("GETs /marketplace/{id}/body and returns {id, body}", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(
      jsonResponse(200, { id: "code-reviewer", body: "# Reviewer\nyou review" }),
    );
    const got = await getPluginBody("code-reviewer");
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/marketplace/code-reviewer/body", {
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
    expect(got.body).toContain("Reviewer");
  });

  it("maps an unknown id (404) → NOT_FOUND", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(404, { error: "no plugin" }));
    await expect(getPluginBody("ghost")).rejects.toThrow("NOT_FOUND");
  });
});

describe("installPlugin", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn();
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("POSTs {id} to the project install URL (no force by default)", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(
      jsonResponse(201, {
        id: "code-reviewer",
        type: "agent",
        path: "/p/.claude/agents/code-reviewer.md",
        overwrote: false,
      }),
    );
    const got = await installPlugin("dex-ui", "code-reviewer");
    expect(fetchMock).toHaveBeenCalledWith("/api/v1/projects/dex-ui/marketplace/install", {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: JSON.stringify({ id: "code-reviewer" }),
    });
    expect(got.overwrote).toBe(false);
    // No force key unless asked.
    const sent = JSON.parse(fetchMock.mock.calls[0][1]!.body as string);
    expect(sent).toEqual({ id: "code-reviewer" });
  });

  it("includes force=true when overwriting (update)", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(
      jsonResponse(201, { id: "x", type: "agent", path: "/p", overwrote: true }),
    );
    await installPlugin("dex-ui", "x", true);
    const sent = JSON.parse(fetchMock.mock.calls[0][1]!.body as string);
    expect(sent).toEqual({ id: "x", force: true });
  });

  it("lifts the 409 already-installed envelope to its message (not bare HTTP 409)", async () => {
    // The critical case: a 409 must surface the human message naming the
    // target path, like dashboardApi.createProject lifts "already exists".
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(409, {
        ok: false,
        error: "already installed at .claude/agents/code-reviewer.md (retry with force)",
      }),
    );
    await expect(installPlugin("dex-ui", "code-reviewer")).rejects.toThrow(
      "already installed at .claude/agents/code-reviewer.md (retry with force)",
    );
  });

  it("maps 401 → UNAUTHENTICATED and 404 → NOT_FOUND, lifts 400/500/502", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(401, { error: "auth" }));
    await expect(installPlugin("p", "x")).rejects.toThrow("UNAUTHENTICATED");
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(404, { error: "no project" }));
    await expect(installPlugin("p", "x")).rejects.toThrow("NOT_FOUND");
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(400, { ok: false, error: "unsupported plugin type: workflow" }),
    );
    await expect(installPlugin("p", "x")).rejects.toThrow("unsupported plugin type: workflow");
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(500, { ok: false, error: "local write failed" }),
    );
    await expect(installPlugin("p", "x")).rejects.toThrow("local write failed");
  });

  it("falls back to HTTP <status> when the error body is unparseable", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      new Response("not json", { status: 502 }),
    );
    await expect(installPlugin("p", "x")).rejects.toThrow("HTTP 502");
  });
});
