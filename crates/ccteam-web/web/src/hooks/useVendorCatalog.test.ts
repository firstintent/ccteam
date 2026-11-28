import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { SharedResourceEnvironment } from "./createSharedResourceStore";
import { createVendorCatalogStore } from "./useVendorCatalog";
import type { ModelsResponse, VendorCatalog } from "../lib/modelsApi";

const realFetch = globalThis.fetch;

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function fakeEnvironment(initiallyHidden = false): SharedResourceEnvironment & {
  pendingTimers(): number;
  fireNext(): void;
  focus(): void;
  setHidden(hidden: boolean): void;
  visibilityChange(): void;
} {
  let hidden = initiallyHidden;
  let nextId = 1;
  let timers: Array<{ id: number; callback: () => void; delay: number }> = [];
  let focusListener: (() => void) | null = null;
  let visibilityListener: (() => void) | null = null;
  return {
    setTimer(callback, delay) {
      const id = nextId++;
      timers.push({ id, callback, delay });
      return id as unknown as ReturnType<typeof setTimeout>;
    },
    clearTimer(timer) {
      timers = timers.filter((entry) => entry.id !== (timer as unknown as number));
    },
    createAbortController: () => new AbortController(),
    isHidden: () => hidden,
    random: () => 0.5,
    onFocus(callback) {
      focusListener = callback;
      return () => {
        if (focusListener === callback) focusListener = null;
      };
    },
    onVisibilityChange(callback) {
      visibilityListener = callback;
      return () => {
        if (visibilityListener === callback) visibilityListener = null;
      };
    },
    pendingTimers: () => timers.length,
    fireNext() {
      const next = timers.shift();
      next?.callback();
    },
    focus: () => focusListener?.(),
    setHidden: (next) => {
      hidden = next;
    },
    visibilityChange: () => visibilityListener?.(),
  };
}

async function flushPromises(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
}

describe("createVendorCatalogStore", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn();
  });

  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("coalesces concurrent subscribers into one in-flight request", () => {
    const env = fakeEnvironment();
    const request = deferred<VendorCatalog>();
    const fetcher = vi.fn(() => request.promise);
    const store = createVendorCatalogStore({ fetcher, env });

    const unsubs = [store.subscribe(() => {}), store.subscribe(() => {}), store.subscribe(() => {})];
    store.refresh();
    env.focus();

    expect(fetcher).toHaveBeenCalledTimes(1);
    for (const unsub of unsubs) unsub();
  });

  it("refreshes newly observed live-only vendor models on focus and explicit refresh", async () => {
    const env = fakeEnvironment();
    const cold: ModelsResponse = {
      vendors: [{ vendor: "agy", observed_at: null, source: null, models: [], efforts: ["low", "medium", "high"] }],
    };
    const observed: ModelsResponse = {
      vendors: [
        {
          vendor: "agy",
          observed_at: "2026-11-28T07:26:31Z",
          source: "ACP session availableModels",
          models: [
            { id: "gemini-4-neon", display_name: "Gemini 4 Neon", efforts: ["low", "medium", "high", "xhigh"] },
            { id: "claude-opus-5-5", display_name: "Claude Opus 5.5", efforts: ["low", "medium", "high", "xhigh"] },
          ],
          default_model: "gemini-4-neon",
          efforts: ["low", "medium", "high", "xhigh"],
        },
      ],
    };
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock
      .mockResolvedValueOnce(jsonResponse(200, cold))
      .mockResolvedValueOnce(jsonResponse(200, observed))
      .mockResolvedValueOnce(jsonResponse(200, observed));

    const store = createVendorCatalogStore({ env });
    const listener = vi.fn();
    const unsub = store.subscribe(listener);
    await flushPromises();

    expect(store.getSnapshot().data?.agy?.models).toEqual([]);
    expect(env.pendingTimers()).toBe(0);

    // Returning from IM or another tab triggers a focus refresh and picks up
    // the models recorded by the first Antigravity session handshake.
    env.focus();
    await flushPromises();
    expect(store.getSnapshot().data?.agy?.models.map((m) => m.id)).toEqual([
      "gemini-4-neon",
      "claude-opus-5-5",
    ]);
    expect(store.getSnapshot().data?.agy?.defaultModel).toBe("gemini-4-neon");

    // Explicit refresh (menu open / vendor pick / post-spawn) hits the server again.
    store.refresh();
    await flushPromises();
    expect(fetchMock).toHaveBeenCalledTimes(3);
    unsub();
  });

  it("degrades 404 (older daemon) to an empty catalog without spinning retry timers", async () => {
    const env = fakeEnvironment();
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(404, {}));

    const store = createVendorCatalogStore({ env });
    const unsub = store.subscribe(() => {});
    await flushPromises();

    expect(store.getSnapshot().data).toEqual({});
    expect(store.getSnapshot().error).toBeNull();
    expect(env.pendingTimers()).toBe(0);
    unsub();
  });

  it("retries transient failures while preserving the last known good catalog", async () => {
    const env = fakeEnvironment();
    const first: ModelsResponse = {
      vendors: [
        {
          vendor: "agy",
          observed_at: "2026-11-28T07:26:31Z",
          source: "ACP session availableModels",
          models: [{ id: "gemini-4-neon" }],
          efforts: ["low", "high"],
        },
      ],
    };
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock
      .mockResolvedValueOnce(jsonResponse(200, first))
      .mockResolvedValueOnce(jsonResponse(503, { error: "restarting" }))
      .mockResolvedValueOnce(jsonResponse(200, first));

    const store = createVendorCatalogStore({ env });
    const unsub = store.subscribe(() => {});
    await flushPromises();
    expect(store.getSnapshot().data?.agy?.models.map((m) => m.id)).toEqual(["gemini-4-neon"]);

    store.refresh();
    await flushPromises();
    // Last good data survives the transient 503 and a backoff retry is scheduled.
    expect(store.getSnapshot().data?.agy?.models.map((m) => m.id)).toEqual(["gemini-4-neon"]);
    expect(env.pendingTimers()).toBe(1);

    env.fireNext();
    await flushPromises();
    expect(fetchMock).toHaveBeenCalledTimes(3);
    expect(env.pendingTimers()).toBe(0);
    unsub();
  });
});
