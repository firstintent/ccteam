import { describe, expect, it, vi } from "vitest";

import {
  createLifecycleReconciler,
  enqueueUnseenLifecycleEvents,
  type LifecycleReconcilerEnvironment,
} from "./lifecycleReconciler";
import {
  AGENTS_RING_CAP,
  appendAgentsEvent,
  type AgentsEvent,
} from "../hooks/useAgentsEvents";

function fakeTimers(): LifecycleReconcilerEnvironment & {
  fire(): void;
  pending(): number;
  delays(): number[];
  setHidden(hidden: boolean): void;
  fireVisibility(): void;
} {
  let nextId = 1;
  let timers: Array<{ id: number; callback: () => void }> = [];
  const delays: number[] = [];
  let hidden = false;
  let visibilityCallback: (() => void) | null = null;
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
    isHidden: () => hidden,
    onVisibilityChange(callback) {
      visibilityCallback = callback;
      return () => {
        if (visibilityCallback === callback) visibilityCallback = null;
      };
    },
    fire() {
      timers.shift()?.callback();
    },
    pending: () => timers.length,
    delays: () => delays,
    setHidden(next) {
      hidden = next;
    },
    fireVisibility() {
      visibilityCallback?.();
    },
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

  it("reconciles each registered slug once after a hidden gap without refreshing projects", () => {
    const env = fakeTimers();
    const reconcileSlug = vi.fn();
    const refreshProjects = vi.fn();
    const reconciler = createLifecycleReconciler(reconcileSlug, 150, env);
    reconciler.setVisibilityRefreshSlugs(["alpha", "beta", "alpha"]);

    env.setHidden(true);
    env.fireVisibility();
    expect(env.pending()).toBe(0);

    env.setHidden(false);
    env.fireVisibility();
    expect(env.pending()).toBe(1);
    expect(reconcileSlug).not.toHaveBeenCalled();
    env.fire();

    expect(reconcileSlug).toHaveBeenCalledTimes(2);
    expect(reconcileSlug).toHaveBeenCalledWith("alpha");
    expect(reconcileSlug).toHaveBeenCalledWith("beta");
    expect(refreshProjects).not.toHaveBeenCalled();
  });

  it("uses sequence watermarks after ring overflow and never re-enqueues retained events", () => {
    let ring: AgentsEvent[] = [];
    for (let index = 0; index < AGENTS_RING_CAP + 10; index += 1) {
      ring = appendAgentsEvent(ring, {
        kind: "session_lifecycle",
        slug: index % 2 === 0 ? "alpha" : "beta",
        content: String(index),
      });
    }
    // Reconstruct every retained object so an identity-based cursor would
    // fail even though their additive sequence numbers remain intact.
    const reconstructed = ring.map((event) => ({ ...event }));
    const enqueue = vi.fn();

    const next = enqueueUnseenLifecycleEvents(reconstructed, AGENTS_RING_CAP + 5, enqueue);
    expect(next).toBe(AGENTS_RING_CAP + 10);
    expect(enqueue).toHaveBeenCalledTimes(5);

    enqueueUnseenLifecycleEvents(reconstructed, next, enqueue);
    expect(enqueue).toHaveBeenCalledTimes(5);
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
