// v0.10.2 (WEB-DSH-1) — the DSH page's shared status store. Before this, the
// status lived in DshView's local state, so leaving `/dsh` unmounted the view
// AND the <iframe> — every menu click re-booted the whole DSH SPA (assets +
// RPC init). Now the status lives in a module-level store: DshView (status
// head / empty states) and DshFrameHost (the persistent keep-alive iframe)
// read ONE source, and there is exactly one starting-poll, owned here.
//
// Lazy gate: `visited` flips only when DshView first mounts (the user's first
// `/dsh` visit). Before that the store issues ZERO requests and the frame host
// renders nothing — users who never open DSH never touch `/api/v1/dsh/*`.
// stop→start still produces a fresh iframe on purpose (src passes through
// null): a new instance is a new page.

import { useSyncExternalStore } from "react";

import { getDshStatus, type DshStatus } from "../lib/dshApi";

export interface DshSnapshot {
  /** True after the first `/dsh` visit; gates ALL dsh traffic. */
  visited: boolean;
  status: DshStatus | null;
  loading: boolean;
  fetchError: string | null;
}

/** How often to re-poll while the instance is booting (`starting`). */
const STARTING_POLL_MS = 1500;

const SERVER_SNAPSHOT: DshSnapshot = {
  visited: false,
  status: null,
  loading: true,
  fetchError: null,
};

export interface DshStoreDeps {
  fetchStatus?: () => Promise<DshStatus>;
  pollMs?: number;
}

export interface DshStore {
  getSnapshot(): DshSnapshot;
  getServerSnapshot(): DshSnapshot;
  subscribe(listener: () => void): () => void;
  /** Called by DshView on every mount: first call flips `visited`; every call
   *  revalidates the status in the background (a running instance keeps the
   *  same embed src, so the persistent iframe is never reloaded by this). */
  visit(): void;
  refresh(): Promise<void>;
  /** start/stop/restart: the result status lands in the store so BOTH the
   *  head and the frame host see it (no duplicate fetch per consumer). */
  runAction(action: () => Promise<DshStatus>): Promise<void>;
}

export function createDshStore(deps: DshStoreDeps = {}): DshStore {
  const fetchStatus = deps.fetchStatus ?? getDshStatus;
  const pollMs = deps.pollMs ?? STARTING_POLL_MS;
  let snapshot: DshSnapshot = { ...SERVER_SNAPSHOT };
  const listeners = new Set<() => void>();
  let poll: ReturnType<typeof setInterval> | null = null;
  let inFlight: Promise<void> | null = null;

  const emit = (): void => {
    for (const listener of listeners) listener();
  };

  const set = (patch: Partial<DshSnapshot>): void => {
    snapshot = { ...snapshot, ...patch };
    emit();
  };

  const stopPoll = (): void => {
    if (poll !== null) {
      clearInterval(poll);
      poll = null;
    }
  };

  // Poll only while booting — a running/attached/stopped instance is
  // quiescent. Gated on `visited` so the poll can never exist for a user who
  // has never opened the page.
  const syncPoll = (): void => {
    const want = snapshot.visited && snapshot.status?.state === "starting";
    if (want && poll === null) {
      poll = setInterval(() => void refresh(), pollMs);
    } else if (!want) {
      stopPoll();
    }
  };

  async function refresh(): Promise<void> {
    if (inFlight) return inFlight;
    const request = (async () => {
      try {
        const next = await fetchStatus();
        set({ status: next, fetchError: null, loading: false });
      } catch (e) {
        // 401 is handled by the global token gate, not an inline error card.
        if (e instanceof Error && e.message === "UNAUTHENTICATED") {
          set({ loading: false });
        } else {
          set({ fetchError: e instanceof Error ? e.message : String(e), loading: false });
        }
      }
    })();
    inFlight = request;
    try {
      await request;
    } finally {
      if (inFlight === request) inFlight = null;
      syncPoll();
    }
  }

  return {
    getSnapshot: () => snapshot,
    getServerSnapshot: () => SERVER_SNAPSHOT,
    subscribe(listener) {
      listeners.add(listener);
      return () => {
        listeners.delete(listener);
      };
    },
    visit() {
      if (!snapshot.visited) set({ visited: true });
      void refresh();
    },
    refresh,
    async runAction(action) {
      try {
        const next = await action();
        set({ status: next, fetchError: null });
      } catch (e) {
        if (!(e instanceof Error && e.message === "UNAUTHENTICATED")) {
          set({ fetchError: e instanceof Error ? e.message : String(e) });
        }
      }
      syncPoll();
    },
  };
}

export const dshStore = createDshStore();

export function useDshStatus(): DshSnapshot {
  return useSyncExternalStore(dshStore.subscribe, dshStore.getSnapshot, dshStore.getServerSnapshot);
}
