/**
 * The host half's backend-for-frontend: ONE prefix route under
 * `API_PREFIX` that answers the browser in the shapes of
 * `src/shared/contract.ts` and speaks ccteam's REST/SSE API upstream.
 *
 * Why a BFF at all rather than letting the panel call ccteam directly:
 * ccteam-web installs no CORS layer (crates/ccteam-web/src/lib.rs — only
 * compression + metrics), so a browser simply cannot reach it cross-origin.
 * Proxying server-side is the only shape that works, and it is also what keeps
 * the credential out of the browser.
 *
 * CREDENTIAL HYGIENE (red line): the REST token is read from plugin config /
 * settings through a closure and used only to build an `Authorization` header.
 * It is never written to `process.env`, never logged, and never placed in any
 * response body or SSE frame.
 *
 * Upstream surface, verified against crates/ccteam-web (see each mapper):
 *   GET  /api/v1/capabilities                → vendor availability probe
 *   GET  /api/v1/agents/graph                → delegation tree
 *   GET  /api/v1/agents/events               → global SSE
 *   GET  /api/v1/sessions/{sid}              → transcript page
 *   GET  /api/v1/sessions/{sid}/events       → per-session SSE
 *   POST /api/v1/sessions/{sid}/turn         → submit a user turn
 *   POST /api/v1/projects/{slug}/sessions    → create a session
 *   PATCH /api/v1/sessions/{sid}             → set the title
 */
import type { IncomingMessage, ServerResponse } from 'node:http'
import {
  API_PREFIX,
  type Activity,
  type ApiMethod,
  type HistoryRequest,
  type HistoryResponse,
  type PanelEvent,
  type SendReceipt,
  type SendRequest,
  type SpawnRequest,
  type SpawnResponse,
  type StatusResponse,
  type TeamGraph,
  type TeamNode,
  type TranscriptRow,
  type VendorAvailability,
} from './shared/contract.js'
import { SseHub, type SseFrame, type UpstreamSource } from './sse.js'

export type FetchLike = (input: string, init?: RequestInit) => Promise<Response>

const API_METHODS: ReadonlySet<string> = new Set<ApiMethod>([
  'status',
  'team.graph',
  'session.history',
  'session.send',
  'session.spawn',
])

/** Proxies drop idle streams; DSH's own keepalive is 15s, ours is 25s. */
const DEFAULT_HEARTBEAT_MS = 25_000
const DEFAULT_HISTORY_LIMIT = 200

export interface BffOptions {
  /** Read live so a settings edit takes effect without a restart. */
  daemonUrl: () => string
  restToken: () => string
  /** Project a spawn lands in when the request does not name one. */
  defaultProject?: () => string
  fetchImpl?: FetchLike
  logger?: { warn(message: string): void }
  heartbeatMs?: number
  retryBaseMs?: number
  retryMaxMs?: number
  sleep?: (ms: number, signal: AbortSignal) => Promise<void>
}

export interface WebServerLike {
  register(route: {
    kind: 'exact' | 'prefix'
    path: string
    handler: (req: IncomingMessage, res: ServerResponse) => void | Promise<void>
  }): () => void
}

export interface BffContext {
  webServer: WebServerLike
  effect?<T extends (() => void | Promise<void>) | void>(setup: () => T, label?: string): () => void
  logger?: { warn(message: string): void }
}

/** An upstream call outcome, kept separate from HTTP status juggling. */
type Upstream =
  | { kind: 'ok'; status: number; body: unknown }
  | { kind: 'http'; status: number; error: string; code?: string }
  | { kind: 'network'; error: string }

export interface Bff {
  handle(req: IncomingMessage, res: ServerResponse): Promise<void>
  close(): void
  /** Live upstream connection count — the fan-out invariant under test. */
  upstreamCount(): number
}

/**
 * Build the BFF. `fetchImpl` is injected rather than reached for globally so
 * the whole surface is testable with zero network.
 */
export function createBff(options: BffOptions): Bff {
  const doFetch: FetchLike = options.fetchImpl ?? ((input, init) => fetch(input, init))
  const heartbeatMs = options.heartbeatMs ?? DEFAULT_HEARTBEAT_MS
  const hub = new SseHub({
    retryBaseMs: options.retryBaseMs,
    retryMaxMs: options.retryMaxMs,
    logger: options.logger,
    sleep: options.sleep,
  })

  const base = (): string => options.daemonUrl().trim().replace(/\/+$/, '')

  /**
   * ccteam presents web tokens as `Authorization: Bearer ccteam:<hex>` and
   * rejects a bare hex in that header (crates/ccteam-web/src/auth.rs
   * parse_bearer + bare_hex). A paste box gets both forms, so a token with no
   * scheme separator is normalized rather than silently 401-ing.
   */
  const authHeader = (): Record<string, string> => {
    const token = options.restToken().trim()
    if (token === '') return {}
    return { authorization: `Bearer ${token.includes(':') ? token : `ccteam:${token}`}` }
  }

  async function call(
    path: string,
    init: { method?: string; body?: unknown } = {},
  ): Promise<Upstream> {
    const headers: Record<string, string> = { accept: 'application/json', ...authHeader() }
    // Content-type is load-bearing: ccteam's FormOrJson extractor treats
    // anything but an exact `application/json` as urlencoded, which both
    // mangles nested fields and downgrades error bodies to plain text.
    if (init.body !== undefined) headers['content-type'] = 'application/json'
    let response: Response
    try {
      response = await doFetch(`${base()}${path}`, {
        method: init.method ?? 'GET',
        headers,
        ...(init.body === undefined ? {} : { body: JSON.stringify(init.body) }),
      })
    } catch (error) {
      return { kind: 'network', error: describe(error) }
    }
    const raw = await response.text().catch(() => '')
    // A 401 is `text/plain: auth required`, so parsing is guarded by content type.
    const isJson = (response.headers.get('content-type') ?? '').includes('application/json')
    let parsed: unknown
    if (isJson && raw !== '') {
      try {
        parsed = JSON.parse(raw) as unknown
      } catch {
        parsed = undefined
      }
    }
    if (!response.ok) {
      const record = asRecord(parsed)
      return {
        kind: 'http',
        status: response.status,
        error: stringOf(record?.error) ?? stringOf(record?.message) ?? raw ?? `HTTP ${response.status}`,
        ...(stringOf(record?.error_code) === undefined ? {} : { code: stringOf(record?.error_code)! }),
      }
    }
    return { kind: 'ok', status: response.status, body: parsed }
  }

  // ---------------------------------------------------------------- methods

  async function status(): Promise<StatusResponse> {
    const result = await call('/api/v1/capabilities')
    if (result.kind === 'network') return { connected: false, reason: 'unreachable' }
    if (result.kind === 'http') {
      // 401/403 is a credential problem whether the box is empty or wrong;
      // anything else means the daemon answered but is not usable.
      const unauthorized = result.status === 401 || result.status === 403
      return { connected: false, reason: unauthorized ? 'unconfigured' : 'unreachable' }
    }
    const harnesses = arrayOf(asRecord(result.body)?.harnesses)
    const vendors: VendorAvailability[] = []
    for (const entry of harnesses) {
      const row = asRecord(entry)
      const vendor = stringOf(row?.vendor)
      if (vendor === undefined) continue
      vendors.push({ vendor, installed: row?.available === true })
    }
    return vendors.length === 0 ? { connected: true } : { connected: true, vendors }
  }

  async function teamGraph(): Promise<TeamGraph> {
    const result = await call('/api/v1/agents/graph')
    if (result.kind !== 'ok') return { projects: [] }
    return buildGraph(arrayOf(asRecord(result.body)?.nodes))
  }

  async function history(request: HistoryRequest): Promise<HistoryResponse> {
    const sid = (request.sid ?? '').trim()
    if (sid === '') return { sid: '', rows: [] }
    const result = await call(
      `/api/v1/sessions/${encodeURIComponent(sid)}?limit=${DEFAULT_HISTORY_LIMIT}`,
    )
    if (result.kind !== 'ok') return { sid, rows: [] }
    const rows = transcriptRows(arrayOf(asRecord(result.body)?.events))
    if (request.since === undefined) return { sid, rows }
    const at = rows.findIndex(row => row.turnId === request.since)
    return { sid, rows: at === -1 ? rows : rows.slice(at + 1) }
  }

  async function send(request: SendRequest): Promise<SendReceipt> {
    const sid = (request.sid ?? '').trim()
    if (sid === '') return { ok: false, errorKind: 'bad_request', error: 'sid is required' }
    const result = await call(`/api/v1/sessions/${encodeURIComponent(sid)}/turn`, {
      method: 'POST',
      body: { text: request.text ?? '' },
    })
    if (result.kind !== 'ok') return failure(result)
    const body = asRecord(result.body)
    // 202 {accepted:true} delivered; the queued variant additionally carries
    // queued/queued_behind (sessions_api.rs handle_session_turn).
    if (body?.queued === true) {
      const behind = stringOf(body.queued_behind)
      return { ok: true, queued: true, ...(behind === undefined ? {} : { queuedBehind: behind }) }
    }
    return { ok: true }
  }

  async function spawn(request: SpawnRequest): Promise<SpawnResponse> {
    const slug = (request.project ?? options.defaultProject?.() ?? '').trim()
    if (slug === '') {
      return {
        ok: false,
        error: 'no project selected: choose a project for the new session '
          + '(or set one in DSH Settings → ccteam Team).',
      }
    }
    const vendor = (request.vendor ?? '').trim()
    if (vendor === '') return { ok: false, error: 'vendor is required' }
    // `role` must be PRESENT; an empty value means roleless, which is
    // ccteam's default. `host` must never appear — the key alone is a 400.
    const created = await call(`/api/v1/projects/${encodeURIComponent(slug)}/sessions`, {
      method: 'POST',
      body: {
        role: '',
        vendor,
        ...defined('model', request.model),
        ...defined('effort', request.effort),
        ...defined('mode', request.mode),
      },
    })
    if (created.kind !== 'ok') {
      const failed = failure(created)
      return { ok: false, error: failed.error ?? 'spawn failed' }
    }
    const sid = stringOf(asRecord(created.body)?.sid)
    if (sid === undefined) return { ok: false, error: 'daemon accepted the spawn but returned no sid' }

    // Neither a title nor a first task is a create-time field upstream, so
    // both are follow-ups. A failed title is cosmetic; a failed first task is
    // not, and is reported with the sid so the session is still reachable.
    const title = (request.title ?? '').trim()
    if (title !== '') {
      await call(`/api/v1/sessions/${encodeURIComponent(sid)}`, { method: 'PATCH', body: { title } })
    }
    const task = (request.task ?? '').trim()
    if (task !== '') {
      const receipt = await send({ sid, text: task })
      if (!receipt.ok) {
        return { ok: false, sid, error: `session ${sid} was created but its first task failed: ${receipt.error ?? 'unknown error'}` }
      }
    }
    return { ok: true, sid }
  }

  async function dispatch(method: string, payload: unknown): Promise<unknown> {
    const record = asRecord(payload) ?? {}
    switch (method as ApiMethod) {
      case 'status':
        return await status()
      case 'team.graph':
        return await teamGraph()
      case 'session.history':
        return await history(record as unknown as HistoryRequest)
      case 'session.send':
        return await send(record as unknown as SendRequest)
      case 'session.spawn':
        return await spawn(record as unknown as SpawnRequest)
      default:
        return undefined
    }
  }

  // -------------------------------------------------------------------- SSE

  const streamHeaders = (): Record<string, string> => ({
    accept: 'text/event-stream',
    ...authHeader(),
  })

  const globalSource: UpstreamSource = {
    open: signal => doFetch(`${base()}/api/v1/agents/events`, { headers: streamHeaders(), signal }),
    translate: frame => translateGlobal(frame),
  }

  const sessionSource = (sid: string): UpstreamSource => ({
    open: signal =>
      doFetch(`${base()}/api/v1/sessions/${encodeURIComponent(sid)}/events`, {
        headers: streamHeaders(),
        signal,
      }),
    translate: frame => translateSession(sid, frame),
  })

  function openEvents(req: IncomingMessage, res: ServerResponse, sid: string | null): void {
    res.writeHead(200, {
      'content-type': 'text/event-stream',
      'cache-control': 'no-cache, no-transform',
      connection: 'keep-alive',
      // Defeat proxy response buffering, which would hold frames back.
      'x-accel-buffering': 'no',
    })
    const emit = (payload: unknown): void => {
      if (res.writableEnded) return
      res.write(`data: ${JSON.stringify(payload)}\n\n`)
    }
    const releases: Array<() => void> = [hub.subscribe('graph', globalSource, emit)]
    if (sid !== null && sid.trim() !== '') {
      const key = `session:${sid}`
      releases.push(hub.subscribe(key, sessionSource(sid), emit))
    }
    const heartbeat = setInterval(() => {
      if (!res.writableEnded) res.write(': ping\n\n')
    }, heartbeatMs)
    heartbeat.unref?.()
    let closed = false
    const shutdown = (): void => {
      if (closed) return
      closed = true
      clearInterval(heartbeat)
      for (const release of releases) release()
      if (!res.writableEnded) res.end()
    }
    req.on('close', shutdown)
    req.on('error', shutdown)
    res.on('close', shutdown)
  }

  // ---------------------------------------------------------------- routing

  async function handle(req: IncomingMessage, res: ServerResponse): Promise<void> {
    let pathname: string
    try {
      pathname = new URL(req.url ?? '/', 'http://dsh.invalid').pathname
    } catch {
      sendJson(res, 404, { error: 'not found' })
      return
    }
    const query = new URL(req.url ?? '/', 'http://dsh.invalid').searchParams
    if (pathname !== API_PREFIX && !pathname.startsWith(`${API_PREFIX}/`)) {
      sendJson(res, 404, { error: 'not found' })
      return
    }
    const rest = pathname.slice(API_PREFIX.length)
    if (rest === '/events') {
      if ((req.method ?? 'GET').toUpperCase() !== 'GET') {
        sendJson(res, 404, { error: 'not found' })
        return
      }
      openEvents(req, res, query.get('sid'))
      return
    }
    const method = rest.startsWith('/') ? rest.slice(1) : rest
    if ((req.method ?? 'GET').toUpperCase() !== 'POST' || !API_METHODS.has(method)) {
      sendJson(res, 404, { error: `unknown method: ${method}` })
      return
    }
    try {
      sendJson(res, 200, await dispatch(method, await readJson(req)))
    } catch (error) {
      options.logger?.warn(`ccteam-team: ${method} failed: ${describe(error)}`)
      sendJson(res, 500, { error: describe(error) })
    }
  }

  return {
    handle,
    close: () => hub.close(),
    upstreamCount: () => hub.upstreamCount,
  }
}

/**
 * Register the BFF's single prefix route. The DSH web server throws on a
 * duplicate (kind, path), so this runs exactly once per plugin fiber and its
 * disposer is handed to `ctx.effect`.
 */
export function registerBff(ctx: BffContext, options: BffOptions): () => void {
  const bff = createBff({ logger: ctx.logger, ...options })
  const unregister = ctx.webServer.register({
    kind: 'prefix',
    path: API_PREFIX,
    handler: (req, res) => bff.handle(req, res),
  })
  const dispose = (): void => {
    unregister()
    bff.close()
  }
  ctx.effect?.(() => dispose, 'ccteam-team.bff')
  return dispose
}

// ------------------------------------------------------------------ mappers

/**
 * `GET /api/v1/agents/graph` returns a flat `nodes` array (snake_case; the
 * crate sets no `rename_all`) whose `parent_sid` carries the linkage. The
 * contract wants a per-project forest, so nest by parent within a project and
 * treat a parent in another project as a root.
 */
export function buildGraph(nodes: unknown[]): TeamGraph {
  const byProject = new Map<string, Map<string, TeamNode>>()
  const order: string[] = []
  const slugOf = new Map<string, string>()
  for (const entry of nodes) {
    const row = asRecord(entry)
    const sid = stringOf(row?.sid)
    if (row === undefined || sid === undefined) continue
    const slug = stringOf(row.slug) ?? ''
    if (!byProject.has(slug)) {
      byProject.set(slug, new Map())
      order.push(slug)
    }
    slugOf.set(sid, slug)
    byProject.get(slug)!.set(sid, {
      sid,
      project: slug,
      vendor: stringOf(row.vendor) ?? '',
      activity: activityOf(stringOf(row.status)),
      ...defined('title', stringOf(row.title)),
      ...defined('role', emptyToUndefined(stringOf(row.role))),
      ...defined('model', stringOf(row.model)),
      ...defined('parentSid', stringOf(row.parent_sid)),
      ...defined('costUsd', numberOf(row.cost_usd)),
      ...defined('tokensTotal', numberOf(row.tokens_total)),
      ...defined('lastActive', emptyToUndefined(stringOf(row.last_active))),
      children: [],
    })
  }
  const projects = order.sort().map(slug => {
    const table = byProject.get(slug)!
    const roots: TeamNode[] = []
    for (const node of table.values()) {
      const parent = node.parentSid === undefined ? undefined : table.get(node.parentSid)
      if (parent === undefined || slugOf.get(node.parentSid ?? '') !== slug) roots.push(node)
      else parent.children.push(node)
    }
    return { slug, nodes: roots }
  })
  return { projects }
}

/**
 * The graph endpoint reports tracked-ness (`live` / `idle`), a coarser axis
 * than the panel's four-way activity. `live` is surfaced as `working` and the
 * live SSE refines it; `stale` / `stuck` are not derivable here (they come
 * from the per-project session view, which would cost one call per project).
 */
function activityOf(status: string | undefined): Activity {
  switch (status) {
    case 'live':
    case 'working':
      return 'working'
    case 'stale':
      return 'stale'
    case 'stuck':
      return 'stuck'
    default:
      return 'idle'
  }
}

/**
 * One upstream history event carries BOTH halves of a turn (`user` +
 * `assistant`, either possibly empty), so it fans out into up to two contract
 * rows. Ids are suffixed to stay unique, which also makes them usable as the
 * `since` cursor.
 */
export function transcriptRows(events: unknown[]): TranscriptRow[] {
  const rows: TranscriptRow[] = []
  for (const entry of events) {
    const row = asRecord(entry)
    if (row === undefined) continue
    const turnId = stringOf(row.turn_id) ?? ''
    const ts = stringOf(row.ts)
    const user = stringOf(row.user) ?? ''
    const assistant = stringOf(row.assistant) ?? ''
    if (user !== '') {
      rows.push({ turnId: `${turnId}:user`, role: 'user', content: user, ...defined('ts', ts) })
    }
    if (assistant !== '') {
      rows.push({
        turnId: `${turnId}:assistant`,
        role: 'assistant',
        content: assistant,
        ...defined('ts', ts),
      })
    }
  }
  return rows
}

/**
 * Global stream → tree invalidation plus the badge feed.
 *
 * `session_lifecycle` and `delegation` change the tree's shape; a completed
 * turn changes its cost/turn counters. Turn completion is `kind:"answer"`
 * (the finalizing `progress`+`done` frame is only emitted when the turn had
 * tool activity, so it is a hint, not the signal). An `answer` carrying
 * `options` is a human-in-the-loop prompt, not a finished turn.
 */
export function translateGlobal(frame: SseFrame): PanelEvent[] {
  if (frame.event === 'reconnect_hint' || frame.event === 'gateway_unavailable') return []
  const data = parseData(frame)
  if (data === undefined) return []
  const kind = stringOf(data.kind)
  if (kind === 'session_lifecycle' || kind === 'delegation') return [{ kind: 'graph' }]
  if (kind === 'answer' && data.options === undefined) {
    const sid = stringOf(data.sid)
    return sid === undefined
      ? [{ kind: 'graph' }]
      : [{ kind: 'graph' }, { kind: 'turn_done', sid }]
  }
  if (kind === 'progress' && data.done === true) return [{ kind: 'graph' }]
  return []
}

/**
 * Per-session stream → transcript deltas. `turn_done` is deliberately NOT
 * emitted here: a client watching a sid subscribes to both streams, and the
 * global one already carries it, so emitting on both would double-count.
 */
export function translateSession(sid: string, frame: SseFrame): PanelEvent[] {
  if (frame.event === 'reconnect_hint' || frame.event === 'gateway_unavailable') return []
  const data = parseData(frame)
  if (data === undefined) return []
  const kind = stringOf(data.kind)
  if (kind === 'answer' && data.options === undefined) {
    return [{
      kind: 'session',
      sid,
      row: {
        turnId: `${stringOf(data.id) ?? ''}:assistant`,
        role: 'assistant',
        content: stringOf(data.content) ?? '',
        ...defined('ts', stringOf(data.ts)),
      },
      activity: 'idle',
    }]
  }
  if (kind === 'activity') return [{ kind: 'session', sid, activity: 'working' }]
  if (kind === 'progress') {
    return [{ kind: 'session', sid, activity: data.done === true ? 'idle' : 'working' }]
  }
  return []
}

// ------------------------------------------------------------------ helpers

function parseData(frame: SseFrame): Record<string, unknown> | undefined {
  try {
    return asRecord(JSON.parse(frame.data) as unknown)
  } catch {
    return undefined
  }
}

function failure(result: Upstream): SendReceipt {
  if (result.kind === 'network') {
    return { ok: false, errorKind: 'unreachable', error: result.error }
  }
  if (result.kind === 'http') {
    return { ok: false, errorKind: result.code ?? errorKindFor(result.status), error: result.error }
  }
  return { ok: false, errorKind: 'unknown', error: 'unexpected upstream result' }
}

function errorKindFor(status: number): string {
  if (status === 400 || status === 422) return 'bad_request'
  if (status === 401 || status === 403) return 'unauthorized'
  if (status === 404) return 'not_found'
  if (status === 409) return 'conflict'
  if (status === 502 || status === 503) return 'gateway'
  return `http_${status}`
}

async function readJson(req: IncomingMessage): Promise<unknown> {
  const chunks: Buffer[] = []
  try {
    for await (const chunk of req as AsyncIterable<Buffer | string>) {
      chunks.push(typeof chunk === 'string' ? Buffer.from(chunk) : chunk)
    }
  } catch {
    return {}
  }
  const raw = Buffer.concat(chunks).toString('utf8').trim()
  if (raw === '') return {}
  try {
    return JSON.parse(raw) as unknown
  } catch {
    return {}
  }
}

function sendJson(res: ServerResponse, status: number, payload: unknown): void {
  if (res.writableEnded) return
  const body = JSON.stringify(payload ?? null)
  res.writeHead(status, {
    'content-type': 'application/json; charset=utf-8',
    'cache-control': 'no-store',
  })
  res.end(body)
}

function defined<K extends string, V>(key: K, value: V | undefined): Record<K, V> | object {
  return value === undefined ? {} : ({ [key]: value } as Record<K, V>)
}

function emptyToUndefined(value: string | undefined): string | undefined {
  return value === undefined || value === '' ? undefined : value
}

function asRecord(value: unknown): Record<string, unknown> | undefined {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined
}

function arrayOf(value: unknown): unknown[] {
  return Array.isArray(value) ? value : []
}

function stringOf(value: unknown): string | undefined {
  return typeof value === 'string' ? value : undefined
}

function numberOf(value: unknown): number | undefined {
  return typeof value === 'number' && Number.isFinite(value) ? value : undefined
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}
