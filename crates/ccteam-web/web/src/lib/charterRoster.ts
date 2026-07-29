// v0.9.11 TEAM-6 — pure roster-grouping / remove-orchestration helpers for
// the charter tab's vendor roster, split out of `pages/CharterPanel.tsx` so
// they can be unit-tested apart from the (hook-free) views that render them
// — the same separation `charterState.ts`'s reducer already has from
// `CharterEditorView`. `handleRosterRemoveClick` in particular is a PLAIN
// function (no hooks): keeping it here also avoids tripping
// `react-refresh/only-export-components`, since `CharterPanel.tsx` should
// only ever export components.

import { deleteHost, type AgentHealth } from "./hostsApi";

/** One host's agent report, resolved for the roster. */
export interface RosterHost {
  host: string;
  hostname: string;
  /** `online` | `offline` (`HostSummary.status`; `local` is always `online`). */
  status: string;
  agents: AgentHealth[];
  /** Unix seconds of the last satellite heartbeat (`HostSummary`); absent for
   *  `local` and for any host the API didn't report one for. */
  last_heartbeat_unix?: number;
}

/** A satellite offline this long reads as forgotten rather than restarting,
 *  so the roster SUGGESTS cleanup. Display threshold only — nothing is
 *  automated off it; removal is always the user's explicit click. */
export const STALE_AFTER_DAYS = 7;

/** How long a host has been out of touch, derived from its last heartbeat.
 *  PURE (`nowMs` is a parameter, never `Date.now()` inside) so both the
 *  component and its tests pin the same clock.
 *
 *  `null` when there is no heartbeat to measure from (`local`, or an older
 *  daemon that doesn't send the field) — the caller then shows nothing
 *  rather than inventing an age. `label` picks the unit to render (`hours`
 *  under a day, `days` beyond); both numbers are floored, and a heartbeat in
 *  the future clamps to 0 rather than going negative. */
export function offlineAge(
  lastHeartbeatUnix: number | undefined,
  nowMs: number,
): { label: "hours" | "days"; hours: number; days: number; stale: boolean } | null {
  if (lastHeartbeatUnix == null) return null;
  const elapsedMs = Math.max(0, nowMs - lastHeartbeatUnix * 1000);
  const hours = Math.floor(elapsedMs / 3_600_000);
  const days = Math.floor(hours / 24);
  return {
    label: days >= 1 ? "days" : "hours",
    hours,
    days,
    stale: days >= STALE_AFTER_DAYS,
  };
}

/** Roster group order: `local` first, then online hosts before offline
 *  ones; stable within a bucket (keeps the API's own return order). */
export function sortRosterHosts(hosts: RosterHost[]): RosterHost[] {
  const rank = (h: RosterHost) => (h.host === "local" ? 0 : h.status === "online" ? 1 : 2);
  return hosts
    .map((h, index) => ({ h, index }))
    .sort((a, b) => rank(a.h) - rank(b.h) || a.index - b.index)
    .map(({ h }) => h);
}

/** Orchestrate one remove-button click against the confirm-armed host id +
 *  the DELETE call + roster/toast side effects. A PLAIN function (no
 *  hooks) — unlike a component this needs no live React tree to exercise,
 *  so it is unit-testable directly.
 *
 *  ONLINE host: first call arms `confirmingHost` and returns without
 *  calling the API (the button flips to the confirm label); a second call
 *  with the SAME host while still armed fires `deleteHost(host,{force:true})`.
 *  OFFLINE host: fires immediately (`force` omitted — the backend's own
 *  online check makes this safe regardless). On success the host drops out
 *  of `roster`; on failure nothing is removed and `onError` reports it
 *  (CharterPanel wires that to the toast bus). */
export function handleRosterRemoveClick(args: {
  host: string;
  online: boolean;
  confirmingHost: string | null;
  setConfirmingHost: (host: string | null) => void;
  setRoster: (updater: (prev: RosterHost[]) => RosterHost[]) => void;
  onError: (message: string) => void;
}): void {
  const { host, online, confirmingHost, setConfirmingHost, setRoster, onError } = args;
  if (online && confirmingHost !== host) {
    setConfirmingHost(host);
    return;
  }
  deleteHost(host, online ? { force: true } : undefined)
    .then(() => {
      setConfirmingHost(null);
      setRoster((prev) => prev.filter((h) => h.host !== host));
    })
    .catch((err) => {
      onError(err instanceof Error ? err.message : String(err));
    });
}
