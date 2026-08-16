import { describe, expect, it, vi } from "vitest";

import {
  createLifecycleReconciler,
  type LifecycleReconcilerEnvironment,
} from "./lifecycleReconciler";

function fakeTimers(): LifecycleReconcilerEnvironment & {
  fire(): void;
  pending(): number;
  delays(): number[];
} {
  let nextId = 1;
  let timers: Array<{ id: number; callback: () => void }> = [];
  const delays: number[] = [];
  return {
    setTimer(callback, delay) {
      const id = nextId++;
      timers.push({ id, callback });
      delays.push(delay);
      return id as unknown as ReturnType<typeof setTimeout>;
    },
    clearTimer(timer) {
      timers = timers.filter((entry) => entry.id !== (timer as unknown as number));
    },
    random: () => 0.5,
    fire() {
      timers.shift()?.callback();
    },
    pending: () => timers.length,
    delays: () => delays,
  };
}

describe("session lifecycle slug reconciliation", () => {
  it("debounces a burst to one reconcile per named slug and never refreshes projects", () => {
    const env = fakeTimers();
    const reconcileSlug = vi.fn();
    const refreshProjects = vi.fn();
    const reconciler = createLifecycleReconciler(reconcileSlug, 150, env);

    for (let frame = 0; frame < 20; frame += 1) {
      reconciler.enqueue(frame % 3 === 0 ? "beta" : "alpha");
    }
    reconciler.enqueue(undefined);

    expect(env.pending()).toBe(1);
    expect(reconcileSlug).not.toHaveBeenCalled();
    env.fire();

    expect(reconcileSlug).toHaveBeenCalledTimes(2);
    expect(reconcileSlug).toHaveBeenCalledWith("alpha");
    expect(reconcileSlug).toHaveBeenCalledWith("beta");
    expect(refreshProjects).not.toHaveBeenCalled();
  });

  it("starts a fresh window after the previous batch flushes", async () => {
    const env = fakeTimers();
    const reconcileSlug = vi.fn();
    const reconciler = createLifecycleReconciler(reconcileSlug, 150, env);

    reconciler.enqueue("alpha");
    env.fire();
    await Promise.resolve();
    await Promise.resolve();
    reconciler.enqueue("alpha");
    env.fire();

    expect(reconcileSlug).toHaveBeenCalledTimes(2);
  });

  it("backs a failing slug off exponentially and resets after success", async () => {
    const env = fakeTimers();
    const reconcileSlug = vi
      .fn<(slug: string) => Promise<void>>()
      .mockRejectedValueOnce(new Error("502"))
      .mockRejectedValueOnce(new Error("503"))
      .mockResolvedValueOnce()
      .mockRejectedValueOnce(new Error("network"));
    const reconciler = createLifecycleReconciler(reconcileSlug, 150, env);

    reconciler.enqueue("alpha");
    env.fire();
    await Promise.resolve();
    await Promise.resolve();
    expect(env.delays()).toEqual([150, 2_000]);

    env.fire();
    await Promise.resolve();
    await Promise.resolve();
    expect(env.delays()).toEqual([150, 2_000, 4_000]);

    env.fire();
    await Promise.resolve();
    await Promise.resolve();
    reconciler.enqueue("alpha");
    env.fire();
    await Promise.resolve();
    await Promise.resolve();
    expect(env.delays()).toEqual([150, 2_000, 4_000, 150, 2_000]);
  });
});
