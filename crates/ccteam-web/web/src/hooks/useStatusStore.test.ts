import { describe, expect, it, vi } from "vitest";

import type { StatusSnapshot } from "../lib/statusApi";
import type { SharedResourceEnvironment } from "./createSharedResourceStore";
import { retryDelayMs } from "../lib/retryBackoff";
import { createStatusStore, STATUS_POLL_MS } from "./useStatusStore";

const STATUS: StatusSnapshot = {
  daemon_healthy: true,
  sessions_live: 1,
  sessions_idle: 0,
  cost_24h_usd: 0,
  cost_24h_by_vendor: {},
  budget_cap_24h: null,
};

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
  delays(): number[];
  fireNext(): void;
  focus(): void;
  setHidden(hidden: boolean): void;
  visibilityChange(): void;
} {
  let hidden = initiallyHidden;
  let nextId = 1;
  let timers: Array<{ id: number; callback: () => void; delay: number }> = [];
  const scheduledDelays: number[] = [];
  let focusListener: (() => void) | null = null;
  let visibilityListener: (() => void) | null = null;
  return {
    setTimer(callback, delay) {
      const id = nextId++;
      timers.push({ id, callback, delay });
      scheduledDelays.push(delay);
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
    delays: () => scheduledDelays,
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
  await Promise.resolve();
  await Promise.resolve();
}

describe("status store request convergence", () => {
  it("coalesces N subscribers into exactly one in-flight request", () => {
    const env = fakeEnvironment();
    const request = deferred<StatusSnapshot>();
    const fetcher = vi.fn(() => request.promise);
    const store = createStatusStore({ fetcher, env });

    const unsubscribers = Array.from({ length: 8 }, () => store.subscribe(() => {}));
    env.focus();

    expect(fetcher).toHaveBeenCalledTimes(1);
    expect(store.debug()).toMatchObject({ subscribers: 8, inFlight: true });
    for (const unsubscribe of unsubscribers) unsubscribe();
  });

  it("does not schedule an interval until a slow response completes", async () => {
    const env = fakeEnvironment();
    const request = deferred<StatusSnapshot>();
    const fetcher = vi.fn(() => request.promise);
    const store = createStatusStore({ fetcher, env });
    const unsubscribe = store.subscribe(() => {});

    expect(env.pendingTimers()).toBe(0);
    env.fireNext();
    env.focus();
    expect(fetcher).toHaveBeenCalledTimes(1);

    request.resolve(STATUS);
    await flushPromises();
    expect(env.pendingTimers()).toBe(1);
    expect(env.delays().at(-1)).toBe(STATUS_POLL_MS);
    unsubscribe();
  });

  it("stops timers and aborts the chain after the final unsubscribe", async () => {
    const env = fakeEnvironment();
    const fetcher = vi.fn().mockResolvedValue(STATUS);
    const store = createStatusStore({ fetcher, env });
    const first = store.subscribe(() => {});
    const second = store.subscribe(() => {});
    await flushPromises();

    first();
    expect(env.pendingTimers()).toBe(1);
    second();
    expect(env.pendingTimers()).toBe(0);
    env.fireNext();
    expect(fetcher).toHaveBeenCalledTimes(1);
  });
});

describe("status store retry backoff", () => {
  it("grows on consecutive failures and resets after a success", async () => {
    const env = fakeEnvironment();
    const fetcher = vi
      .fn<(signal: AbortSignal) => Promise<StatusSnapshot>>()
      .mockRejectedValueOnce(new Error("502"))
      .mockRejectedValueOnce(new Error("503"))
      .mockResolvedValueOnce(STATUS)
      .mockRejectedValueOnce(new Error("network"));
    const store = createStatusStore({ fetcher, env });
    const unsubscribe = store.subscribe(() => {});

    await flushPromises();
    expect(env.delays()).toEqual([2_000]);
    env.fireNext();
    await flushPromises();
    expect(env.delays()).toEqual([2_000, 4_000]);
    env.fireNext();
    await flushPromises();
    expect(env.delays()).toEqual([2_000, 4_000, STATUS_POLL_MS]);
    env.fireNext();
    await flushPromises();
    expect(env.delays()).toEqual([2_000, 4_000, STATUS_POLL_MS, 2_000]);
    unsubscribe();
  });

  it("keeps jitter inside ±25% and never exceeds the five-minute cap", () => {
    expect(retryDelayMs(1, 0)).toBe(1_500);
    expect(retryDelayMs(1, 1)).toBe(2_500);
    expect(retryDelayMs(2, 0)).toBe(3_000);
    expect(retryDelayMs(2, 1)).toBe(5_000);
    expect(retryDelayMs(99, 1)).toBe(300_000);
  });
});

describe("status store visibility", () => {
  it("pauses while hidden and resumes with one immediate refresh", async () => {
    const env = fakeEnvironment(true);
    const fetcher = vi.fn().mockResolvedValue(STATUS);
    const store = createStatusStore({ fetcher, env });
    const unsubscribe = store.subscribe(() => {});

    expect(fetcher).not.toHaveBeenCalled();
    env.setHidden(false);
    env.visibilityChange();
    expect(fetcher).toHaveBeenCalledTimes(1);
    await flushPromises();
    expect(env.pendingTimers()).toBe(1);

    env.setHidden(true);
    env.visibilityChange();
    expect(env.pendingTimers()).toBe(0);
    env.fireNext();
    expect(fetcher).toHaveBeenCalledTimes(1);

    env.setHidden(false);
    env.visibilityChange();
    env.focus();
    expect(fetcher).toHaveBeenCalledTimes(2);
    unsubscribe();
  });
});
