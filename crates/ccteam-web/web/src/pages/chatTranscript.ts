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

import type {
  SessionActivity,
  SessionEvent,
  SessionEventOption,
} from "../hooks/useSessionEvents";
import type { OutboundAttachmentRef, SessionHistoryEvent, TurnStatus } from "../lib/sessionsApi";

export type RowKind = "user" | "assistant" | "tool" | "system" | "approval" | "activity";

/** One rendered transcript row. `approval` rows carry the W2 ChoicePrompt
 *  options (`{label, id}`) so ChatConsole can render clickable
 *  [Approve][Deny] chips, plus the `token` the web resolve path POSTs back
 *  (R-H1); `resolved` flips once the user clicks (so the chips disable).
 *  `activity` rows carry the structured per-step payload (v0.8.19) for a
 *  compact mono activity line. */
export interface TranscriptRow {
  id: string;
  kind: RowKind;
  content: string;
  /** Server-side timestamp (RFC 3339) — from the history event's `ts` or the
   *  live SSE frame's `ts` (WEB-TS-1). Rows of one turn legitimately share
   *  it; rows persisted before this field existed simply have none. */
  ts?: string;
  status?: TurnStatus;
  /** Project asset references only; rendering constructs a fixed same-origin
   * URL from `id` and never accepts a URL or byte payload from this state. */
  attachments?: OutboundAttachmentRef[];
  /** Approval-only: the options to render as buttons (`{label, id}`). */
  options?: SessionEventOption[];
  /** Approval-only: the pending-resolution token the resolve POST carries
   *  (R-H1). Absent ⇒ the row can't be resolved (no affordance). */
  token?: string;
  /** Approval-only: true once an option was clicked. */
  resolved?: boolean;
  /** Activity-only (single, legacy): the structured per-step payload. Only a
   *  bare/malformed activity frame (no fold) carries this now; folded runs use
   *  {@link fold}. */
  activity?: SessionActivity;
  /** Activity-only: the FOLDED run of activity steps (v0.8.21). Mirrors the IM
   *  ProgressFold — a turn's many tool/think steps collapse into ONE counter
   *  row. Present ⇒ render {@link TranscriptRow.content} as the fold line. */
  fold?: ActivityFold;
}

export const ROWS_CAP = 400;

/** Per-sid localStorage key. Bumping the suffix (`v3` ← `v2` ← the old flat
 *  `ccteam.chat.rows.v1`) abandons the prior buffer; `v3` retires the
 *  one-row-per-activity-step shape in favor of the folded counter row. */
export function rowsKeyFor(sid: string): string {
  return `ccteam.chat.rows.v3.${sid}`;
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
 *  with non-empty `options` becomes an `approval` row (the W2 prompt); an
 *  `activity` event becomes a compact `activity` row (v0.8.19); otherwise an
 *  `answer` becomes an assistant bubble and a `progress` becomes a system
 *  note. */
export function eventToRow(ev: SessionEvent): TranscriptRow | null {
  // Capacity/lifecycle frames update shell state; they are not conversation
  // content and must never appear as an assistant bubble.
  if (ev.kind === "session_lifecycle" || ev.kind === "scheduled_changed") return null;
  if (ev.options && ev.options.length > 0) {
    return {
      id: ev.id ?? nextRowId("approval"),
      kind: "approval",
      content: ev.content || "needs approval",
      ts: ev.ts,
      options: ev.options,
      token: ev.token,
    };
  }
  // A bare/malformed activity frame (no structured payload to fold) renders
  // as a single compact mono row. Well-formed activity frames are folded by
  // {@link appendEvent} into ONE counter row, so they never reach here.
  if (ev.kind === "activity") {
    const summary = ev.activity?.summary || ev.content;
    if (!summary) return null;
    return {
      id: ev.id ?? nextRowId("activity"),
      kind: "activity",
      content: summary,
      ts: ev.ts,
      activity: ev.activity,
    };
  }
  if (ev.kind === "answer") {
    if (!ev.content && (!ev.attachments || ev.attachments.length === 0)) return null;
    return {
      id: ev.id ?? nextRowId("assistant"),
      kind: "assistant",
      content: ev.content,
      ts: ev.ts,
      status: ev.status,
      attachments: ev.attachments,
    };
  }
  // progress — only surface a finalizing edit with text (status churn is noise).
  if (ev.done && ev.content) {
    return { id: ev.id ?? nextRowId("system"), kind: "system", content: ev.content, ts: ev.ts };
  }
  return null;
}

// ---- activity fold (v0.8.21) ----------------------------------------------
//
// THE FIX this enables: the gateway emits one activity frame per item
// lifecycle (start / update / complete) for EVERY step (see
// `ccteam-im/src/gateway.rs` "ACTIVITY (web-only)"), so a turn with 40 bash +
// 15 read + 16 edit calls produced ~140 rows — a wall of `Bash(…)` lines that
// also blew past {@link ROWS_CAP} and evicted real messages. We fold a run of
// consecutive activity steps into ONE compact counter row, mirroring the IM
// `ProgressFold` (ccteam-im/src/progress.rs) so the two surfaces can't drift:
// `⏳ working… · 🔧 bash ×40 · 📖 read ×15 · ✏️ edit ×16`.

/** A folded run of activity steps: each step counted ONCE (deduped by
 *  `item_id` — a tool's start+complete pair, or a reasoning item's repeated
 *  updates, must not double-count) and bucketed by category. */
export interface ActivityFold {
  /** `item_id`s already counted (dedup of the start/complete/update lifecycle). */
  seen: string[];
  /** Count buckets in first-seen order (stable render). */
  buckets: { emoji: string; label: string; count: number }[];
  /** Any reasoning step seen — drives the `💭 thinking…` head when no tool ran. */
  thinking: boolean;
}

interface ActivityCategory {
  emoji: string;
  label: string;
}

/** Mirror of the Rust `tool_category` (ccteam-im/src/progress.rs): a raw
 *  Claude/Codex tool name → a folded category. Match is case-sensitive (the
 *  adapter emits the canonical tool name); an unknown tool folds under the
 *  wrench keeping its own lowercased name, so nothing is silently dropped
 *  (e.g. `Agent` → `🔧 agent`). */
function toolCategory(name: string): ActivityCategory {
  switch (name) {
    case "Read":
    case "Grep":
    case "Glob":
    case "LS":
    case "NotebookRead":
      return { emoji: "📖", label: "read" };
    case "Bash":
    case "BashOutput":
    case "KillBash":
    case "KillShell":
      return { emoji: "🔧", label: "bash" };
    case "Edit":
    case "MultiEdit":
    case "Write":
    case "NotebookEdit":
      return { emoji: "✏️", label: "edit" };
    case "WebSearch":
    case "WebFetch":
      return { emoji: "🔎", label: "web" };
    case "Task":
      return { emoji: "🤖", label: "task" };
    case "TodoWrite":
      return { emoji: "📝", label: "todo" };
    default:
      return { emoji: "🔧", label: name.toLowerCase() || "tool" };
  }
}

/** The count bucket for one structured activity, keyed off its `kind` first
 *  (command/file/web fold to fixed categories), then the tool `name` for a
 *  generic `tool_call`. Returns `null` for `thinking` (it sets the head, not a
 *  bucket). Mirrors `ProgressFold::apply`. */
function activityCategory(a: SessionActivity): ActivityCategory | null {
  switch (a.kind) {
    case "thinking":
      return null;
    case "command_exec":
      return { emoji: "🔧", label: "bash" };
    case "file_change":
      return { emoji: "✏️", label: "edit" };
    case "web_search":
      return { emoji: "🔎", label: "web" };
    default: // tool_call / tool_result / anything else → key off the name
      return toolCategory(a.name);
  }
}

/** A fresh, empty fold. */
export function emptyFold(): ActivityFold {
  return { seen: [], buckets: [], thinking: false };
}

/** Fold one structured activity into `prev`, returning a NEW fold (pure +
 *  immutable). De-dups by `item_id`; a `thinking` step only flags the head. */
export function foldActivity(prev: ActivityFold, a: SessionActivity): ActivityFold {
  // Already counted (start↔complete pair, or a repeated reasoning update):
  // re-assert the thinking flag if needed, otherwise leave the fold untouched.
  if (a.item_id && prev.seen.includes(a.item_id)) {
    return a.kind === "thinking" && !prev.thinking ? { ...prev, thinking: true } : prev;
  }
  const seen = a.item_id ? [...prev.seen, a.item_id] : prev.seen;
  const cat = activityCategory(a);
  if (!cat) return { ...prev, seen, thinking: true };
  const buckets = prev.buckets.map((b) => ({ ...b }));
  const hit = buckets.find((b) => b.label === cat.label);
  if (hit) hit.count += 1;
  else buckets.push({ emoji: cat.emoji, label: cat.label, count: 1 });
  return { ...prev, seen, buckets };
}

/** Render a fold to its one-line summary, mirroring `ProgressFold::render`:
 *  `⏳ working… · 🔧 bash ×40 · 📖 read ×15`, or `💭 thinking…` when only
 *  reasoning has happened. */
export function renderFold(fold: ActivityFold): string {
  const head = fold.buckets.length === 0 && fold.thinking ? "💭 thinking…" : "⏳ working…";
  if (fold.buckets.length === 0) return head;
  const counts = fold.buckets.map((b) => `${b.emoji} ${b.label} ×${b.count}`).join(" · ");
  return `${head} · ${counts}`;
}

/** Reduce one SSE {@link SessionEvent} into the transcript, FOLDING a run of
 *  consecutive structured activity steps into a single counter row. Any other
 *  event (answer / approval / finalizing progress, or a bare activity frame
 *  with no payload) lands as its own row via {@link eventToRow} — which
 *  naturally "closes" the current fold, so the next activity starts a fresh
 *  one. This is the single entry the live SSE loop uses. */
export function appendEvent(rows: TranscriptRow[], ev: SessionEvent): TranscriptRow[] {
  if (ev.kind === "activity" && ev.activity) {
    const last = rows[rows.length - 1];
    if (last && last.kind === "activity" && last.fold) {
      const fold = foldActivity(last.fold, ev.activity);
      // The fold row's time is the LATEST step's, so a long turn's one
      // activity line visibly advances instead of pinning the first step's
      // clock (GitHub #186 B).
      const merged: TranscriptRow = { ...last, fold, content: renderFold(fold), ts: ev.ts ?? last.ts };
      return [...rows.slice(0, -1), merged];
    }
    const fold = foldActivity(emptyFold(), ev.activity);
    return appendRow(rows, {
      id: ev.id ?? nextRowId("activity"),
      kind: "activity",
      content: renderFold(fold),
      ts: ev.ts,
      fold,
    });
  }
  const row = eventToRow(ev);
  return row ? appendRow(rows, row) : rows;
}

/** Apply an authoritative history page on top of what the live stream
 *  delivered WHILE that page was being fetched (GitHub #186 A).
 *
 *  `events` is the sid's whole SSE buffer and `barrier` how much of it was
 *  already buffered when the history request left. Everything before the
 *  barrier was mirrored to turns.jsonl before it was fanned out, hence is
 *  represented by `seeded` (or is transient activity) — with ONE exception:
 *  approval prompts never enter history, so an unresolved approval row that
 *  arrived on this stream is carried over from `current`. Frames after the
 *  barrier raced the server's read and are re-applied, deduplicating a
 *  reply the page already mirrors by content, so a reply can neither vanish
 *  (`setRows(history)` over a just-folded answer — the "TG has it, web shows
 *  a bare completed turn" report) nor double. Pure — unit-testable. */
export function mergeHistory(
  current: TranscriptRow[],
  seeded: TranscriptRow[],
  events: SessionEvent[],
  barrier: number,
): TranscriptRow[] {
  let next = [...seeded];
  const delivered = new Set(
    events.slice(0, barrier).flatMap((ev) => (ev.id ? [ev.id] : [])),
  );
  for (const row of current) {
    if (
      row.kind === "approval" &&
      !row.resolved &&
      delivered.has(row.id) &&
      !next.some((r) => r.id === row.id)
    ) {
      next = appendRow(next, row);
    }
  }
  for (const ev of events.slice(barrier)) {
    const isApproval = ev.options !== undefined && ev.options.length > 0;
    if (
      ev.kind === "answer" &&
      !isApproval &&
      ev.content &&
      seeded.some((r) => r.kind === "assistant" && r.content === ev.content)
    ) {
      continue; // already mirrored into this page
    }
    if (ev.id && next.some((r) => r.id === ev.id)) continue;
    next = appendEvent(next, ev);
  }
  return next;
}

/** Seed a transcript from mirrored history (`GET /sessions/{sid}`). Each
 *  turn yields a user row (when it had a prompt) then an assistant row
 *  (when it had a reply). Used to populate a reopened per-session page
 *  before the live SSE takes over. */
export function historyToRows(events: SessionHistoryEvent[]): TranscriptRow[] {
  const rows: TranscriptRow[] = [];
  for (const ev of events) {
    if (ev.user) {
      rows.push({ id: `${ev.turn_id}-u`, kind: "user", content: ev.user, ts: ev.ts });
    }
    if (ev.assistant || (ev.attachments && ev.attachments.length > 0)) {
      rows.push({
        id: `${ev.turn_id}-a`,
        kind: "assistant",
        content: ev.assistant,
        ts: ev.ts,
        status: ev.status,
        attachments: ev.attachments,
      });
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
