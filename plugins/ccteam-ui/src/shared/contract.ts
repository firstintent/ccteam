/**
 * The seam between the host half (Cordis plugin + BFF) and the client half
 * (the React workbench). Both sides import ONLY from this file; neither
 * imports the other. The browser never sees a ccteam URL or credential — it
 * speaks this contract to the host, and the host speaks ccteam REST/SSE
 * upstream.
 *
 * Wire shape: POST {API_PREFIX}/{method} with a JSON body, JSON response.
 * Events:     GET  {API_PREFIX}/events[?sid=<sid>] as text/event-stream.
 * Files:      POST {API_PREFIX}/upload?project=<slug>&name=<name> (raw bytes)
 *             GET  {API_PREFIX}/attachment?project=<slug>&name=<basename>
 */

export const API_PREFIX = '/ccteam/api'

/** Sub-paths under {@link API_PREFIX} that are not JSON methods. */
export const EVENTS_PATH = '/events'
export const UPLOAD_PATH = '/upload'
export const ATTACHMENT_PATH = '/attachment'

export type ApiMethod =
  | 'status' // connectivity + vendor availability (drives empty/error states)
  | 'team.graph' // delegation tree across vendors, grouped by project
  | 'catalog.projects' // projects the identity can spawn into
  | 'catalog.models' // per-vendor model ids + effort ladders (advisory)
  | 'catalog.roles' // roles defined in one project
  | 'session.history' // transcript page for one sid (newest-last, cursor for older)
  | 'session.status' // live statusline: model / effort / context window
  | 'session.send' // submit a user turn to one sid
  | 'session.spawn' // create a new session (optionally with a first task)
  | 'session.interrupt' // interrupt the running turn, keep the session
  | 'session.stop' // stop the session (explicit user command)
  | 'session.resolve' // answer a pending human-in-the-loop choice
  | 'session.rename' // set the session title

export type Activity = 'working' | 'idle' | 'stale' | 'stuck'

export interface TeamNode {
  sid: string
  project: string
  vendor: string
  activity: Activity
  title?: string
  role?: string
  model?: string
  effort?: string
  host?: string
  parentSid?: string
  costUsd?: number
  tokensTotal?: number
  lastActive?: string
  turnCount?: number
  children: TeamNode[]
}

export interface TeamGraph {
  projects: Array<{ slug: string; nodes: TeamNode[] }>
}

export interface ProjectInfo {
  slug: string
  /** Team label the daemon shows for the project (slug when unset). */
  team?: string
  host?: string
}

export interface ModelEntry {
  id: string
  displayName?: string
  /** Effort ladder this model takes (empty = the vendor-level ladder). */
  efforts: string[]
}

export interface VendorModels {
  vendor: string
  models: ModelEntry[]
  /** Vendor-level effort ladder (empty = no effort axis). */
  efforts: string[]
  observedAt?: string
}

export interface ModelsCatalog {
  vendors: VendorModels[]
}

export interface RolesRequest {
  project: string
}

export interface RolesResponse {
  project: string
  roles: string[]
}

/** One attachment on a turn, as the workbench renders it. */
export interface AttachmentRef {
  kind: 'image' | 'file' | 'skill'
  /** Display name (basename or skill id). */
  name: string
  /** Workbench-relative URL serving the bytes (unset for skills / unknown paths). */
  url?: string
}

export type StepKind = 'tool_call' | 'tool_result' | 'thinking' | 'file_change' | 'web_search' | 'command_exec'

/** One structured activity step of a live turn (tool call, command, …). */
export interface Step {
  itemId: string
  kind: StepKind | string
  name: string
  summary: string
  status: 'started' | 'completed' | string
  ts?: string
}

export interface ChoiceOption {
  id: string
  label: string
}

export interface TurnUsage {
  costUsd?: number
  inputTokens?: number
  outputTokens?: number
}

export interface TranscriptRow {
  turnId: string
  role: 'user' | 'assistant'
  content: string
  ts?: string
  vendor?: string
  status?: string
  attachments?: AttachmentRef[]
  usage?: TurnUsage
}

export interface HistoryRequest {
  sid: string
  /** Opaque cursor from a previous page's `nextBefore` (older rows). */
  before?: string
  limit?: number
}

export interface HistoryResponse {
  sid: string
  rows: TranscriptRow[]
  nextBefore?: string
  hasMore: boolean
}

export interface TurnAttachmentInput {
  kind: 'image' | 'file'
  /** The `path` returned by the upload endpoint. */
  path: string
}

export interface SendRequest {
  sid: string
  text: string
  attachments?: TurnAttachmentInput[]
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
   * set.
   */
  project?: string
  vendor: string
  model?: string
  effort?: string
  mode?: string
  /** Role file name under the project's `.claude/agents/`; empty = roleless. */
  role?: string
  title?: string
  task?: string
  attachments?: TurnAttachmentInput[]
}

export interface SpawnResponse {
  ok: boolean
  sid?: string
  error?: string
}

export interface SessionRef {
  sid: string
}

export interface SessionStatus {
  sid: string
  model?: string
  effort?: string
  context?: { usedTokens?: number; windowTokens?: number; pct?: number }
}

export interface ResolveRequest {
  sid: string
  token: string
  selection: string
}

export interface RenameRequest {
  sid: string
  title: string
}

export interface SimpleReceipt {
  ok: boolean
  errorKind?: string
  error?: string
}

export interface UploadResponse {
  ok: boolean
  attachment?: AttachmentRef & { path: string }
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
 * One per-session frame, translated from ccteam's session stream:
 * - `progress`: the running turn's narrative so far (a snapshot, not a delta;
 *   `done` closes the status line);
 * - `activity`: one structured step started/completed;
 * - `answer`: a delivered assistant message — the turn's result, or a
 *   human-in-the-loop prompt when `options` are present;
 * - `lifecycle`: the session changed state (started / evicted / stopped …).
 */
export type SessionEvent =
  | { kind: 'progress'; content: string; done: boolean; ts?: string }
  | { kind: 'activity'; step: Step; ts?: string }
  | {
    kind: 'answer'
    id: string
    content: string
    ts?: string
    status?: string
    attachments?: AttachmentRef[]
    options?: ChoiceOption[]
    token?: string
  }
  | { kind: 'lifecycle'; state: string; reason?: string; ts?: string }

/**
 * One SSE frame from the host. `graph` frames invalidate the team tree;
 * `turn_done` marks a completed turn (badge counter feed); `delegation`
 * frames narrate parent/child relations; `session` frames carry one
 * {@link SessionEvent} for the sid the client subscribed to. Unknown kinds
 * must be ignored by the client (forward-compat).
 */
export type PanelEvent =
  | { kind: 'graph' }
  | { kind: 'turn_done'; sid?: string }
  | { kind: 'delegation'; relation: string; parentSid?: string; childSid?: string; title?: string; reason?: string }
  | { kind: 'session'; sid: string; event: SessionEvent }
