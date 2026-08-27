/**
 * One hand-rolled store for the whole workbench: open state, connection,
 * catalogs (projects / models / roles), the team graph, the current
 * selection, per-sid chats (canonical rows + the in-flight live turn), the
 * details column, the spawn draft, the badge counter and recents. State
 * transitions are pure reducer functions (the unit-test surface); the store
 * wrapper adds subscribe/getSnapshot for useSyncExternalStore, and the
 * persistence attachment mirrors a few preferences into localStorage under
 * `ccteam.console.*`.
 */
import type {
  Activity,
  AttachmentRef,
  ChoiceOption,
  ModelsCatalog,
  ProjectInfo,
  SendReceipt,
  SessionEvent,
  SessionStatus,
  StatusResponse,
  Step,
  TeamGraph,
  TeamNode,
  TranscriptRow,
  TurnUsage,
  VendorAvailability,
} from '../shared/contract.js'

/** The seven spawnable vendors, in display order. */
export const VENDORS: readonly string[] = ['claude', 'codex', 'grok', 'opencode', 'kimi', 'pi', 'dsh']

/** Recents strip capacity. */
export const MAX_RECENTS = 5

/** localStorage keys (decree: everything under `ccteam.console.*`). */
export const STORAGE_KEYS = {
  open: 'ccteam.console.open',
  recents: 'ccteam.console.recents',
  project: 'ccteam.console.project',
  vendor: 'ccteam.console.vendor',
  team: 'ccteam.console.team',
  details: 'ccteam.console.details',
} as const

/** Connection phase driving the state screens and the entry dot. */
export type ConnectionPhase = 'checking' | 'ok' | 'unreachable' | 'unconfigured'

/** What the main column shows. */
export type Selection =
  | { kind: 'none' }
  | { kind: 'new' }
  | { kind: 'session'; sid: string }

/** Inline chat notice derived from a receipt or a stream event (honest, never swallowed). */
export interface ChatNotice {
  id: number
  kind: 'queued' | 'error' | 'info'
  queuedBehind?: string
  errorKind?: string
  message?: string
}

/** One rendered transcript row. */
export type ChatRow =
  | { kind: 'user'; id: string; content: string; ts?: string; attachments?: AttachmentRef[]; local?: boolean }
  | {
    kind: 'assistant'
    id: string
    content: string
    ts?: string
    steps: Step[]
    attachments?: AttachmentRef[]
    usage?: TurnUsage
    /** Settled from the live stream; replaced by its canonical row on the next history load. */
    ephemeral?: boolean
  }
  | {
    kind: 'choice'
    id: string
    content: string
    options: ChoiceOption[]
    token: string
    resolved?: string
    resolving?: boolean
    error?: string
  }
  | { kind: 'system'; id: string; text: string; tone: 'info' | 'warn' | 'error' }

/** The in-flight assistant turn (narrative snapshot + structured steps). */
export interface LiveTurn {
  id: string
  content: string
  steps: Step[]
  startedAt: number
}

/** Per-sid chat state. */
export interface ChatState {
  rows: ChatRow[]
  live: LiveTurn | null
  activity: Activity | undefined
  /** A choice prompt is pending (the working indicator yields to it). */
  waiting: boolean
  loading: boolean
  loadingOlder: boolean
  error: string | null
  notices: ChatNotice[]
  nextBefore?: string
  hasMore: boolean
  status: SessionStatus | null
}

/** The spawn draft (persisted project/vendor so the next spawn starts there). */
export interface SpawnDraft {
  project: string | null
  vendor: string | null
  model: string | null
  effort: string | null
  role: string | null
}

/** Whole-workbench state. */
export interface ConsoleState {
  open: boolean
  connection: { phase: ConnectionPhase; vendors: VendorAvailability[] }
  graph: TeamGraph | null
  graphLoading: boolean
  graphStale: boolean
  graphError: string | null
  catalogs: {
    projects: ProjectInfo[] | null
    models: ModelsCatalog | null
    roles: Record<string, string[]>
  }
  selection: Selection
  filter: string
  collapsed: Record<string, boolean>
  recents: string[]
  chats: Record<string, ChatState>
  teamOpen: boolean
  details: { open: boolean; step: { sid: string; itemId: string } | null }
  spawn: { busy: boolean; error: string | null; draft: SpawnDraft }
  badge: number
  nextNoticeId: number
  nextLocalId: number
}

/** Every state transition. */
export type Action =
  | { type: 'open_panel' }
  | { type: 'close_panel' }
  | { type: 'toggle_panel' }
  | { type: 'select_session'; sid: string }
  | { type: 'select_new' }
  | { type: 'clear_selection' }
  | { type: 'set_filter'; filter: string }
  | { type: 'toggle_project'; slug: string }
  | { type: 'toggle_team' }
  | { type: 'toggle_details' }
  | { type: 'open_details' }
  | { type: 'close_details' }
  | { type: 'select_step'; sid: string; itemId: string }
  | { type: 'status_loaded'; status: StatusResponse }
  | { type: 'status_failed' }
  | { type: 'graph_loading' }
  | { type: 'graph_loaded'; graph: TeamGraph }
  | { type: 'graph_failed'; message: string }
  | { type: 'graph_stale' }
  | { type: 'projects_loaded'; projects: ProjectInfo[] }
  | { type: 'models_loaded'; models: ModelsCatalog }
  | { type: 'roles_loaded'; project: string; roles: string[] }
  | { type: 'set_draft'; draft: Partial<SpawnDraft> }
  | { type: 'spawn_started' }
  | { type: 'spawn_failed'; message: string }
  | { type: 'spawn_done' }
  | { type: 'turn_done'; sid?: string }
  | { type: 'history_loading'; sid: string; older?: boolean }
  | { type: 'history_loaded'; sid: string; rows: TranscriptRow[]; nextBefore?: string; hasMore: boolean; older?: boolean }
  | { type: 'history_failed'; sid: string; message: string }
  | { type: 'session_event'; sid: string; event: SessionEvent; now: number }
  | { type: 'session_status'; sid: string; status: SessionStatus }
  | { type: 'send_started'; sid: string; text: string; attachments?: AttachmentRef[] }
  | { type: 'send_settled'; sid: string; receipt: SendReceipt }
  | { type: 'send_failed'; sid: string; message: string }
  | { type: 'notice'; sid: string; kind: 'info' | 'error'; message: string }
  | { type: 'choice_resolving'; sid: string; id: string }
  | { type: 'choice_resolved'; sid: string; id: string; selection: string }
  | { type: 'choice_failed'; sid: string; id: string; message: string }
  | { type: 'delegation'; parentSid?: string; childSid?: string; relation: string; title?: string; reason?: string }

/** Persisted slice loaded at store creation. */
export interface Persisted {
  open?: boolean
  recents?: string[]
  project?: string
  vendor?: string
  teamOpen?: boolean
  detailsOpen?: boolean
}

/**
 * Build the initial state, folding in the persisted slice.
 * @param persisted - previously stored preferences.
 * @returns the initial workbench state.
 */
export function initialState(persisted: Persisted = {}): ConsoleState {
  return {
    open: persisted.open ?? false,
    connection: { phase: 'checking', vendors: [] },
    graph: null,
    graphLoading: false,
    graphStale: true,
    graphError: null,
    catalogs: { projects: null, models: null, roles: {} },
    selection: { kind: 'none' },
    filter: '',
    collapsed: {},
    recents: (persisted.recents ?? []).filter(sid => typeof sid === 'string').slice(0, MAX_RECENTS),
    chats: {},
    teamOpen: persisted.teamOpen ?? true,
    details: { open: persisted.detailsOpen ?? false, step: null },
    spawn: {
      busy: false,
      error: null,
      draft: {
        project: persisted.project ?? null,
        vendor: persisted.vendor ?? null,
        model: null,
        effort: null,
        role: null,
      },
    },
    badge: 0,
    nextNoticeId: 1,
    nextLocalId: 1,
  }
}

const EMPTY_CHAT: ChatState = {
  rows: [],
  live: null,
  activity: undefined,
  waiting: false,
  loading: false,
  loadingOlder: false,
  error: null,
  notices: [],
  hasMore: false,
  status: null,
}

/**
 * The chat state of one sid (an empty one when never opened).
 * @param state - workbench state.
 * @param sid - session id.
 * @returns the chat state.
 */
export function chatOf(state: ConsoleState, sid: string): ChatState {
  return state.chats[sid] ?? EMPTY_CHAT
}

function withChat(state: ConsoleState, sid: string, chat: ChatState): ConsoleState {
  return { ...state, chats: { ...state.chats, [sid]: chat } }
}

function pushRecent(recents: string[], sid: string): string[] {
  return [sid, ...recents.filter(existing => existing !== sid)].slice(0, MAX_RECENTS)
}

function withNotice(state: ConsoleState, sid: string, notice: Omit<ChatNotice, 'id'>): ConsoleState {
  const chat = chatOf(state, sid)
  return {
    ...withChat(state, sid, { ...chat, notices: [...chat.notices, { id: state.nextNoticeId, ...notice }] }),
    nextNoticeId: state.nextNoticeId + 1,
  }
}

/**
 * Map one history row onto a rendered row.
 * @param row - transcript row.
 * @returns the chat row.
 */
export function rowFromTranscript(row: TranscriptRow): ChatRow {
  if (row.role === 'user') {
    return {
      kind: 'user',
      id: row.turnId,
      content: row.content,
      ...(row.ts === undefined ? {} : { ts: row.ts }),
      ...(row.attachments === undefined ? {} : { attachments: row.attachments }),
    }
  }
  return {
    kind: 'assistant',
    id: row.turnId,
    content: row.content,
    steps: [],
    ...(row.ts === undefined ? {} : { ts: row.ts }),
    ...(row.attachments === undefined ? {} : { attachments: row.attachments }),
    ...(row.usage === undefined ? {} : { usage: row.usage }),
  }
}

/**
 * Reconcile a freshly loaded canonical page with what the chat already
 * shows: optimistic local user rows whose text arrived drop out; ephemeral
 * assistant rows settled from the stream hand their steps to the canonical
 * row carrying the same text and drop out; choice/system rows survive after
 * the canonical rows.
 */
function reconcile(existing: ChatRow[], canonical: ChatRow[]): ChatRow[] {
  const rows = canonical.slice()
  const canonicalUserTexts = new Set(rows.filter(r => r.kind === 'user').map(r => r.content))
  const lastAssistantIndex = (() => {
    for (let i = rows.length - 1; i >= 0; i -= 1) if (rows[i]!.kind === 'assistant') return i
    return -1
  })()
  const tail: ChatRow[] = []
  for (const row of existing) {
    if (row.kind === 'user') {
      if (row.local === true && !canonicalUserTexts.has(row.content)) tail.push(row)
      continue
    }
    if (row.kind === 'assistant') {
      if (row.ephemeral !== true) continue
      const target = rows.findIndex(r => r.kind === 'assistant' && r.content === row.content)
      if (target !== -1) {
        const canon = rows[target]!
        if (canon.kind === 'assistant' && row.steps.length > 0 && canon.steps.length === 0) {
          rows[target] = { ...canon, steps: row.steps }
        }
        continue
      }
      if (lastAssistantIndex !== -1 && row.steps.length > 0) {
        const canon = rows[lastAssistantIndex]!
        if (canon.kind === 'assistant' && canon.steps.length === 0 && row.content === '') {
          rows[lastAssistantIndex] = { ...canon, steps: row.steps }
          continue
        }
      }
      tail.push(row)
      continue
    }
    if (row.kind === 'choice' && row.resolved !== undefined) continue
    tail.push(row)
  }
  return [...rows, ...tail]
}

/** Lifecycle states worth a transcript row. */
const NARRATED_LIFECYCLE: ReadonlySet<string> = new Set(['stopped', 'evicted', 'failed', 'crashed', 'exited', 'resumed', 'interrupted'])
/** Lifecycle states after which nothing is running. */
const ENDED_LIFECYCLE: ReadonlySet<string> = new Set(['stopped', 'evicted', 'failed', 'crashed', 'exited'])

function upsertStep(steps: Step[], step: Step): Step[] {
  const at = steps.findIndex(s => s.itemId === step.itemId)
  if (at === -1) return [...steps, step]
  const next = steps.slice()
  next[at] = { ...next[at]!, ...step }
  return next
}

function completeSteps(steps: Step[]): Step[] {
  return steps.every(s => s.status === 'completed')
    ? steps
    : steps.map(s => (s.status === 'completed' ? s : { ...s, status: 'completed' }))
}

function applySessionEvent(chat: ChatState, action: Extract<Action, { type: 'session_event' }>): ChatState {
  const event = action.event
  switch (event.kind) {
    case 'progress': {
      const live: LiveTurn = chat.live ?? { id: `live-${action.now}`, content: '', steps: [], startedAt: action.now }
      const nextLive: LiveTurn = {
        ...live,
        content: event.content !== '' ? event.content : live.content,
        steps: event.done ? completeSteps(live.steps) : live.steps,
      }
      return { ...chat, live: nextLive, activity: event.done ? chat.activity : 'working', waiting: false }
    }
    case 'activity': {
      const live: LiveTurn = chat.live ?? { id: `live-${action.now}`, content: '', steps: [], startedAt: action.now }
      return {
        ...chat,
        live: { ...live, steps: upsertStep(live.steps, event.step) },
        activity: 'working',
        waiting: false,
      }
    }
    case 'answer': {
      const notices = chat.notices.filter(notice => notice.kind !== 'queued')
      if (event.options !== undefined && event.options.length > 0 && event.token !== undefined) {
        const row: ChatRow = {
          kind: 'choice',
          id: `choice-${event.id}`,
          content: event.content,
          options: event.options,
          token: event.token,
        }
        if (chat.rows.some(r => r.id === row.id)) return chat
        return { ...chat, rows: [...chat.rows, row], notices, activity: 'idle', waiting: true }
      }
      const steps = chat.live === null ? [] : completeSteps(chat.live.steps)
      const settled: ChatRow = {
        kind: 'assistant',
        id: `answer-${event.id}`,
        content: event.content,
        steps,
        ephemeral: true,
        ...(event.ts === undefined ? {} : { ts: event.ts }),
        ...(event.attachments === undefined ? {} : { attachments: event.attachments }),
      }
      if (chat.rows.some(r => r.id === settled.id)) return { ...chat, live: null, activity: 'idle', waiting: false, notices }
      return { ...chat, rows: [...chat.rows, settled], live: null, activity: 'idle', waiting: false, notices }
    }
    case 'lifecycle': {
      // Only transitions a reader must know about become rows; bookkeeping
      // states (renamed, spawned, …) reshape the tree without narration.
      if (!NARRATED_LIFECYCLE.has(event.state)) return chat
      const row: ChatRow = {
        kind: 'system',
        id: `lifecycle-${action.now}`,
        text: event.reason === undefined ? event.state : `${event.state} · ${event.reason}`,
        tone: event.state === 'failed' || event.state === 'crashed' ? 'error' : 'info',
      }
      const ended = ENDED_LIFECYCLE.has(event.state)
      return {
        ...chat,
        rows: [...chat.rows, row],
        ...(ended ? { live: null, activity: 'idle' as Activity, waiting: false } : {}),
      }
    }
  }
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
    case 'select_session': {
      const same = state.selection.kind === 'session' && state.selection.sid === action.sid
      if (same) return state
      return {
        ...state,
        selection: { kind: 'session', sid: action.sid },
        recents: pushRecent(state.recents, action.sid),
        details: { ...state.details, step: null },
      }
    }
    case 'select_new':
      if (state.selection.kind === 'new') return state
      return { ...state, selection: { kind: 'new' }, spawn: { ...state.spawn, error: null } }
    case 'clear_selection':
      if (state.selection.kind === 'none') return state
      return { ...state, selection: { kind: 'none' } }
    case 'set_filter':
      if (state.filter === action.filter) return state
      return { ...state, filter: action.filter }
    case 'toggle_project':
      return { ...state, collapsed: { ...state.collapsed, [action.slug]: !state.collapsed[action.slug] } }
    case 'toggle_team':
      return { ...state, teamOpen: !state.teamOpen }
    case 'toggle_details':
      return { ...state, details: { ...state.details, open: !state.details.open } }
    case 'open_details':
      if (state.details.open) return state
      return { ...state, details: { ...state.details, open: true } }
    case 'close_details':
      if (!state.details.open) return state
      return { ...state, details: { ...state.details, open: false } }
    case 'select_step':
      return { ...state, details: { open: true, step: { sid: action.sid, itemId: action.itemId } } }
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
    case 'projects_loaded':
      return { ...state, catalogs: { ...state.catalogs, projects: action.projects } }
    case 'models_loaded':
      return { ...state, catalogs: { ...state.catalogs, models: action.models } }
    case 'roles_loaded':
      return { ...state, catalogs: { ...state.catalogs, roles: { ...state.catalogs.roles, [action.project]: action.roles } } }
    case 'set_draft': {
      const draft = { ...state.spawn.draft, ...action.draft }
      // A vendor switch invalidates a vendor-specific model/effort choice
      // (unless the same action sets them); a project switch, the role.
      if (action.draft.vendor !== undefined && action.draft.vendor !== state.spawn.draft.vendor) {
        if (action.draft.model === undefined) draft.model = null
        if (action.draft.effort === undefined) draft.effort = null
      }
      if (action.draft.project !== undefined && action.draft.project !== state.spawn.draft.project && action.draft.role === undefined) {
        draft.role = null
      }
      return { ...state, spawn: { ...state.spawn, draft } }
    }
    case 'spawn_started':
      return { ...state, spawn: { ...state.spawn, busy: true, error: null } }
    case 'spawn_failed':
      return { ...state, spawn: { ...state.spawn, busy: false, error: action.message } }
    case 'spawn_done':
      return { ...state, spawn: { ...state.spawn, busy: false, error: null }, graphStale: true }
    case 'turn_done': {
      const badged = state.open ? state : { ...state, badge: state.badge + 1 }
      if (action.sid === undefined) return badged
      const chat = chatOf(badged, action.sid)
      if (chat.activity === 'working' && chat.live !== null && chat.live.content === '' && chat.live.steps.length === 0) {
        return withChat(badged, action.sid, { ...chat, activity: 'idle', live: null })
      }
      if (chat.activity === 'working' && chat.live === null) {
        return withChat(badged, action.sid, { ...chat, activity: 'idle' })
      }
      return badged
    }
    case 'history_loading': {
      const chat = chatOf(state, action.sid)
      return withChat(state, action.sid, action.older === true
        ? { ...chat, loadingOlder: true }
        : { ...chat, loading: true, error: null })
    }
    case 'history_loaded': {
      const chat = chatOf(state, action.sid)
      const canonical = action.rows.map(rowFromTranscript)
      if (action.older === true) {
        const known = new Set(chat.rows.map(r => r.id))
        const fresh = canonical.filter(r => !known.has(r.id))
        return withChat(state, action.sid, {
          ...chat,
          rows: [...fresh, ...chat.rows],
          loadingOlder: false,
          hasMore: action.hasMore,
          ...(action.nextBefore === undefined ? {} : { nextBefore: action.nextBefore }),
        })
      }
      // Rows already paged in (older than this page) stay in front of the page.
      const pageIds = new Set(canonical.map(r => r.id))
      const isCanonical = (r: ChatRow): boolean =>
        (r.kind === 'user' && r.local !== true) || (r.kind === 'assistant' && r.ephemeral !== true)
      const paged = chat.rows.filter(r => isCanonical(r) && !pageIds.has(r.id))
      const merged = reconcile(chat.rows.filter(r => !paged.includes(r)), canonical)
      return withChat(state, action.sid, {
        ...chat,
        rows: [...paged, ...merged],
        loading: false,
        error: null,
        ...(chat.nextBefore === undefined || paged.length === 0
          ? { hasMore: action.hasMore, ...(action.nextBefore === undefined ? {} : { nextBefore: action.nextBefore }) }
          : {}),
      })
    }
    case 'history_failed':
      return withChat(state, action.sid, { ...chatOf(state, action.sid), loading: false, loadingOlder: false, error: action.message })
    case 'session_event': {
      const chat = chatOf(state, action.sid)
      const next = applySessionEvent(chat, action)
      return next === chat ? state : withChat(state, action.sid, next)
    }
    case 'session_status':
      return withChat(state, action.sid, { ...chatOf(state, action.sid), status: action.status })
    case 'send_started': {
      const chat = chatOf(state, action.sid)
      const row: ChatRow = {
        kind: 'user',
        id: `local-${state.nextLocalId}`,
        content: action.text,
        local: true,
        ...(action.attachments === undefined || action.attachments.length === 0 ? {} : { attachments: action.attachments }),
      }
      return {
        ...withChat(state, action.sid, { ...chat, rows: [...chat.rows, row], notices: [], waiting: false }),
        nextLocalId: state.nextLocalId + 1,
      }
    }
    case 'send_settled': {
      const receipt = action.receipt
      if (receipt.ok && receipt.queued !== true) return state
      if (receipt.ok) {
        return withNotice(state, action.sid, {
          kind: 'queued',
          ...(receipt.queuedBehind === undefined ? {} : { queuedBehind: receipt.queuedBehind }),
        })
      }
      return withNotice(state, action.sid, {
        kind: 'error',
        ...(receipt.errorKind === undefined ? {} : { errorKind: receipt.errorKind }),
        ...(receipt.error === undefined ? {} : { message: receipt.error }),
      })
    }
    case 'send_failed':
      return withNotice(state, action.sid, { kind: 'error', message: action.message })
    case 'notice':
      return withNotice(state, action.sid, { kind: action.kind, message: action.message })
    case 'choice_resolving':
    case 'choice_resolved':
    case 'choice_failed': {
      const chat = chatOf(state, action.sid)
      const at = chat.rows.findIndex(r => r.kind === 'choice' && r.id === action.id)
      if (at === -1) return state
      const row = chat.rows[at]!
      if (row.kind !== 'choice') return state
      const next: ChatRow = action.type === 'choice_resolving'
        ? { ...row, resolving: true, error: undefined }
        : action.type === 'choice_resolved'
          ? { ...row, resolving: false, resolved: action.selection, error: undefined }
          : { ...row, resolving: false, error: action.message }
      const rows = chat.rows.slice()
      rows[at] = next
      return withChat(state, action.sid, {
        ...chat,
        rows,
        ...(action.type === 'choice_resolved' ? { waiting: false, activity: 'working' as Activity } : {}),
      })
    }
    case 'delegation': {
      let next: ConsoleState = { ...state, graphStale: true }
      if (action.parentSid !== undefined && state.chats[action.parentSid] !== undefined) {
        const title = action.title ?? action.childSid ?? ''
        const message = action.relation === 'spawned'
          ? `spawned:${title}`
          : action.relation === 'failed'
            ? `failed:${title}:${action.reason ?? ''}`
            : `done:${title}`
        next = withNotice(next, action.parentSid, { kind: 'info', message })
      }
      return next
    }
  }
}

/**
 * The workbench's one store: a bare observable source (getSnapshot +
 * subscribe — the `HostObservable` shape the DSH slot framework binds into a
 * `useConsole` selector hook for every slot component) plus its single
 * write path.
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
 * be unavailable or poisoned, and the workbench must still boot).
 */
export function loadPersisted(storage: StorageLike | undefined): Persisted {
  if (storage === undefined) return {}
  try {
    const persisted: Persisted = {}
    if (storage.getItem(STORAGE_KEYS.open) === '1') persisted.open = true
    const recentsRaw = storage.getItem(STORAGE_KEYS.recents)
    if (recentsRaw !== null) {
      const parsed: unknown = JSON.parse(recentsRaw)
      if (Array.isArray(parsed)) persisted.recents = parsed.filter((s): s is string => typeof s === 'string')
    }
    const project = storage.getItem(STORAGE_KEYS.project)
    if (project !== null && project !== '') persisted.project = project
    const vendor = storage.getItem(STORAGE_KEYS.vendor)
    if (vendor !== null && vendor !== '') persisted.vendor = vendor
    const team = storage.getItem(STORAGE_KEYS.team)
    if (team === '0') persisted.teamOpen = false
    const details = storage.getItem(STORAGE_KEYS.details)
    if (details === '1') persisted.detailsOpen = true
    return persisted
  } catch {
    // Swallows storage/JSON failures: private-mode denials or corrupt values
    // must not keep the workbench from booting with defaults.
    return {}
  }
}

/**
 * Mirror preferences into storage on every relevant change.
 * @param store - the store to observe.
 * @param storage - target storage.
 * @returns unsubscribe.
 */
export function attachPersistence(store: ConsoleStore, storage: StorageLike): () => void {
  let last = store.getSnapshot()
  const write = (state: ConsoleState): void => {
    try {
      storage.setItem(STORAGE_KEYS.open, state.open ? '1' : '0')
      storage.setItem(STORAGE_KEYS.recents, JSON.stringify(state.recents))
      if (state.spawn.draft.project !== null) storage.setItem(STORAGE_KEYS.project, state.spawn.draft.project)
      if (state.spawn.draft.vendor !== null) storage.setItem(STORAGE_KEYS.vendor, state.spawn.draft.vendor)
      storage.setItem(STORAGE_KEYS.team, state.teamOpen ? '1' : '0')
      storage.setItem(STORAGE_KEYS.details, state.details.open ? '1' : '0')
    } catch {
      // Swallows quota/private-mode write failures: persistence is a
      // convenience, never worth breaking the live workbench over.
    }
  }
  return store.subscribe(() => {
    const state = store.getSnapshot()
    if (
      state.open === last.open
      && state.recents === last.recents
      && state.spawn.draft.project === last.spawn.draft.project
      && state.spawn.draft.vendor === last.spawn.draft.vendor
      && state.teamOpen === last.teamOpen
      && state.details.open === last.details.open
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
 * Filter a flattened project forest by a free-text query (title, sid,
 * vendor, model, role — case-insensitive). Ancestors of a match are kept so
 * the tree shape survives.
 * @param rows - flattened rows.
 * @param query - the filter text (blank = everything).
 * @returns the matching rows.
 */
export function filterRows(rows: FlatNode[], query: string): FlatNode[] {
  const needle = query.trim().toLowerCase()
  if (needle === '') return rows
  const matches = (node: TeamNode): boolean =>
    [node.title, node.sid, node.vendor, node.model, node.role]
      .some(field => field !== undefined && field.toLowerCase().includes(needle))
  const keep = new Set<string>()
  const visit = (node: TeamNode): boolean => {
    let hit = matches(node)
    for (const child of node.children) if (visit(child)) hit = true
    if (hit) keep.add(node.sid)
    return hit
  }
  for (const row of rows) if (row.depth === 0) visit(row.node)
  return rows.filter(row => keep.has(row.node.sid))
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
 * Known project slugs, in graph order.
 * @param graph - the team graph (null while unloaded).
 * @returns slugs (empty while the graph is unknown).
 */
export function projectSlugs(graph: TeamGraph | null): string[] {
  return graph === null ? [] : graph.projects.map(project => project.slug)
}

/** What the workbench does with one SpawnResponse. */
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

/**
 * The effort ladder for a vendor/model pair from the catalog (model-level
 * ladder wins, else the vendor ladder).
 * @param catalog - models catalog (null while unloaded).
 * @param vendor - vendor id.
 * @param model - chosen model id (null = vendor default).
 * @returns the efforts (empty = no effort axis known).
 */
export function effortsFor(catalog: ModelsCatalog | null, vendor: string | null, model: string | null): string[] {
  if (catalog === null || vendor === null) return []
  const row = catalog.vendors.find(v => v.vendor === vendor)
  if (row === undefined) return []
  if (model !== null) {
    const entry = row.models.find(m => m.id === model)
    if (entry !== undefined && entry.efforts.length > 0) return entry.efforts
  }
  return row.efforts
}
