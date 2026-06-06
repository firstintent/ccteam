// v0.8.7 W4 (DD.1) — per-session transcript model for the rewired
// ChatConsole. Pure + dependency-free (no React, no `window` at module
// load) so the per-sid keying invariant is unit-testable in node env and
// shared as the single source of truth.
//
// THE FIX this enables: the old ChatConsole stored EVERY session's turns in
// ONE flat localStorage key (`ccteam.chat.rows.v1`) over one global WS, so
// switching sessions interleaved streams. Here each gateway `s{n}` session
// owns its OWN transcript (a per-sid localStorage key), so switching the
// sid view NEVER mixes two sessions' rows.

import type { SessionEvent, SessionEventOption } from "../hooks/useSessionEvents";
import type { SessionHistoryEvent } from "../lib/sessionsApi";

export type RowKind = "user" | "assistant" | "tool" | "system" | "approval";

/** One rendered transcript row. `approval` rows carry the W2 ChoicePrompt
 *  options (`{label, id}`) so ChatConsole can render clickable
 *  [Approve][Deny] chips, plus the `token` the web resolve path POSTs back
 *  (R-H1); `resolved` flips once the user clicks (so the chips disable). */
export interface TranscriptRow {
  id: string;
  kind: RowKind;
  content: string;
  /** Approval-only: the options to render as buttons (`{label, id}`). */
  options?: SessionEventOption[];
  /** Approval-only: the pending-resolution token the resolve POST carries
   *  (R-H1). Absent ⇒ the row can't be resolved (no affordance). */
  token?: string;
  /** Approval-only: true once an option was clicked. */
  resolved?: boolean;
}

export const ROWS_CAP = 400;

/** Per-sid localStorage key. Bumping the `v2` suffix (vs the old flat
 *  `ccteam.chat.rows.v1`) also abandons the session-mixing buffer. */
export function rowsKeyFor(sid: string): string {
  return `ccteam.chat.rows.v2.${sid}`;
}

/** Stable-ish id for a new row (no crypto dependency — collisions are
 *  cosmetic, only used as a React key). */
export function nextRowId(prefix: string): string {
  return `${prefix}-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

/** Append `row` to `rows`, capping the buffer at {@link ROWS_CAP} (oldest
 *  drop). Returns a NEW array. */
export function appendRow(rows: TranscriptRow[], row: TranscriptRow): TranscriptRow[] {
  const next = [...rows, row];
  return next.length > ROWS_CAP ? next.slice(next.length - ROWS_CAP) : next;
}

/** Map one SSE {@link SessionEvent} to a transcript row, or `null` when it
 *  carries nothing to render (an empty non-final progress edit). An event
 *  with non-empty `options` becomes an `approval` row (the W2 prompt);
 *  otherwise an `answer` becomes an assistant bubble and a `progress`
 *  becomes a system note. */
export function eventToRow(ev: SessionEvent): TranscriptRow | null {
  if (ev.options && ev.options.length > 0) {
    return {
      id: ev.id ?? nextRowId("approval"),
      kind: "approval",
      content: ev.content || "needs approval",
      options: ev.options,
      token: ev.token,
    };
  }
  if (ev.kind === "answer") {
    if (!ev.content) return null;
    return { id: ev.id ?? nextRowId("assistant"), kind: "assistant", content: ev.content };
  }
  // progress — only surface a finalizing edit with text (status churn is noise).
  if (ev.done && ev.content) {
    return { id: ev.id ?? nextRowId("system"), kind: "system", content: ev.content };
  }
  return null;
}

/** Seed a transcript from mirrored history (`GET /sessions/{sid}`). Each
 *  turn yields a user row (when it had a prompt) then an assistant row
 *  (when it had a reply). Used to populate a reopened per-session page
 *  before the live SSE takes over. */
export function historyToRows(events: SessionHistoryEvent[]): TranscriptRow[] {
  const rows: TranscriptRow[] = [];
  for (const ev of events) {
    if (ev.user) {
      rows.push({ id: `${ev.turn_id}-u`, kind: "user", content: ev.user });
    }
    if (ev.assistant) {
      rows.push({ id: `${ev.turn_id}-a`, kind: "assistant", content: ev.assistant });
    }
  }
  return rows;
}

/** Load a sid's persisted transcript from localStorage. Returns `[]` on
 *  miss / parse error / storage disabled. The `store` arg is injectable so
 *  tests don't need a DOM `localStorage`. */
export function loadRows(
  sid: string,
  store: Pick<Storage, "getItem"> | undefined = safeStorage(),
): TranscriptRow[] {
  if (!store) return [];
  try {
    const parsed = JSON.parse(store.getItem(rowsKeyFor(sid)) ?? "[]");
    return Array.isArray(parsed) ? (parsed as TranscriptRow[]) : [];
  } catch {
    return [];
  }
}

/** Persist a sid's transcript (capped). No-op on storage failure. */
export function saveRows(
  sid: string,
  rows: TranscriptRow[],
  store: Pick<Storage, "setItem"> | undefined = safeStorage(),
): void {
  if (!store) return;
  try {
    store.setItem(rowsKeyFor(sid), JSON.stringify(rows.slice(-ROWS_CAP)));
  } catch {
    // storage full / disabled — the in-memory transcript still works.
  }
}

/** Best-effort handle to `window.localStorage`, or `undefined` in a non-DOM
 *  (node / SSR) context. Keeps this module importable without a `window`. */
function safeStorage(): Storage | undefined {
  try {
    return typeof localStorage !== "undefined" ? localStorage : undefined;
  } catch {
    return undefined;
  }
}
