import { retryDelayMs } from "../lib/retryBackoff";

export interface SharedResourceSnapshot<T> {
  data: T | null;
  loading: boolean;
  error: string | null;
}

export interface SharedResourceEnvironment {
  setTimer(callback: () => void, delayMs: number): ReturnType<typeof setTimeout>;
  clearTimer(timer: ReturnType<typeof setTimeout>): void;
  createAbortController(): AbortController;
  isHidden(): boolean;
  random(): number;
  onFocus(callback: () => void): () => void;
  onVisibilityChange(callback: () => void): () => void;
}

export interface SharedResourceStoreOptions<T> {
  fetcher(signal: AbortSignal): Promise<T>;
  /** Healthy completion-to-next-start gap. Omit for fetch/retry-only stores. */
  intervalMs?: number;
  failureBaseMs?: number;
  failureCapMs?: number;
  jitterRatio?: number;
  env?: SharedResourceEnvironment;
}

export interface SharedResourceStore<T> {
  getSnapshot(): SharedResourceSnapshot<T>;
  subscribe(listener: () => void): () => void;
  refresh(): void;
  /** Test diagnostics; production consumers only need subscribe/getSnapshot. */
  debug(): { subscribers: number; inFlight: boolean; failures: number };
}

const DEFAULT_FAILURE_BASE_MS = 2_000;
const DEFAULT_FAILURE_CAP_MS = 300_000;
const DEFAULT_JITTER_RATIO = 0.25;

export const defaultSharedResourceEnvironment: SharedResourceEnvironment = {
  setTimer: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimer: (timer) => clearTimeout(timer),
  createAbortController: () => new AbortController(),
  isHidden: () => typeof document !== "undefined" && document.hidden,
  random: () => Math.random(),
  onFocus: (callback) => {
    if (typeof window === "undefined") return () => {};
    window.addEventListener("focus", callback);
    return () => window.removeEventListener("focus", callback);
  },
  onVisibilityChange: (callback) => {
    if (typeof document === "undefined") return () => {};
    document.addEventListener("visibilitychange", callback);
    return () => document.removeEventListener("visibilitychange", callback);
  },
};

/**
 * Ref-counted resource engine used by the status and projects stores.
 *
 * A timer is installed only after the active request settles. Focus refreshes
 * never abort or duplicate an active request, and the final unsubscribe (or a
 * hidden document) aborts the request and clears every timer.
 */
export function createSharedResourceStore<T>(
  options: SharedResourceStoreOptions<T>,
): SharedResourceStore<T> {
  const env = options.env ?? defaultSharedResourceEnvironment;
  const failureBaseMs = options.failureBaseMs ?? DEFAULT_FAILURE_BASE_MS;
  const failureCapMs = options.failureCapMs ?? DEFAULT_FAILURE_CAP_MS;
  const jitterRatio = options.jitterRatio ?? DEFAULT_JITTER_RATIO;
  const listeners = new Set<() => void>();
  let snapshot: SharedResourceSnapshot<T> = { data: null, loading: true, error: null };
  let active: { controller: AbortController } | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let failures = 0;
  let removeFocus: (() => void) | null = null;
  let removeVisibility: (() => void) | null = null;

  const emit = (): void => {
    for (const listener of listeners) listener();
  };

  const setSnapshot = (next: SharedResourceSnapshot<T>): void => {
    snapshot = next;
    emit();
  };

  const clearPendingTimer = (): void => {
    if (timer === null) return;
    env.clearTimer(timer);
    timer = null;
  };

  const abortActive = (): void => {
    const request = active;
    active = null;
    request?.controller.abort();
  };

  const schedule = (delayMs: number): void => {
    if (listeners.size === 0 || env.isHidden() || timer !== null) return;
    timer = env.setTimer(() => {
      timer = null;
      attempt();
    }, delayMs);
  };

  const settleFailure = (request: { controller: AbortController }, error: unknown): void => {
    if (active !== request) return;
    active = null;
    if (request.controller.signal.aborted || listeners.size === 0 || env.isHidden()) return;
    failures += 1;
    setSnapshot({
      data: snapshot.data,
      loading: false,
      error: error instanceof Error ? error.message : String(error),
    });
    schedule(retryDelayMs(failures, env.random(), failureBaseMs, failureCapMs, jitterRatio));
  };

  const attempt = (): void => {
    if (listeners.size === 0 || env.isHidden() || active !== null) return;
    clearPendingTimer();
    const request = { controller: env.createAbortController() };
    active = request;
    let result: Promise<T>;
    try {
      result = options.fetcher(request.controller.signal);
    } catch (error) {
      settleFailure(request, error);
      return;
    }
    void result.then(
      (data) => {
        if (active !== request) return;
        active = null;
        if (request.controller.signal.aborted || listeners.size === 0 || env.isHidden()) return;
        failures = 0;
        setSnapshot({ data, loading: false, error: null });
        if (options.intervalMs !== undefined) schedule(options.intervalMs);
      },
      (error) => settleFailure(request, error),
    );
  };

  const refresh = (): void => {
    if (listeners.size === 0 || env.isHidden() || active !== null) return;
    clearPendingTimer();
    attempt();
  };

  const pause = (): void => {
    clearPendingTimer();
    abortActive();
  };

  const start = (): void => {
    removeFocus = env.onFocus(refresh);
    removeVisibility = env.onVisibilityChange(() => {
      if (env.isHidden()) pause();
      else refresh();
    });
    if (!env.isHidden()) attempt();
  };

  const stop = (): void => {
    pause();
    failures = 0;
    removeFocus?.();
    removeVisibility?.();
    removeFocus = null;
    removeVisibility = null;
  };

  return {
    getSnapshot: () => snapshot,
    subscribe: (listener) => {
      const first = listeners.size === 0;
      listeners.add(listener);
      if (first) start();
      return () => {
        listeners.delete(listener);
        if (listeners.size === 0) stop();
      };
    },
    refresh,
    debug: () => ({ subscribers: listeners.size, inFlight: active !== null, failures }),
  };
}
