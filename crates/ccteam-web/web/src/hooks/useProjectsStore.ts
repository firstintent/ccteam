import { useSyncExternalStore } from "react";

import { fetchDashboard, type DashboardRow } from "../lib/dashboardApi";
import {
  createSharedResourceStore,
  type SharedResourceEnvironment,
  type SharedResourceStore,
} from "./createSharedResourceStore";

const SERVER_SNAPSHOT = { data: null, loading: true, error: null } as const;

export function createProjectsStore(options: {
  fetcher?: (signal: AbortSignal) => Promise<DashboardRow[]>;
  env?: SharedResourceEnvironment;
} = {}): SharedResourceStore<DashboardRow[]> {
  return createSharedResourceStore({
    fetcher:
      options.fetcher ??
      ((signal) => fetchDashboard({ signal, background: true })),
    // Projects are slow-moving: fetch on first subscriber, focus/visibility,
    // and retry after failure. There is no healthy-state interval.
    env: options.env,
  });
}

export const projectsStore = createProjectsStore();

export function useProjectsStore() {
  const snapshot = useSyncExternalStore(
    projectsStore.subscribe,
    projectsStore.getSnapshot,
    () => SERVER_SNAPSHOT,
  );
  return { projects: snapshot.data, loading: snapshot.loading, error: snapshot.error };
}
