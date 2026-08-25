/**
 * The seam between the host half (Cordis plugin + BFF) and the client half
 * (React panel). Both sides import ONLY from this file; neither imports the
 * other. The browser never sees a ccteam URL or credential — it speaks this
 * contract to the host, and the host speaks ccteam REST/SSE upstream.
 *
 * Wire shape: POST {API_PREFIX}/{method} with a JSON body, JSON response.
 * Events:     GET  {API_PREFIX}/events[?sid=<sid>] as text/event-stream.
 */

export const API_PREFIX = '/ccteam/api'

export type ApiMethod =
  | 'status' // connectivity + vendor availability (drives empty/error states)
  | 'team.graph' // delegation tree across vendors, grouped by project
  | 'session.history' // transcript rows for one sid
  | 'session.send' // submit a user turn to one sid
  | 'session.spawn' // create a new session (optionally with a first task)

export type Activity = 'working' | 'idle' | 'stale' | 'stuck'

export interface TeamNode {
  sid: string
  project: string
  vendor: string
  activity: Activity
  title?: string
  role?: string
  model?: string
  parentSid?: string
  costUsd?: number
  tokensTotal?: number
  lastActive?: string
  children: TeamNode[]
}

export interface TeamGraph {
  projects: Array<{ slug: string; nodes: TeamNode[] }>
}

export interface TranscriptRow {
  turnId: string
  role: 'user' | 'assistant'
  content: string
  ts?: string
}

export interface HistoryRequest {
  sid: string
  /** Return only rows after this turn id (incremental refresh). */
  since?: string
}

export interface HistoryResponse {
  sid: string
  rows: TranscriptRow[]
}

export interface SendRequest {
  sid: string
  text: string
}

/** Honest receipt: queued and failed states are surfaced, never swallowed. */
export interface SendReceipt {
  ok: boolean
  queued?: boolean
  queuedBehind?: string
  errorKind?: string
  error?: string
}

export interface SpawnRequest {
  /**
   * Project slug the session is created in. ccteam's create-session endpoint
   * is project-scoped and never infers a project, so the host falls back to
   * the configured default and reports an actionable error when neither is
   * set. Optional: existing callers keep compiling.
   */
  project?: string
  vendor: string
  model?: string
  effort?: string
  mode?: string
  title?: string
  task?: string
}

export interface SpawnResponse {
  ok: boolean
  sid?: string
  error?: string
}

export interface VendorAvailability {
  vendor: string
  installed: boolean
}

export interface StatusResponse {
  connected: boolean
  /** Set when not connected: what to tell the user. */
  reason?: 'unconfigured' | 'unreachable'
  vendors?: VendorAvailability[]
}

/**
 * One SSE frame from the host. `graph` frames invalidate the team tree;
 * `session` frames carry a transcript delta for the sid the client subscribed
 * to; `turn_done` marks a completed turn (badge counter feed). Unknown kinds
 * must be ignored by the client (forward-compat).
 */
export interface PanelEvent {
  kind: 'graph' | 'session' | 'turn_done' | string
  sid?: string
  row?: TranscriptRow
  activity?: Activity
}
