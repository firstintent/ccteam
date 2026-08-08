// modelsApi.ts unit tests (fetch-spy, node env) — the live vendor catalog
// behind the composer's model + effort menus.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { fetchModels, indexCatalog, MODELS_URL, type ModelsResponse } from "./modelsApi";

const realFetch = globalThis.fetch;

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

/** The shape the daemon reports (one entry per vendor). */
const RESPONSE: ModelsResponse = {
  vendors: [
    {
      vendor: "kimi",
      observed_at: "2026-08-01T11:33:50Z",
      source: "ACP session availableModels",
      models: [
        { id: "kimi-code/k3", display_name: "K3", efforts: ["low", "high", "max"] },
        { id: "kimi-code/k3-256k", display_name: "K3 256K" },
      ],
      efforts: ["low", "high", "max"],
    },
    { vendor: "opencode", observed_at: null, source: null, models: [], efforts: [] },
  ],
};

describe("fetchModels", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn();
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("GETs /api/v1/models with same-origin creds", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, RESPONSE));
    const got = await fetchModels();
    expect(fetchMock).toHaveBeenCalledWith(MODELS_URL, {
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
    expect(MODELS_URL).toBe("/api/v1/models");
    expect(got.vendors[0]!.models[0]!.display_name).toBe("K3");
    expect(got.vendors[0]!.efforts).toEqual(["low", "high", "max"]);
  });

  it("maps 401 → UNAUTHENTICATED (the global token gate handles it)", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(401, {}));
    await expect(fetchModels()).rejects.toThrow("UNAUTHENTICATED");
  });

  it("maps a 404 (daemon older than this SPA) to HTTP 404", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(404, {}));
    await expect(fetchModels()).rejects.toThrow("HTTP 404");
  });

  it("surfaces the server's {error} body on a non-2xx", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(500, { error: "model probe failed" }),
    );
    await expect(fetchModels()).rejects.toThrow("model probe failed");
  });
});

describe("indexCatalog", () => {
  it("keys the vendors by id, preserving each vendor's own ordering", () => {
    const catalog = indexCatalog(RESPONSE);
    expect(Object.keys(catalog).sort()).toEqual(["kimi", "opencode"]);
    expect(catalog.kimi).toEqual({
      models: [
        { id: "kimi-code/k3", display_name: "K3", efforts: ["low", "high", "max"] },
        { id: "kimi-code/k3-256k", display_name: "K3 256K" },
      ],
      efforts: ["low", "high", "max"],
    });
  });

  it("keeps a never-observed vendor as an empty entry (the caller falls back to static)", () => {
    expect(indexCatalog(RESPONSE).opencode).toEqual({ models: [], efforts: [] });
  });

  it("is total: no response / no vendors / junk entries yield an empty-but-usable catalog", () => {
    expect(indexCatalog(null)).toEqual({});
    expect(indexCatalog(undefined)).toEqual({});
    expect(indexCatalog({} as ModelsResponse)).toEqual({});
    expect(
      indexCatalog({
        vendors: [
          { vendor: "  ", models: [], efforts: [] },
          { vendor: "grok" } as unknown as ModelsResponse["vendors"][number],
        ],
      }),
    ).toEqual({ grok: { models: [], efforts: [] } });
  });

  it("drops blank / duplicate / non-string tokens rather than showing them in a menu", () => {
    const catalog = indexCatalog({
      vendors: [
        {
          vendor: "grok",
          models: [{ id: "grok-4.5" }, { id: " grok-4.5 " }, { id: "" }, { id: 7 as never }],
          efforts: ["low", "low", " high ", "", null as never],
        },
      ],
    });
    expect(catalog.grok).toEqual({ models: [{ id: "grok-4.5" }], efforts: ["low", "high"] });
  });
});
