// v0.10.2 (WEB-DSH-1) — dshStore semantics. The store is the ONE source of
// DSH status for both consumers (DshView head + DshFrameHost iframe) and owns
// the only starting-poll; the keep-alive behavior itself is proven by the
// shell navigation test (pages/DshKeepAlive.test.tsx).

import { afterEach, describe, expect, it, vi } from "vitest";

import { createDshStore } from "./dshStore";
import type { DshStatus } from "../lib/dshApi";

const status = (over: Partial<DshStatus> = {}): DshStatus => ({
  state: "running",
  port: 35479,
  companion_port: 7332,
  home_kind: "own",
  dsh_version: "0.1.0-rc.6",
  ...over,
});

describe("dshStore", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  it("is lazy: zero fetches before the first /dsh visit", () => {
    const fetchStatus = vi.fn().mockResolvedValue(status());
    const store = createDshStore({ fetchStatus });
    const unsubscribe = store.subscribe(() => {});
    expect(store.getSnapshot().visited).toBe(false);
    expect(fetchStatus).not.toHaveBeenCalled();
    unsubscribe();
    expect(fetchStatus).not.toHaveBeenCalled();
  });

  it("visit() flips the gate and loads the status", async () => {
    const fetchStatus = vi.fn().mockResolvedValue(status());
    const store = createDshStore({ fetchStatus });
    store.visit();
    await vi.waitFor(() => expect(store.getSnapshot().status?.state).toBe("running"));
    expect(store.getSnapshot().visited).toBe(true);
    expect(store.getSnapshot().loading).toBe(false);
    expect(store.getSnapshot().fetchError).toBeNull();
    expect(fetchStatus).toHaveBeenCalledTimes(1);
  });

  it("a revisit revalidates but keeps the last status on screen", async () => {
    const fetchStatus = vi.fn().mockResolvedValue(status());
    const store = createDshStore({ fetchStatus });
    store.visit();
    await vi.waitFor(() => expect(store.getSnapshot().status).not.toBeNull());
    store.visit();
    // No loading spinner flash on revisit: the stored status stays put while
    // the background refresh runs (this is what makes the iframe src stable).
    expect(store.getSnapshot().status?.state).toBe("running");
    expect(store.getSnapshot().loading).toBe(false);
    await vi.waitFor(() => expect(fetchStatus).toHaveBeenCalledTimes(2));
  });

  it("polls only while starting, and stops once running", async () => {
    vi.useFakeTimers();
    let current = status({ state: "starting", port: null, companion_port: null });
    const fetchStatus = vi.fn(async () => current);
    const store = createDshStore({ fetchStatus, pollMs: 1000 });
    store.visit();
    await vi.advanceTimersByTimeAsync(0);
    expect(fetchStatus).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1000);
    expect(fetchStatus).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(1000);
    expect(fetchStatus).toHaveBeenCalledTimes(3);
    current = status({ state: "running" });
    await vi.advanceTimersByTimeAsync(1000);
    expect(fetchStatus).toHaveBeenCalledTimes(4);
    await vi.advanceTimersByTimeAsync(10_000);
    expect(fetchStatus).toHaveBeenCalledTimes(4);
  });

  it("runAction writes the result status for every consumer", async () => {
    vi.useFakeTimers();
    const fetchStatus = vi.fn(async () => status({ state: "stopped", companion_port: null }));
    const store = createDshStore({ fetchStatus, pollMs: 1000 });
    store.visit();
    await vi.advanceTimersByTimeAsync(0);
    expect(fetchStatus).toHaveBeenCalledTimes(1); // stopped → no poll
    await vi.advanceTimersByTimeAsync(3000);
    expect(fetchStatus).toHaveBeenCalledTimes(1);

    await store.runAction(async () => status({ state: "starting", companion_port: null }));
    expect(store.getSnapshot().status?.state).toBe("starting");
    await vi.advanceTimersByTimeAsync(1000);
    expect(fetchStatus.mock.calls.length).toBeGreaterThanOrEqual(2); // poll started
  });

  it("maps failures to fetchError but swallows UNAUTHENTICATED", async () => {
    const boom = createDshStore({
      fetchStatus: async () => {
        throw new Error("boom");
      },
    });
    boom.visit();
    await vi.waitFor(() => expect(boom.getSnapshot().fetchError).toBe("boom"));
    expect(boom.getSnapshot().loading).toBe(false);

    const unauth = createDshStore({
      fetchStatus: async () => {
        throw new Error("UNAUTHENTICATED");
      },
    });
    unauth.visit();
    await vi.waitFor(() => expect(unauth.getSnapshot().loading).toBe(false));
    expect(unauth.getSnapshot().fetchError).toBeNull();
    expect(unauth.getSnapshot().status).toBeNull();
  });
});
