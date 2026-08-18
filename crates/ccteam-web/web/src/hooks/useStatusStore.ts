import { useSyncExternalStore } from "react";

import { getStatus, type StatusSnapshot } from "../lib/statusApi";
import {
  createSharedResourceStore,
  type SharedResourceEnvironment,
  type SharedResourceStore,
} from "./createSharedResourceStore";

export const STATUS_POLL_MS = 20_000;

const SERVER_SNAPSHOT = { data: null, loading: true, error: null } as const;

export function createStatusStore(options: {
  fetcher?: (signal: AbortSignal) => Promise<StatusSnapshot>;
  env?: SharedResourceEnvironment;
} = {}): SharedResourceStore<StatusSnapshot> {
  return createSharedResourceStore({
    fetcher:
      options.fetcher ??
      ((signal) => getStatus({ signal, background: true })),
    intervalMs: STATUS_POLL_MS,
    env: options.env,
  });
}

export const statusStore = createStatusStore();

/** One daemon-wide status request chain shared by every mounted consumer. */
export function useStatusStore() {
  const snapshot = useSyncExternalStore(
    statusStore.subscribe,
    statusStore.getSnapshot,
    () => SERVER_SNAPSHOT,
  );
  return { ...snapshot, refresh: statusStore.refresh };
}
