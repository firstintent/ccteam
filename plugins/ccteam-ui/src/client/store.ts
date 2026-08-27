/**
 * One hand-rolled store for the whole panel: open/width, the view stack,
 * graph cache, per-sid transcripts, the badge counter, and recents. State
 * transitions are pure reducer functions (the unit-test surface); the store
 * wrapper adds subscribe/getSnapshot for useSyncExternalStore, and the
 * persistence attachment mirrors open/width/recents into localStorage under
 * `ccteam.console.*`.
 */
import type {
  Activity,
  SendReceipt,
  StatusResponse,
  TeamGraph,
  TeamNode,
  TranscriptRow,
  VendorAvailability,
} from '../shared/contract.js'

/** The seven spawnable vendors, in display order. */
export const VENDORS: readonly string[] = ['claude', 'codex', 'grok', 'opencode', 'kimi', 'pi', 'dsh']

/** Panel width bounds and default (px). */
export const MIN_WIDTH = 320
export const MAX_WIDTH = 720
export const DEFAULT_WIDTH = 400

/** Recents strip capacity. */
export const MAX_RECENTS = 3

/** localStorage keys (decree: everything under `ccteam.console.*`). */
export const STORAGE_KEYS = {
  open: 'ccteam.console.open',
  width: 'ccteam.console.width',
  recents: 'ccteam.console.recents',
  project: 'ccteam.console.project',
} as const

/** Connection phase driving the panel's state screens and the entry dot. */
export type ConnectionPhase = 'checking' | 'ok' | 'unreachable' | 'unconfigured'

/** One entry of the view stack; the top entry renders. */
export type View =
  | { kind: 'tree' }
  | { kind: 'chat'; sid: string }
  | { kind: 'spawn' }

/** Inline chat notice derived from a SendReceipt (honest, never swallowed). */
export interface ChatNotice {
  id: number
  kind: 'queued' | 'error'
  queuedBehind?: string
  errorKind?: string
  message?: string
}

/** Per-sid transcript state. */
export interface ChatState {
  rows: TranscriptRow[]
  activity: Activity | undefined
  loading: boolean
  error: string | null
  notices: ChatNotice[]
}

/** Whole-panel state. */
export interface ConsoleState {
  open: boolean
  width: number
  stack: View[]
  connection: { phase: ConnectionPhase; vendors: VendorAvailability[] }
  graph: TeamGraph | null
  graphLoading: boolean
  graphStale: boolean
  graphError: string | null
  chats: Record<string, ChatState>
  badge: number
  recents: string[]
  collapsed: Record<string, boolean>
  spawn: { busy: boolean; error: string | null }
  /** Last project the user spawned into (persisted; preselects the form). */
  spawnProject: string | null
  nextNoticeId: number
  nextLocalId: number
}

/** Every state transition. */
export type Action =
  | { type: 'open_panel' }
  | { type: 'close_panel' }
  | { type: 'toggle_panel' }
  | { type: 'back' }
  | { type: 'show_tree' }
  | { type: 'open_chat'; sid: string }
  | { type: 'open_spawn' }
  | { type: 'set_width'; width: number }
  | { type: 'toggle_project'; slug: string }
  | { type: 'status_loaded'; status: StatusResponse }
  | { type: 'status_failed' }
  | { type: 'graph_loading' }
  | { type: 'graph_loaded'; graph: TeamGraph }
  | { type: 'graph_failed'; message: string }
  | { type: 'graph_stale' }
  | { type: 'turn_done'; sid?: string }
  | { type: 'history_loading'; sid: string }
  | { type: 'history_loaded'; sid: string; rows: TranscriptRow[] }
  | { type: 'history_failed'; sid: string; message: string }
  | { type: 'event_row'; sid: string; row: TranscriptRow }
  | { type: 'activity'; sid: string; activity: Activity }
  | { type: 'send_started'; sid: string; text: string }
  | { type: 'send_settled'; sid: string; receipt: SendReceipt }
  | { type: 'send_failed'; sid: string; message: string }
  | { type: 'spawn_started' }
  | { type: 'spawn_failed'; message: string }
  | { type: 'spawn_done' }
  | { type: 'set_spawn_project'; project: string }

/** Persisted slice loaded at store creation. */
export interface Persisted {
  open?: boolean
  width?: number
  recents?: string[]
  project?: string
}

/**
 * Clamp a requested panel width to the allowed band.
 * @param width - requested width in px.
 * @returns the clamped width.
 */
export function clampWidth(width: number): number {
  if (!Number.isFinite(width)) return DEFAULT_WIDTH
  return Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, Math.round(width)))
}

/**
 * Build the initial state, folding in the persisted slice.
 * @param persisted - previously stored open/width/recents.
 * @returns the initial console state.
 */
export function initialState(persisted: Persisted = {}): ConsoleState {
  return {
    open: persisted.open ?? false,
    width: clampWidth(persisted.width ?? DEFAULT_WIDTH),
    stack: [{ kind: 'tree' }],
    connection: { phase: 'checking', vendors: [] },
    graph: null,
    graphLoading: false,
    graphStale: true,
    graphError: null,
    chats: {},
    badge: 0,
    recents: (persisted.recents ?? []).filter(sid => typeof sid === 'string').slice(0, MAX_RECENTS),
    collapsed: {},
    spawn: { busy: false, error: null },
    spawnProject: persisted.project ?? null,
    nextNoticeId: 1,
    nextLocalId: 1,
  }
}

const EMPTY_CHAT: ChatState = { rows: [], activity: undefined, loading: false, error: null, notices: [] }

function chatOf(state: ConsoleState, sid: string): ChatState {
  return state.chats[sid] ?? EMPTY_CHAT
}

function withChat(state: ConsoleState, sid: string, chat: ChatState): ConsoleState {
  return { ...state, chats: { ...state.chats, [sid]: chat } }
}

function pushRecent(recents: string[], sid: string): string[] {
  return [sid, ...recents.filter(existing => existing !== sid)].slice(0, MAX_RECENTS)
}

/**
 * Merge one live row into a transcript. Rows dedupe by turnId; a server user
 * row replaces the oldest optimistic local row carrying the same content, so
 * a sent message never doubles when its canonical row arrives.
 */
function mergeRow(rows: TranscriptRow[], row: TranscriptRow): TranscriptRow[] {
  if (rows.some(existing => existing.turnId === row.turnId)) return rows
  if (row.role === 'user') {
    const localIndex = rows.findIndex(
      existing => existing.role === 'user' && existing.turnId.startsWith('local-') && existing.content === row.content,
    )
    if (localIndex >= 0) {
      const next = rows.slice()
      next[localIndex] = row
      return next
    }
  }
  return [...rows, row]
}

/**
 * Pure state transition.
 * @param state - previous state.
 * @param action - the transition.
 * @returns the next state (reference-equal when nothing changed).
 */
export function reduce(state: ConsoleState, action: Action): ConsoleState {
  switch (action.type) {
    case 'open_panel':
      if (state.open && state.badge === 0) return state
      return { ...state, open: true, badge: 0 }
    case 'close_panel':
      if (!state.open) return state
      return { ...state, open: false }
    case 'toggle_panel':
      return state.open ? { ...state, open: false } : { ...state, open: true, badge: 0 }
    case 'back':
      if (state.stack.length > 1) return { ...state, stack: state.stack.slice(0, -1) }
      return state.open ? { ...state, open: false } : state
    case 'show_tree':
      return { ...state, stack: [{ kind: 'tree' }] }
    case 'open_chat': {
      // A non-tree top (a previous chat, or the spawn form that just created
      // this session) is replaced: Esc from the new chat returns to the tree.
      const top = state.stack[state.stack.length - 1]
      const stack = top !== undefined && top.kind !== 'tree'
        ? [...state.stack.slice(0, -1), { kind: 'chat', sid: action.sid } as View]
        : [...state.stack, { kind: 'chat', sid: action.sid } as View]
      return { ...state, stack, recents: pushRecent(state.recents, action.sid) }
    }
    case 'open_spawn': {
      const top = state.stack[state.stack.length - 1]
      if (top !== undefined && top.kind === 'spawn') return state
      return { ...state, stack: [...state.stack, { kind: 'spawn' }], spawn: { busy: false, error: null } }
    }
    case 'set_width':
      return { ...state, width: clampWidth(action.width) }
    case 'toggle_project':
      return {
        ...state,
        collapsed: { ...state.collapsed, [action.slug]: !state.collapsed[action.slug] },
      }
    case 'status_loaded': {
      const phase: ConnectionPhase = action.status.connected
        ? 'ok'
        : action.status.reason === 'unconfigured' ? 'unconfigured' : 'unreachable'
      return { ...state, connection: { phase, vendors: action.status.vendors ?? [] } }
    }
    case 'status_failed':
      return { ...state, connection: { ...state.connection, phase: 'unreachable' } }
    case 'graph_loading':
      return { ...state, graphLoading: true, graphError: null }
    case 'graph_loaded':
      return { ...state, graph: action.graph, graphLoading: false, graphStale: false, graphError: null }
    case 'graph_failed':
      return { ...state, graphLoading: false, graphError: action.message }
    case 'graph_stale':
      return state.graphStale ? state : { ...state, graphStale: true }
    case 'turn_done': {
      const badged = state.open ? state : { ...state, badge: state.badge + 1 }
      if (action.sid === undefined) return badged
      const chat = chatOf(badged, action.sid)
      if (chat.activity === undefined || chat.activity === 'working') {
        return withChat(badged, action.sid, { ...chat, activity: 'idle' })
      }
      return badged
    }
    case 'history_loading':
      return withChat(state, action.sid, { ...chatOf(state, action.sid), loading: true, error: null })
    case 'history_loaded': {
      const chat = chatOf(state, action.sid)
      const locals = chat.rows.filter(row => row.turnId.startsWith('local-'))
      const rows = locals.reduce(mergeRow, action.rows.slice())
      return withChat(state, action.sid, { ...chat, rows, loading: false, error: null })
    }
    case 'history_failed':
      return withChat(state, action.sid, {
        ...chatOf(state, action.sid),
        loading: false,
        error: action.message,
      })
    case 'event_row': {
      const chat = chatOf(state, action.sid)
      const rows = mergeRow(chat.rows, action.row)
      const notices = chat.notices.filter(notice => notice.kind !== 'queued')
      if (rows === chat.rows && notices.length === chat.notices.length) return state
      return withChat(state, action.sid, { ...chat, rows, notices })
    }
    case 'activity': {
      const chat = chatOf(state, action.sid)
      if (chat.activity === action.activity) return state
      return withChat(state, action.sid, { ...chat, activity: action.activity })
    }
    case 'send_started': {
      const chat = chatOf(state, action.sid)
      const row: TranscriptRow = { turnId: `local-${state.nextLocalId}`, role: 'user', content: action.text }
      return {
        ...withChat(state, action.sid, { ...chat, rows: [...chat.rows, row], notices: [] }),
        nextLocalId: state.nextLocalId + 1,
      }
    }
    case 'send_settled': {
      const chat = chatOf(state, action.sid)
      const receipt = action.receipt
      if (receipt.ok && receipt.queued !== true) return state
      const notice: ChatNotice = receipt.ok
        ? {
            id: state.nextNoticeId,
            kind: 'queued',
            ...(receipt.queuedBehind !== undefined ? { queuedBehind: receipt.queuedBehind } : {}),
          }
        : {
            id: state.nextNoticeId,
            kind: 'error',
            ...(receipt.errorKind !== undefined ? { errorKind: receipt.errorKind } : {}),
            ...(receipt.error !== undefined ? { message: receipt.error } : {}),
          }
      return {
        ...withChat(state, action.sid, { ...chat, notices: [...chat.notices, notice] }),
        nextNoticeId: state.nextNoticeId + 1,
      }
    }
    case 'send_failed': {
      const chat = chatOf(state, action.sid)
      const notice: ChatNotice = { id: state.nextNoticeId, kind: 'error', message: action.message }
      return {
        ...withChat(state, action.sid, { ...chat, notices: [...chat.notices, notice] }),
        nextNoticeId: state.nextNoticeId + 1,
      }
    }
    case 'spawn_started':
      return { ...state, spawn: { busy: true, error: null } }
    case 'spawn_failed':
      return { ...state, spawn: { busy: false, error: action.message } }
    case 'spawn_done':
      return { ...state, spawn: { busy: false, error: null }, graphStale: true }
    case 'set_spawn_project':
      if (state.spawnProject === action.project) return state
      return { ...state, spawnProject: action.project }
  }
}

/**
 * The panel's one store: a bare observable source (getSnapshot + subscribe —
 * the `HostObservable` shape the DSH slot framework binds into a `useConsole`
 * selector hook for every slot component) plus its single write path.
 */
export interface ConsoleStore {
  getSnapshot(): ConsoleState
  dispatch(action: Action): void
  subscribe(listener: () => void): () => void
}

/**
 * Create the store.
 * @param initial - starting state (initialState(loadPersisted(...)) in production).
 * @returns the store.
 */
export function createStore(initial: ConsoleState): ConsoleStore {
  let state = initial
  const listeners = new Set<() => void>()
  return {
    getSnapshot: () => state,
    dispatch(action) {
      const next = reduce(state, action)
      if (next === state) return
      state = next
      for (const listener of [...listeners]) listener()
    },
    subscribe(listener) {
      listeners.add(listener)
      return () => {
        listeners.delete(listener)
      }
    },
  }
}

/** The storage face persistence needs (localStorage-compatible). */
export interface StorageLike {
  getItem(key: string): string | null
  setItem(key: string, value: string): void
}

/**
 * Read the persisted slice.
 * @param storage - storage to read (undefined = nothing persisted).
 * @returns the persisted slice (empty on any read/parse failure — storage may
 * be unavailable or poisoned, and the panel must still boot).
 */
export function loadPersisted(storage: StorageLike | undefined): Persisted {
  if (storage === undefined) return {}
  try {
    const persisted: Persisted = {}
    if (storage.getItem(STORAGE_KEYS.open) === '1') persisted.open = true
    const width = Number(storage.getItem(STORAGE_KEYS.width))
    if (Number.isFinite(width) && width > 0) persisted.width = width
    const recentsRaw = storage.getItem(STORAGE_KEYS.recents)
    if (recentsRaw !== null) {
      const parsed: unknown = JSON.parse(recentsRaw)
      if (Array.isArray(parsed)) persisted.recents = parsed.filter((s): s is string => typeof s === 'string')
    }
    const project = storage.getItem(STORAGE_KEYS.project)
    if (project !== null && project !== '') persisted.project = project
    return persisted
  } catch {
    // Swallows storage/JSON failures: private-mode denials or corrupt values
    // must not keep the panel from booting with defaults.
    return {}
  }
}

/**
 * Mirror open/width/recents into storage on every relevant change.
 * @param store - the store to observe.
 * @param storage - target storage.
 * @returns unsubscribe.
 */
export function attachPersistence(store: ConsoleStore, storage: StorageLike): () => void {
  let last = store.getSnapshot()
  const write = (state: ConsoleState): void => {
    try {
      storage.setItem(STORAGE_KEYS.open, state.open ? '1' : '0')
      storage.setItem(STORAGE_KEYS.width, String(state.width))
      storage.setItem(STORAGE_KEYS.recents, JSON.stringify(state.recents))
      if (state.spawnProject !== null) storage.setItem(STORAGE_KEYS.project, state.spawnProject)
    } catch {
      // Swallows quota/private-mode write failures: persistence is a
      // convenience, never worth breaking the live panel over.
    }
  }
  return store.subscribe(() => {
    const state = store.getSnapshot()
    if (
      state.open === last.open
      && state.width === last.width
      && state.recents === last.recents
      && state.spawnProject === last.spawnProject
    ) {
      last = state
      return
    }
    last = state
    write(state)
  })
}

/** One flattened, depth-annotated tree row. */
export interface FlatNode {
  node: TeamNode
  depth: number
}

/**
 * Flatten a project's node forest into render order (parents before their
 * delegation children, depth for indentation).
 * @param nodes - the project's root nodes.
 * @returns depth-annotated rows.
 */
export function flattenNodes(nodes: readonly TeamNode[]): FlatNode[] {
  const rows: FlatNode[] = []
  const visit = (node: TeamNode, depth: number): void => {
    rows.push({ node, depth })
    for (const child of node.children) visit(child, Math.min(depth + 1, 6))
  }
  for (const node of nodes) visit(node, 0)
  return rows
}

/**
 * Map contract activity onto the StateDot state vocabulary.
 * @param activity - session activity.
 * @returns the StateDot state (working animates, idle is settled-green,
 * stale warns, stuck is the error red).
 */
export function dotState(activity: Activity | undefined): 'ongoing' | 'done' | 'warning' | 'error' {
  switch (activity) {
    case 'working':
      return 'ongoing'
    case 'stale':
      return 'warning'
    case 'stuck':
      return 'error'
    case 'idle':
    case undefined:
      return 'done'
  }
}

/**
 * Compact cost text for tree rows.
 * @param costUsd - accumulated cost.
 * @returns `$x.xx` (two decimals under $10, one above), or null when unknown.
 */
export function formatCost(costUsd: number | undefined): string | null {
  if (costUsd === undefined || !Number.isFinite(costUsd)) return null
  if (costUsd >= 10) return `$${costUsd.toFixed(1)}`
  return `$${costUsd.toFixed(2)}`
}

/**
 * Find a node by sid anywhere in the graph.
 * @param graph - the team graph (null while unloaded).
 * @param sid - session id.
 * @returns the node, or undefined.
 */
export function findNode(graph: TeamGraph | null, sid: string): TeamNode | undefined {
  if (graph === null) return undefined
  const visit = (nodes: readonly TeamNode[]): TeamNode | undefined => {
    for (const node of nodes) {
      if (node.sid === sid) return node
      const hit = visit(node.children)
      if (hit !== undefined) return hit
    }
    return undefined
  }
  for (const project of graph.projects) {
    const hit = visit(project.nodes)
    if (hit !== undefined) return hit
  }
  return undefined
}

/**
 * Two-letter vendor monogram for the tree glyph (text, never a brand mark).
 * @param vendor - vendor id.
 * @returns the glyph text.
 */
export function vendorGlyph(vendor: string): string {
  return vendor.slice(0, 2)
}

/**
 * Known project slugs, in graph order (the spawn form's choices).
 * @param graph - the team graph (null while unloaded).
 * @returns slugs (empty while the graph is unknown).
 */
export function projectSlugs(graph: TeamGraph | null): string[] {
  return graph === null ? [] : graph.projects.map(project => project.slug)
}

/** What the panel does with one SpawnResponse. */
export type SpawnOutcome =
  /** A session exists upstream — enter its chat (surfacing the error there when the first task failed). */
  | { kind: 'chat'; sid: string; errorMessage?: string }
  /** Nothing was created — keep the form up with the (actionable) error. */
  | { kind: 'form_error'; message: string }

/**
 * Decide the navigation for a spawn response. A sid may be present even on
 * `ok: false` (the session spawned but its first task failed) — the session
 * is real, so the user lands in its chat with the error stated there.
 * @param response - the SpawnResponse.
 * @returns the outcome plan.
 */
export function planSpawnOutcome(response: { ok: boolean; sid?: string; error?: string }): SpawnOutcome {
  if (response.sid !== undefined && response.sid !== '') {
    if (response.ok) return { kind: 'chat', sid: response.sid }
    return { kind: 'chat', sid: response.sid, errorMessage: response.error ?? 'unknown' }
  }
  return { kind: 'form_error', message: response.error ?? 'unknown' }
}
