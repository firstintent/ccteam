// A periodic snapshot fetch that CANNOT pile up.
//
// The failure this exists to prevent (observed 2026-08-02 on a host whose
// route to the daemon was slow): `setInterval(load, 15_000)` fires on wall
// time, independent of whether the previous request finished. Once one request
// takes longer than the interval, every tick adds another in-flight request
// and the queue grows without bound. A browser opens only ~6 HTTP/1.1
// connections per origin, and long-lived SSE streams already hold some of
// them, so a handful of stuck snapshot requests starve EVERY other request on
// the page — REST calls, and even static assets. One slow endpoint becomes a
// dead UI.
//
// The fix is structural, not a tuning knob: schedule the next request from the
// COMPLETION of the previous one (a self-rescheduling timer chain), never from
// a fixed-rate interval. `intervalMs` therefore means "quiet gap between
// requests", which makes concurrent duplicates impossible by construction —
// a slow endpoint degrades to a lower refresh rate instead of a request pile.
// Failures back off exponentially so an endpoint that is down is not hammered,
// and the in-flight request is aborted on stop so a departed view never holds
// a connection.
//
// Shape follows `useSessionEvents`: a pure, injectable engine
// ([`createSnapshotPoller`]) holds the whole lifecycle so the invariants are
// testable in Vitest's node environment without React, a DOM, real timers, or
// a real network; the hook is a thin binding to component state.

import { useCallback, useEffect, useRef, useState } from "react";

/** Injectable timer seam — the engine never touches globals directly. */
export interface SnapshotPollerEnvironment {
  setTimer(callback: () => void, delay: number): ReturnType<typeof setTimeout>;
  clearTimer(timer: ReturnType<typeof setTimeout>): void;
  createAbortController(): AbortController;
}

export interface SnapshotPollerOptions {
  /** Quiet gap between the END of one request and the START of the next. */
  intervalMs: number;
  /** Multiplier applied per consecutive failure (default 2). */
  backoffFactor?: number;
  /** Ceiling for the backed-off gap (default 5 minutes). */
  backoffCapMs?: number;
}

export interface SnapshotPollerCallbacks<T> {
  onData(data: T): void;
  /** Fired after every settle (success or failure) — drives `loading: false`. */
  onSettled(): void;
}

export interface SnapshotPoller {
  /** Fetch immediately, then keep rescheduling. */
  start(): void;
  /** Fetch now: cancels the pending timer AND aborts any in-flight request. */
  refresh(): void;
  /** Abort in-flight, cancel the timer, and refuse further work. */
  stop(): void;
  /** Test seam: is a request open right now? */
  inFlight(): boolean;
  /** Test seam: consecutive failures feeding the backoff. */
  failures(): number;
}

const DEFAULT_BACKOFF_FACTOR = 2;
const DEFAULT_BACKOFF_CAP_MS = 300_000;

export const defaultSnapshotPollerEnvironment: SnapshotPollerEnvironment = {
  setTimer: (callback, delay) => setTimeout(callback, delay),
  clearTimer: (timer) => clearTimeout(timer),
  createAbortController: () => new AbortController(),
};

/** Delay before the next attempt: `intervalMs` while healthy, exponentially
 *  backed off (capped) after consecutive failures. Pure. */
export function nextPollDelayMs(
  intervalMs: number,
  consecutiveFailures: number,
  backoffFactor: number = DEFAULT_BACKOFF_FACTOR,
  capMs: number = DEFAULT_BACKOFF_CAP_MS,
): number {
  if (consecutiveFailures <= 0) return intervalMs;
  return Math.min(capMs, intervalMs * backoffFactor ** consecutiveFailures);
}

/**
 * The polling engine. `getFetcher` is called per attempt so a caller can swap
 * the fetcher without restarting the chain; the fetcher receives an
 * `AbortSignal` and MUST pass it to `fetch` for abort to release the socket.
 *
 * Invariant: at most ONE request is open at any time. An attempt that arrives
 * while one is open is DROPPED, never queued — that is what makes pile-up
 * impossible regardless of how slow the endpoint is.
 */
export function createSnapshotPoller<T>(
  getFetcher: () => (signal: AbortSignal) => Promise<T>,
  callbacks: SnapshotPollerCallbacks<T>,
  options: SnapshotPollerOptions,
  env: SnapshotPollerEnvironment = defaultSnapshotPollerEnvironment,
): SnapshotPoller {
  const { intervalMs, backoffFactor, backoffCapMs } = options;
  let stopped = false;
  let open: AbortController | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let consecutiveFailures = 0;

  const schedule = (): void => {
    if (stopped || timer !== null) return;
    const delay = nextPollDelayMs(intervalMs, consecutiveFailures, backoffFactor, backoffCapMs);
    timer = env.setTimer(() => {
      timer = null;
      attempt();
    }, delay);
  };

  const attempt = (): void => {
    if (stopped) return;
    // THE invariant: one at a time. Never queue behind an open request.
    if (open) return;
    const controller = env.createAbortController();
    open = controller;
    let settled = false;
    const settle = (): void => {
      if (settled) return;
      settled = true;
      if (open === controller) open = null;
      if (stopped || controller.signal.aborted) return;
      callbacks.onSettled();
      schedule();
    };
    let promise: Promise<T>;
    try {
      promise = getFetcher()(controller.signal);
    } catch {
      // A fetcher that throws synchronously must not wedge the chain.
      consecutiveFailures += 1;
      settle();
      return;
    }
    void promise.then(
      (data) => {
        if (!stopped && !controller.signal.aborted) {
          consecutiveFailures = 0;
          callbacks.onData(data);
        }
        settle();
      },
      () => {
        // Best-effort: keep the last good snapshot on screen, back off.
        if (!stopped && !controller.signal.aborted) consecutiveFailures += 1;
        settle();
      },
    );
  };

  return {
    start: attempt,
    refresh: () => {
      if (stopped) return;
      if (timer !== null) {
        env.clearTimer(timer);
        timer = null;
      }
      // A manual refresh SUPERSEDES what is in flight — abort it rather than
      // letting the new request become a second open connection.
      open?.abort();
      open = null;
      attempt();
    },
    stop: () => {
      stopped = true;
      if (timer !== null) {
        env.clearTimer(timer);
        timer = null;
      }
      // Release the socket: a departed view must not keep a request open.
      open?.abort();
      open = null;
    },
    inFlight: () => open !== null,
    failures: () => consecutiveFailures,
  };
}

export interface PolledSnapshotResult<T> {
  data: T;
  loading: boolean;
  /** Fetch now, cancelling any in-flight request and resetting the timer. */
  refresh(): void;
}

/**
 * Keep `data` fresh from `fetcher`, at most one request in flight at a time.
 *
 * `fetcher` is read per attempt, so an inline closure does not restart the
 * poll on every render; pass `deps` to intentionally restart it (e.g. a slug
 * changed).
 */
export function usePolledSnapshot<T>(
  fetcher: (signal: AbortSignal) => Promise<T>,
  initial: T,
  options: SnapshotPollerOptions & { env?: SnapshotPollerEnvironment },
  deps: readonly unknown[] = [],
): PolledSnapshotResult<T> {
  const { intervalMs, backoffFactor, backoffCapMs, env } = options;
  const [data, setData] = useState<T>(initial);
  const [loading, setLoading] = useState(true);

  // Latest fetcher without restarting the chain. Refreshed in an effect
  // declared BEFORE the polling effect, so it is current before the first
  // attempt on every commit — never assigned during render.
  const fetcherRef = useRef(fetcher);
  useEffect(() => {
    fetcherRef.current = fetcher;
  }, [fetcher]);

  const pollerRef = useRef<SnapshotPoller | null>(null);
  const refresh = useCallback(() => pollerRef.current?.refresh(), []);

  useEffect(() => {
    const poller = createSnapshotPoller<T>(
      () => fetcherRef.current,
      {
        onData: setData,
        onSettled: () => setLoading(false),
      },
      { intervalMs, backoffFactor, backoffCapMs },
      env,
    );
    pollerRef.current = poller;
    poller.start();
    return () => {
      poller.stop();
      if (pollerRef.current === poller) pollerRef.current = null;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [intervalMs, backoffFactor, backoffCapMs, env, ...deps]);

  return { data, loading, refresh };
}
