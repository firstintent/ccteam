// 2026-08-02 — the pile-up invariants of the snapshot poller.
//
// These are regression tests for a REAL outage: `AgentsView` polled
// `/api/v1/agents/graph` with `setInterval(load, 15s)`, so on a host with a
// slow route to the daemon every tick added another in-flight request. The
// browser's ~6-connections-per-origin budget (two already held by SSE streams)
// filled with stuck graph requests and every other request on the page —
// `/me`, `/projects`, even `logo.svg` — sat pending forever.
//
// The engine is tested directly (no React, no DOM, no real timers): a fake
// timer env makes "the interval fired again while a request was open" an
// explicit, deterministic step. Under the old `setInterval` shape
// `pending-requests-observed` would be 4 in the first test below; the whole
// point is that it is 1.

import { describe, expect, it, vi } from "vitest";

import { createSnapshotPoller, nextPollDelayMs } from "./usePolledSnapshot";
import type { SnapshotPollerEnvironment } from "./usePolledSnapshot";

/** A timer env whose queue is drained manually, so "time passes" is a step. */
function fakeEnv(): SnapshotPollerEnvironment & {
  fire(): void;
  pending(): number;
  aborted(): number;
} {
  let queue: Array<{ id: number; cb: () => void }> = [];
  let nextId = 1;
  let abortedCount = 0;
  return {
    setTimer(cb) {
      const id = nextId++;
      queue.push({ id, cb });
      return id as unknown as ReturnType<typeof setTimeout>;
    },
    clearTimer(timer) {
      queue = queue.filter((entry) => entry.id !== (timer as unknown as number));
    },
    createAbortController() {
      const controller = new AbortController();
      controller.signal.addEventListener("abort", () => {
        abortedCount += 1;
      });
      return controller;
    },
    fire() {
      const due = queue;
      queue = [];
      for (const entry of due) entry.cb();
    },
    pending: () => queue.length,
    aborted: () => abortedCount,
  };
}

/** A fetcher that never settles, counting how many calls are open at once. */
function hangingFetcher(): {
  fetcher: () => Promise<never>;
  calls: () => number;
} {
  let calls = 0;
  return {
    fetcher: () => {
      calls += 1;
      return new Promise<never>(() => {});
    },
    calls: () => calls,
  };
}

describe("nextPollDelayMs", () => {
  it("uses the plain interval while healthy", () => {
    expect(nextPollDelayMs(15_000, 0)).toBe(15_000);
  });

  it("backs off exponentially per consecutive failure", () => {
    expect(nextPollDelayMs(15_000, 1)).toBe(30_000);
    expect(nextPollDelayMs(15_000, 2)).toBe(60_000);
    expect(nextPollDelayMs(15_000, 3)).toBe(120_000);
  });

  it("caps the backed-off delay so a dead endpoint is never polled forever apart", () => {
    expect(nextPollDelayMs(15_000, 99)).toBe(300_000);
    expect(nextPollDelayMs(15_000, 99, 2, 60_000)).toBe(60_000);
  });
});

describe("createSnapshotPoller — pile-up is structurally impossible", () => {
  it("never opens a second request while one is in flight", () => {
    const env = fakeEnv();
    const hang = hangingFetcher();
    const poller = createSnapshotPoller(
      () => hang.fetcher,
      { onData: () => {}, onSettled: () => {} },
      { intervalMs: 15_000 },
      env,
    );

    poller.start();
    expect(hang.calls()).toBe(1);
    expect(poller.inFlight()).toBe(true);

    // The old `setInterval` shape would start a request on every tick. Here the
    // chain schedules from COMPLETION, so a hung request means nothing is even
    // queued — and a stray attempt is dropped rather than piled on.
    expect(env.pending()).toBe(0);
    for (let i = 0; i < 20; i += 1) {
      poller.start();
      env.fire();
    }
    expect(hang.calls()).toBe(1);
  });

  it("schedules the next request only after the previous one settles", async () => {
    const env = fakeEnv();
    const seen: number[] = [];
    let n = 0;
    const poller = createSnapshotPoller(
      () => async () => {
        n += 1;
        return n;
      },
      { onData: (value: number) => seen.push(value), onSettled: () => {} },
      { intervalMs: 15_000 },
      env,
    );

    poller.start();
    await Promise.resolve();
    await Promise.resolve();
    expect(seen).toEqual([1]);
    // One — and only one — follow-up is queued after the settle.
    expect(env.pending()).toBe(1);

    env.fire();
    await Promise.resolve();
    await Promise.resolve();
    expect(seen).toEqual([1, 2]);
    expect(env.pending()).toBe(1);
  });

  it("stop() aborts the in-flight request so the socket is released", () => {
    const env = fakeEnv();
    const hang = hangingFetcher();
    const poller = createSnapshotPoller(
      () => hang.fetcher,
      { onData: () => {}, onSettled: () => {} },
      { intervalMs: 15_000 },
      env,
    );
    poller.start();
    expect(env.aborted()).toBe(0);
    poller.stop();
    expect(env.aborted()).toBe(1);
    expect(poller.inFlight()).toBe(false);
    // A stopped poller refuses further work — no zombie chain after unmount.
    poller.start();
    expect(hang.calls()).toBe(1);
  });

  it("refresh() supersedes the in-flight request instead of doubling it", () => {
    const env = fakeEnv();
    const hang = hangingFetcher();
    const poller = createSnapshotPoller(
      () => hang.fetcher,
      { onData: () => {}, onSettled: () => {} },
      { intervalMs: 15_000 },
      env,
    );
    poller.start();
    poller.refresh();
    expect(env.aborted()).toBe(1);
    expect(hang.calls()).toBe(2);
    // Still exactly one open request, not two.
    expect(poller.inFlight()).toBe(true);
  });

  it("a rejecting fetcher backs off and keeps the chain alive", async () => {
    const env = fakeEnv();
    const settles: number[] = [];
    const poller = createSnapshotPoller(
      () => () => Promise.reject(new Error("boom")),
      { onData: () => {}, onSettled: () => settles.push(1) },
      { intervalMs: 1_000 },
      env,
    );

    poller.start();
    await Promise.resolve();
    await Promise.resolve();
    expect(poller.failures()).toBe(1);
    expect(settles).toHaveLength(1);
    expect(env.pending()).toBe(1);

    env.fire();
    await Promise.resolve();
    await Promise.resolve();
    expect(poller.failures()).toBe(2);
    expect(env.pending()).toBe(1);
  });

  it("a fetcher that throws synchronously does not wedge the chain", () => {
    const env = fakeEnv();
    const poller = createSnapshotPoller(
      () => () => {
        throw new Error("sync boom");
      },
      { onData: () => {}, onSettled: () => {} },
      { intervalMs: 1_000 },
      env,
    );
    poller.start();
    expect(poller.inFlight()).toBe(false);
    expect(poller.failures()).toBe(1);
    expect(env.pending()).toBe(1);
  });

  it("a late resolution from an aborted request is ignored", async () => {
    const env = fakeEnv();
    const seen: string[] = [];
    let resolveFirst: ((v: string) => void) | null = null;
    let call = 0;
    const poller = createSnapshotPoller(
      () => () => {
        call += 1;
        if (call === 1) return new Promise<string>((r) => (resolveFirst = r));
        return Promise.resolve("second");
      },
      { onData: (v: string) => seen.push(v), onSettled: () => {} },
      { intervalMs: 1_000 },
      env,
    );

    poller.start();
    poller.refresh(); // aborts call 1, starts call 2
    await Promise.resolve();
    await Promise.resolve();
    expect(seen).toEqual(["second"]);

    // The abandoned first request answering late must not overwrite the newer
    // snapshot (the stale-write hazard every naive poller has).
    resolveFirst?.("first");
    await Promise.resolve();
    await Promise.resolve();
    expect(seen).toEqual(["second"]);
  });

  it("uses the CURRENT fetcher on each attempt without restarting the chain", async () => {
    const env = fakeEnv();
    const seen: string[] = [];
    let current = () => Promise.resolve("a");
    const poller = createSnapshotPoller(
      () => current,
      { onData: (v: string) => seen.push(v), onSettled: () => {} },
      { intervalMs: 1_000 },
      env,
    );
    poller.start();
    await Promise.resolve();
    await Promise.resolve();
    current = () => Promise.resolve("b");
    env.fire();
    await Promise.resolve();
    await Promise.resolve();
    expect(seen).toEqual(["a", "b"]);
  });
});

describe("createSnapshotPoller — default environment", () => {
  it("falls back to real timers when no env is injected", async () => {
    vi.useFakeTimers();
    try {
      const seen: number[] = [];
      const poller = createSnapshotPoller(
        () => async () => 7,
        { onData: (v: number) => seen.push(v), onSettled: () => {} },
        { intervalMs: 50 },
      );
      poller.start();
      await vi.advanceTimersByTimeAsync(0);
      expect(seen).toEqual([7]);
      await vi.advanceTimersByTimeAsync(60);
      expect(seen.length).toBeGreaterThan(1);
      poller.stop();
    } finally {
      vi.useRealTimers();
    }
  });
});
