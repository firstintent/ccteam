import { retryDelayMs } from "./retryBackoff";

export interface LifecycleReconcilerEnvironment {
  setTimer(callback: () => void, delayMs: number): ReturnType<typeof setTimeout>;
  clearTimer(timer: ReturnType<typeof setTimeout>): void;
  random?(): number;
  isHidden?(): boolean;
  onVisibilityChange?(callback: () => void): () => void;
}

export interface LifecycleReconciler {
  enqueue(slug: string | undefined): void;
  stop(): void;
}

const defaultEnvironment: LifecycleReconcilerEnvironment = {
  setTimer: (callback, delayMs) => setTimeout(callback, delayMs),
  clearTimer: (timer) => clearTimeout(timer),
  random: () => Math.random(),
  isHidden: () => typeof document !== "undefined" && document.hidden,
  onVisibilityChange: (callback) => {
    if (typeof document === "undefined") return () => {};
    document.addEventListener("visibilitychange", callback);
    return () => document.removeEventListener("visibilitychange", callback);
  },
};

/** Collect lifecycle frames for a short window and reconcile each named
 * project once. Failed slugs retry independently with jittered backoff; hidden
 * tabs hold their pending slugs until one immediate visible-state reconcile.
 * The API accepts only a slug callback, so it has no path that can refresh
 * `/projects` or fan out across unrelated projects. */
export function createLifecycleReconciler(
  reconcileSlug: (slug: string) => void | Promise<void>,
  delayMs: number = 150,
  env: LifecycleReconcilerEnvironment = defaultEnvironment,
): LifecycleReconciler {
  const pending = new Set<string>();
  const active = new Set<string>();
  const failures = new Map<string, number>();
  const retryTimers = new Map<string, ReturnType<typeof setTimeout>>();
  const isHidden = () => env.isHidden?.() ?? false;
  const random = () => env.random?.() ?? Math.random();
  let debounceTimer: ReturnType<typeof setTimeout> | null = null;
  let stopped = false;

  const scheduleDebounce = (): void => {
    if (stopped || isHidden() || pending.size === 0 || debounceTimer !== null) return;
    debounceTimer = env.setTimer(flush, delayMs);
  };

  const scheduleRetry = (slug: string): void => {
    if (stopped) return;
    if (isHidden()) {
      pending.add(slug);
      return;
    }
    const delay = retryDelayMs(failures.get(slug) ?? 1, random());
    const timer = env.setTimer(() => {
      retryTimers.delete(slug);
      attempt(slug);
    }, delay);
    retryTimers.set(slug, timer);
  };

  const attempt = (slug: string): void => {
    if (stopped) return;
    if (isHidden()) {
      pending.add(slug);
      return;
    }
    if (active.has(slug) || retryTimers.has(slug)) {
      pending.add(slug);
      return;
    }
    active.add(slug);
    let result: void | Promise<void>;
    try {
      result = reconcileSlug(slug);
    } catch (error) {
      result = Promise.reject(error);
    }
    void Promise.resolve(result).then(
      () => {
        active.delete(slug);
        failures.delete(slug);
        if (pending.has(slug)) scheduleDebounce();
      },
      () => {
        active.delete(slug);
        pending.delete(slug);
        failures.set(slug, (failures.get(slug) ?? 0) + 1);
        scheduleRetry(slug);
      },
    );
  };

  const flush = (): void => {
    debounceTimer = null;
    const slugs = [...pending];
    pending.clear();
    for (const slug of slugs) attempt(slug);
  };

  const removeVisibility = env.onVisibilityChange?.(() => {
    if (isHidden()) {
      if (debounceTimer !== null) env.clearTimer(debounceTimer);
      debounceTimer = null;
      for (const [slug, timer] of retryTimers) {
        env.clearTimer(timer);
        pending.add(slug);
      }
      retryTimers.clear();
      return;
    }
    // Becoming visible reconciles every accumulated slug immediately once.
    if (pending.size > 0) flush();
  });

  return {
    enqueue: (slug) => {
      if (!slug || stopped) return;
      if (retryTimers.has(slug)) return;
      pending.add(slug);
      if (active.has(slug)) return;
      scheduleDebounce();
    },
    stop: () => {
      stopped = true;
      pending.clear();
      if (debounceTimer !== null) env.clearTimer(debounceTimer);
      debounceTimer = null;
      for (const timer of retryTimers.values()) env.clearTimer(timer);
      retryTimers.clear();
      removeVisibility?.();
    },
  };
}
