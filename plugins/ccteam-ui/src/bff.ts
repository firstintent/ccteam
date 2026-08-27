/**
 * The host half's backend-for-frontend: ONE prefix route under
 * `API_PREFIX` that answers the browser in the shapes of
 * `src/shared/contract.ts` and speaks ccteam's REST/SSE API upstream.
 *
 * Why a BFF at all rather than letting the workbench call ccteam directly:
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
 *   GET   /api/v1/capabilities                    → vendor availability probe
 *   GET   /api/v1/agents/graph                    → delegation tree
 *   GET   /api/v1/agents/events                   → global SSE
 *   GET   /api/v1/projects                        → project catalog
 *   GET   /api/v1/models                          → model / effort catalog
 *   GET   /api/v1/projects/{slug}/roles           → role catalog
 *   GET   /api/v1/sessions/{sid}                  → transcript page (cursor)
 *   GET   /api/v1/sessions/{sid}/status           → live statusline
 *   GET   /api/v1/sessions/{sid}/events           → per-session SSE
 *   POST  /api/v1/sessions/{sid}/turn             → submit a user turn
 *   POST  /api/v1/sessions/{sid}/interrupt        → interrupt the running turn
 *   POST  /api/v1/sessions/{sid}/stop             → stop the session
 *   POST  /api/v1/sessions/{sid}/resolve          → answer a choice prompt
 *   PATCH /api/v1/sessions/{sid}                  → set the title
 *   POST  /api/v1/projects/{slug}/sessions        → create a session
 *   POST  /api/v1/projects/{slug}/uploads?name=   → store an attachment
 *   GET   /api/v1/projects/{slug}/uploads/{name}  → attachment bytes
 */
import type { IncomingMessage, ServerResponse } from 'node:http'
import {
  API_PREFIX,
  ATTACHMENT_PATH,
  EVENTS_PATH,
  UPLOAD_PATH,
  type Activity,
  type ApiMethod,
  type AttachmentRef,
  type ChoiceOption,
  type HistoryRequest,
  type HistoryResponse,
  type ModelsCatalog,
  type PanelEvent,
  type ProjectInfo,
  type RenameRequest,
  type ResolveRequest,
  type RolesRequest,
  type RolesResponse,
  type SendReceipt,
  type SendRequest,
  type SessionEvent,
  type SessionRef,
  type SessionStatus,
  type SimpleReceipt,
  type SpawnRequest,
  type SpawnResponse,
  type StatusResponse,
  type Step,
  type TeamGraph,
  type TeamNode,
  type TranscriptRow,
  type TurnAttachmentInput,
  type VendorAvailability,
} from './shared/contract.js'
import { SseHub, type SseFrame, type UpstreamSource } from './sse.js'

export type FetchLike = (input: string, init?: RequestInit) => Promise<Response>

const API_METHODS: ReadonlySet<string> = new Set<ApiMethod>([
  'status',
  'team.graph',
  'catalog.projects',
  'catalog.models',
  'catalog.roles',
  'session.history',
  'session.status',
  'session.send',
  'session.spawn',
  'session.interrupt',
  'session.stop',
  'session.resolve',
  'session.rename',
])

/** Proxies drop idle streams; DSH's own keepalive is 15s, ours is 25s. */
const DEFAULT_HEARTBEAT_MS = 25_000
const DEFAULT_HISTORY_LIMIT = 100
const MAX_HISTORY_LIMIT = 1000
/** Upload cap mirrors ccteam's (25 MiB). */
const MAX_UPLOAD_BYTES = 25 * 1024 * 1024

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
  /** sid → project slug, refreshed by every graph read (attachment routing). */
  const slugBySid = new Map<string, string>()

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
    const graph = buildGraph(arrayOf(asRecord(result.body)?.nodes))
    for (const project of graph.projects) {
      const visit = (node: TeamNode): void => {
        slugBySid.set(node.sid, project.slug)
        node.children.forEach(visit)
      }
      project.nodes.forEach(visit)
    }
    return graph
  }

  async function projects(): Promise<{ projects: ProjectInfo[] }> {
    const result = await call('/api/v1/projects')
    if (result.kind !== 'ok') return { projects: [] }
    const rows = Array.isArray(result.body) ? result.body : arrayOf(asRecord(result.body)?.projects)
    return { projects: buildProjects(rows) }
  }

  async function models(): Promise<ModelsCatalog> {
    const result = await call('/api/v1/models')
    if (result.kind !== 'ok') return { vendors: [] }
    return buildModels(arrayOf(asRecord(result.body)?.vendors))
  }

  async function roles(request: RolesRequest): Promise<RolesResponse> {
    const project = (request.project ?? '').trim()
    if (project === '') return { project: '', roles: [] }
    const result = await call(`/api/v1/projects/${encodeURIComponent(project)}/roles`)
    if (result.kind !== 'ok') return { project, roles: [] }
    const rows = Array.isArray(result.body) ? result.body : arrayOf(asRecord(result.body)?.roles)
    const names: string[] = []
    for (const entry of rows) {
      const name = typeof entry === 'string' ? entry : stringOf(asRecord(entry)?.role) ?? stringOf(asRecord(entry)?.name)
      if (name !== undefined && name !== '') names.push(name)
    }
    return { project, roles: names }
  }

  async function history(request: HistoryRequest): Promise<HistoryResponse> {
    const sid = (request.sid ?? '').trim()
    if (sid === '') return { sid: '', rows: [], hasMore: false }
    const limit = Math.min(MAX_HISTORY_LIMIT, Math.max(1, Math.floor(numberOf(request.limit) ?? DEFAULT_HISTORY_LIMIT)))
    const before = stringOf(request.before)
    const query = `limit=${limit}${before === undefined || before === '' ? '' : `&before=${encodeURIComponent(before)}`}`
    const result = await call(`/api/v1/sessions/${encodeURIComponent(sid)}?${query}`)
    if (result.kind !== 'ok') return { sid, rows: [], hasMore: false }
    const body = asRecord(result.body)
    const rows = transcriptRows(arrayOf(body?.events), sid)
    const nextBefore = stringOf(body?.next_before)
    return {
      sid,
      rows,
      hasMore: body?.has_more === true,
      ...(nextBefore === undefined || nextBefore === '' ? {} : { nextBefore }),
    }
  }

  async function sessionStatus(request: SessionRef): Promise<SessionStatus> {
    const sid = (request.sid ?? '').trim()
    const result = await call(`/api/v1/sessions/${encodeURIComponent(sid)}/status`)
    if (result.kind !== 'ok') return { sid }
    const body = asRecord(result.body)
    const context = asRecord(body?.context)
    return {
      sid,
      ...defined('model', stringOf(body?.model)),
      ...defined('effort', stringOf(body?.effort)),
      ...(context === undefined
        ? {}
        : {
            context: {
              ...defined('usedTokens', numberOf(context.used_tokens)),
              ...defined('windowTokens', numberOf(context.window_tokens)),
              ...defined('pct', numberOf(context.pct)),
            },
          }),
    }
  }

  function turnAttachments(input: TurnAttachmentInput[] | undefined): Array<{ kind: string; path: string }> {
    return (input ?? [])
      .filter(a => (a.kind === 'image' || a.kind === 'file') && typeof a.path === 'string' && a.path !== '')
      .map(a => ({ kind: a.kind, path: a.path }))
  }

  async function send(request: SendRequest): Promise<SendReceipt> {
    const sid = (request.sid ?? '').trim()
    if (sid === '') return { ok: false, errorKind: 'bad_request', error: 'sid is required' }
    const attachments = turnAttachments(request.attachments)
    const result = await call(`/api/v1/sessions/${encodeURIComponent(sid)}/turn`, {
      method: 'POST',
      body: { text: request.text ?? '', ...(attachments.length === 0 ? {} : { attachments }) },
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
          + '(or set one in DSH Settings → Plugins → ccteam-ui).',
      }
    }
    const vendor = (request.vendor ?? '').trim()
    if (vendor === '') return { ok: false, error: 'vendor is required' }
    // `role` must be PRESENT; an empty value means roleless, which is
    // ccteam's default. `host` must never appear — the key alone is a 400.
    const created = await call(`/api/v1/projects/${encodeURIComponent(slug)}/sessions`, {
      method: 'POST',
      body: {
        role: (request.role ?? '').trim(),
        vendor,
        ...defined('model', emptyToUndefined(request.model)),
        ...defined('effort', emptyToUndefined(request.effort)),
        ...defined('mode', emptyToUndefined(request.mode)),
      },
    })
    if (created.kind !== 'ok') {
      const failed = failure(created)
      return { ok: false, error: failed.error ?? 'spawn failed' }
    }
    const sid = stringOf(asRecord(created.body)?.sid)
    if (sid === undefined) return { ok: false, error: 'daemon accepted the spawn but returned no sid' }
    slugBySid.set(sid, slug)

    // Neither a title nor a first task is a create-time field upstream, so
    // both are follow-ups. A failed title is cosmetic; a failed first task is
    // not, and is reported with the sid so the session is still reachable.
    const title = (request.title ?? '').trim()
    if (title !== '') {
      await call(`/api/v1/sessions/${encodeURIComponent(sid)}`, { method: 'PATCH', body: { title } })
    }
    const task = (request.task ?? '').trim()
    if (task !== '') {
      const receipt = await send({ sid, text: task, ...(request.attachments === undefined ? {} : { attachments: request.attachments }) })
      if (!receipt.ok) {
        return { ok: false, sid, error: `session ${sid} was created but its first task failed: ${receipt.error ?? 'unknown error'}` }
      }
    }
    return { ok: true, sid }
  }

  async function simple(path: string, init: { method: string; body?: unknown }): Promise<SimpleReceipt> {
    const result = await call(path, init)
    if (result.kind !== 'ok') {
      const failed = failure(result)
      return { ok: false, ...defined('errorKind', failed.errorKind), ...defined('error', failed.error) }
    }
    return { ok: true }
  }

  async function interrupt(request: SessionRef): Promise<SimpleReceipt> {
    const sid = (request.sid ?? '').trim()
    if (sid === '') return { ok: false, errorKind: 'bad_request', error: 'sid is required' }
    return simple(`/api/v1/sessions/${encodeURIComponent(sid)}/interrupt`, { method: 'POST' })
  }

  async function stop(request: SessionRef): Promise<SimpleReceipt> {
    const sid = (request.sid ?? '').trim()
    if (sid === '') return { ok: false, errorKind: 'bad_request', error: 'sid is required' }
    return simple(`/api/v1/sessions/${encodeURIComponent(sid)}/stop`, { method: 'POST' })
  }

  async function resolve(request: ResolveRequest): Promise<SimpleReceipt> {
    const sid = (request.sid ?? '').trim()
    const token = (request.token ?? '').trim()
    const selection = (request.selection ?? '').trim()
    if (sid === '' || token === '' || selection === '') {
      return { ok: false, errorKind: 'bad_request', error: 'sid, token and selection are required' }
    }
    return simple(`/api/v1/sessions/${encodeURIComponent(sid)}/resolve`, {
      method: 'POST',
      body: { token, selection },
    })
  }

  async function rename(request: RenameRequest): Promise<SimpleReceipt> {
    const sid = (request.sid ?? '').trim()
    const title = (request.title ?? '').trim()
    if (sid === '' || title === '') return { ok: false, errorKind: 'bad_request', error: 'sid and title are required' }
    return simple(`/api/v1/sessions/${encodeURIComponent(sid)}`, { method: 'PATCH', body: { title } })
  }

  async function dispatch(method: string, payload: unknown): Promise<unknown> {
    const record = asRecord(payload) ?? {}
    switch (method as ApiMethod) {
      case 'status':
        return await status()
      case 'team.graph':
        return await teamGraph()
      case 'catalog.projects':
        return await projects()
      case 'catalog.models':
        return await models()
      case 'catalog.roles':
        return await roles(record as unknown as RolesRequest)
      case 'session.history':
        return await history(record as unknown as HistoryRequest)
      case 'session.status':
        return await sessionStatus(record as unknown as SessionRef)
      case 'session.send':
        return await send(record as unknown as SendRequest)
      case 'session.spawn':
        return await spawn(record as unknown as SpawnRequest)
      case 'session.interrupt':
        return await interrupt(record as unknown as SessionRef)
      case 'session.stop':
        return await stop(record as unknown as SessionRef)
      case 'session.resolve':
        return await resolve(record as unknown as ResolveRequest)
      case 'session.rename':
        return await rename(record as unknown as RenameRequest)
      default:
        return undefined
    }
  }

  // -------------------------------------------------------------- files

  async function slugForSid(sid: string): Promise<string | undefined> {
    const known = slugBySid.get(sid)
    if (known !== undefined) return known
    await teamGraph()
    return slugBySid.get(sid)
  }

  async function handleUpload(req: IncomingMessage, res: ServerResponse, query: URLSearchParams): Promise<void> {
    const project = (query.get('project') ?? '').trim()
    const name = (query.get('name') ?? '').trim()
    if (project === '' || name === '') {
      sendJson(res, 400, { ok: false, error: 'project and name are required' })
      return
    }
    const body = await readBytes(req, MAX_UPLOAD_BYTES)
    if (body === undefined) {
      sendJson(res, 413, { ok: false, error: 'file exceeds the 25 MiB cap' })
      return
    }
    if (body.byteLength === 0) {
      sendJson(res, 400, { ok: false, error: 'empty file' })
      return
    }
    let response: Response
    try {
      response = await doFetch(
        `${base()}/api/v1/projects/${encodeURIComponent(project)}/uploads?name=${encodeURIComponent(name)}`,
        {
          method: 'POST',
          headers: {
            accept: 'application/json',
            'content-type': req.headers['content-type'] ?? 'application/octet-stream',
            ...authHeader(),
          },
          body: body as unknown as BodyInit,
        },
      )
    } catch (error) {
      sendJson(res, 502, { ok: false, error: describe(error) })
      return
    }
    const raw = await response.text().catch(() => '')
    let parsed: unknown
    try {
      parsed = raw === '' ? undefined : (JSON.parse(raw) as unknown)
    } catch {
      parsed = undefined
    }
    const record = asRecord(parsed)
    if (!response.ok) {
      sendJson(res, response.status, { ok: false, error: stringOf(record?.error) ?? raw ?? `HTTP ${response.status}` })
      return
    }
    const path = stringOf(record?.path) ?? ''
    const stored = stringOf(record?.name) ?? basename(path)
    const kind = stringOf(record?.kind) === 'image' ? 'image' : 'file'
    sendJson(res, 200, {
      ok: true,
      attachment: { kind, name: stored, path, url: attachmentUrlFor(project, stored) },
    })
  }

  async function handleAttachment(res: ServerResponse, query: URLSearchParams): Promise<void> {
    const name = (query.get('name') ?? '').trim()
    let project = (query.get('project') ?? '').trim()
    const sid = (query.get('sid') ?? '').trim()
    if (project === '' && sid !== '') project = (await slugForSid(sid)) ?? ''
    if (project === '' || name === '' || name.includes('/') || name.includes('..')) {
      sendJson(res, 400, { error: 'project (or sid) and a plain name are required' })
      return
    }
    let response: Response
    try {
      response = await doFetch(
        `${base()}/api/v1/projects/${encodeURIComponent(project)}/uploads/${encodeURIComponent(name)}`,
        { headers: authHeader() },
      )
    } catch (error) {
      sendJson(res, 502, { error: describe(error) })
      return
    }
    if (!response.ok) {
      sendJson(res, response.status, { error: `HTTP ${response.status}` })
      return
    }
    const headers: Record<string, string> = { 'cache-control': 'private, max-age=3600' }
    for (const key of ['content-type', 'content-length', 'content-disposition']) {
      const value = response.headers.get(key)
      if (value !== null) headers[key] = value
    }
    res.writeHead(200, headers)
    const bytes = new Uint8Array(await response.arrayBuffer())
    res.end(Buffer.from(bytes))
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
    let url: URL
    try {
      url = new URL(req.url ?? '/', 'http://dsh.invalid')
    } catch {
      sendJson(res, 404, { error: 'not found' })
      return
    }
    const pathname = url.pathname
    const query = url.searchParams
    if (pathname !== API_PREFIX && !pathname.startsWith(`${API_PREFIX}/`)) {
      sendJson(res, 404, { error: 'not found' })
      return
    }
    const rest = pathname.slice(API_PREFIX.length)
    const verb = (req.method ?? 'GET').toUpperCase()
    if (rest === EVENTS_PATH) {
      if (verb !== 'GET') {
        sendJson(res, 404, { error: 'not found' })
        return
      }
      openEvents(req, res, query.get('sid'))
      return
    }
    if (rest === UPLOAD_PATH) {
      if (verb !== 'POST') {
        sendJson(res, 404, { error: 'not found' })
        return
      }
      try {
        await handleUpload(req, res, query)
      } catch (error) {
        options.logger?.warn(`ccteam-ui: upload failed: ${describe(error)}`)
        sendJson(res, 500, { ok: false, error: describe(error) })
      }
      return
    }
    if (rest === ATTACHMENT_PATH) {
      if (verb !== 'GET') {
        sendJson(res, 404, { error: 'not found' })
        return
      }
      try {
        await handleAttachment(res, query)
      } catch (error) {
        options.logger?.warn(`ccteam-ui: attachment failed: ${describe(error)}`)
        sendJson(res, 500, { error: describe(error) })
      }
      return
    }
    const method = rest.startsWith('/') ? rest.slice(1) : rest
    if (verb !== 'POST' || !API_METHODS.has(method)) {
      sendJson(res, 404, { error: `unknown method: ${method}` })
      return
    }
    try {
      sendJson(res, 200, await dispatch(method, await readJson(req)))
    } catch (error) {
      options.logger?.warn(`ccteam-ui: ${method} failed: ${describe(error)}`)
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
  ctx.effect?.(() => dispose, 'ccteam-ui.bff')
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
      ...defined('title', emptyToUndefined(stringOf(row.title))),
      ...defined('role', emptyToUndefined(stringOf(row.role))),
      ...defined('model', emptyToUndefined(stringOf(row.model))),
      ...defined('effort', emptyToUndefined(stringOf(row.effort))),
      ...defined('host', emptyToUndefined(stringOf(row.host))),
      ...defined('parentSid', emptyToUndefined(stringOf(row.parent_sid))),
      ...defined('costUsd', numberOf(row.cost_usd)),
      ...defined('tokensTotal', numberOf(row.tokens_total)),
      ...defined('lastActive', emptyToUndefined(stringOf(row.last_active))),
      ...defined('turnCount', numberOf(row.turn_count)),
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
 * `GET /api/v1/projects` returns dashboard rows (`slug`, `team`, and the
 * project state); only the identity-facing fields travel to the workbench.
 */
export function buildProjects(rows: unknown[]): ProjectInfo[] {
  const projects: ProjectInfo[] = []
  for (const entry of rows) {
    const row = asRecord(entry)
    const slug = stringOf(row?.slug)
    if (row === undefined || slug === undefined || slug === '') continue
    const state = asRecord(row.state)
    projects.push({
      slug,
      ...defined('team', emptyToUndefined(stringOf(row.team))),
      ...defined('host', emptyToUndefined(stringOf(row.host) ?? stringOf(state?.host))),
    })
  }
  return projects.sort((a, b) => a.slug.localeCompare(b.slug))
}

/**
 * `GET /api/v1/models` → `{vendors: [{vendor, models: [{id, display_name,
 * efforts}], efforts, observed_at, source}]}` (advisory, never a gate).
 */
export function buildModels(rows: unknown[]): ModelsCatalog {
  const vendors: ModelsCatalog['vendors'] = []
  for (const entry of rows) {
    const row = asRecord(entry)
    const vendor = stringOf(row?.vendor)
    if (row === undefined || vendor === undefined) continue
    const models = arrayOf(row.models).flatMap((m) => {
      const model = asRecord(m)
      const id = stringOf(model?.id)
      if (model === undefined || id === undefined || id === '') return []
      return [{
        id,
        ...defined('displayName', emptyToUndefined(stringOf(model.display_name))),
        efforts: stringsOf(model.efforts),
      }]
    })
    vendors.push({
      vendor,
      models,
      efforts: stringsOf(row.efforts),
      ...defined('observedAt', emptyToUndefined(stringOf(row.observed_at))),
    })
  }
  return { vendors }
}

/**
 * The graph endpoint reports tracked-ness (`live` / `idle`), a coarser axis
 * than the workbench's four-way activity. `live` is surfaced as `working` and
 * the live SSE refines it; `stale` / `stuck` are not derivable here (they
 * come from the per-project session view, which would cost one call per
 * project).
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

function attachmentUrlFor(project: string, name: string): string {
  return `${API_PREFIX}${ATTACHMENT_PATH}?project=${encodeURIComponent(project)}&name=${encodeURIComponent(name)}`
}

function attachmentUrlForSid(sid: string, name: string): string {
  return `${API_PREFIX}${ATTACHMENT_PATH}?sid=${encodeURIComponent(sid)}&name=${encodeURIComponent(name)}`
}

/**
 * Attachment refs as ccteam serializes them (`{kind, path?, name?}`), mapped
 * onto the workbench shape with a URL the attachment route can serve.
 */
export function attachmentRefs(value: unknown, sid: string): AttachmentRef[] | undefined {
  const rows = arrayOf(value)
  if (rows.length === 0) return undefined
  const refs: AttachmentRef[] = []
  for (const entry of rows) {
    const row = asRecord(entry)
    if (row === undefined) continue
    const rawKind = stringOf(row.kind)
    const kind = rawKind === 'image' ? 'image' : rawKind === 'skill' ? 'skill' : 'file'
    const path = stringOf(row.path)
    const name = emptyToUndefined(stringOf(row.name)) ?? (path === undefined ? undefined : basename(path))
    if (name === undefined || name === '') continue
    refs.push({
      kind,
      name,
      ...(kind === 'skill' || path === undefined ? {} : { url: attachmentUrlForSid(sid, basename(path)) }),
    })
  }
  return refs.length === 0 ? undefined : refs
}

/**
 * One upstream history event carries BOTH halves of a turn (`user` +
 * `assistant`, either possibly empty), so it fans out into up to two contract
 * rows. Ids are suffixed to stay unique.
 */
export function transcriptRows(events: unknown[], sid = ''): TranscriptRow[] {
  const rows: TranscriptRow[] = []
  for (const entry of events) {
    const row = asRecord(entry)
    if (row === undefined) continue
    const turnId = stringOf(row.turn_id) ?? ''
    const ts = stringOf(row.ts)
    const user = stringOf(row.user) ?? ''
    const assistant = stringOf(row.assistant) ?? ''
    const vendor = emptyToUndefined(stringOf(row.vendor))
    const status = emptyToUndefined(stringOf(row.status))
    const attachments = attachmentRefs(row.attachments, sid)
    const usageRow = asRecord(row.usage)
    const usage = usageRow === undefined
      ? undefined
      : {
          ...defined('costUsd', numberOf(usageRow.cost_usd)),
          ...defined('inputTokens', numberOf(usageRow.input_tokens)),
          ...defined('outputTokens', numberOf(usageRow.output_tokens)),
        }
    if (user !== '') {
      rows.push({
        turnId: `${turnId}:user`,
        role: 'user',
        content: user,
        ...defined('ts', ts),
        ...defined('vendor', vendor),
        ...defined('attachments', attachments),
      })
    }
    if (assistant !== '') {
      rows.push({
        turnId: `${turnId}:assistant`,
        role: 'assistant',
        content: assistant,
        ...defined('ts', ts),
        ...defined('vendor', vendor),
        ...defined('status', status),
        ...(usage === undefined || Object.keys(usage).length === 0 ? {} : { usage }),
      })
    }
  }
  return rows
}

/**
 * Global stream → tree invalidation, the badge feed, delegation narration
 * and lifecycle frames for whichever sid they concern (clients filter).
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
  const sid = stringOf(data.sid)
  if (kind === 'session_lifecycle') {
    const state = stringOf(data.state)
    const out: PanelEvent[] = [{ kind: 'graph' }]
    if (sid !== undefined && state !== undefined) {
      out.push({
        kind: 'session',
        sid,
        event: { kind: 'lifecycle', state, ...defined('reason', emptyToUndefined(stringOf(data.reason))), ...defined('ts', stringOf(data.ts)) },
      })
    }
    return out
  }
  if (kind === 'delegation') {
    const relation = stringOf(data.relation)
    // A frame without a relation reshapes the tree but narrates nothing.
    if (relation === undefined) return [{ kind: 'graph' }]
    return [
      { kind: 'graph' },
      {
        kind: 'delegation',
        relation,
        ...defined('parentSid', stringOf(data.parent_sid)),
        ...defined('childSid', stringOf(data.child_sid)),
        ...defined('title', emptyToUndefined(stringOf(data.title))),
        ...defined('reason', emptyToUndefined(stringOf(data.reason))),
      },
    ]
  }
  if (kind === 'answer' && data.options === undefined) {
    return sid === undefined
      ? [{ kind: 'graph' }]
      : [{ kind: 'graph' }, { kind: 'turn_done', sid }]
  }
  if (kind === 'progress' && data.done === true) return [{ kind: 'graph' }]
  return []
}

/**
 * Per-session stream → the full session event vocabulary. `turn_done` is
 * deliberately NOT emitted here: a client watching a sid subscribes to both
 * streams, and the global one already carries it, so emitting on both would
 * double-count.
 */
export function translateSession(sid: string, frame: SseFrame): PanelEvent[] {
  if (frame.event === 'reconnect_hint' || frame.event === 'gateway_unavailable') return []
  const data = parseData(frame)
  if (data === undefined) return []
  const kind = stringOf(data.kind)
  const ts = stringOf(data.ts)
  const wrap = (event: SessionEvent): PanelEvent[] => [{ kind: 'session', sid, event }]
  if (kind === 'answer') {
    const options = choiceOptions(data.options)
    const token = stringOf(data.token)
    return wrap({
      kind: 'answer',
      id: stringOf(data.id) ?? '',
      content: stringOf(data.content) ?? '',
      ...defined('ts', ts),
      ...defined('status', emptyToUndefined(stringOf(data.status))),
      ...defined('attachments', attachmentRefs(data.attachments, sid)),
      ...(options.length > 0 ? { options } : {}),
      ...defined('token', token),
    })
  }
  if (kind === 'activity') {
    const step = stepOf(data.activity, stringOf(data.content))
    return step === undefined ? [] : wrap({ kind: 'activity', step, ...defined('ts', ts) })
  }
  if (kind === 'progress') {
    return wrap({ kind: 'progress', content: stringOf(data.content) ?? '', done: data.done === true, ...defined('ts', ts) })
  }
  if (kind === 'session_lifecycle') {
    const state = stringOf(data.state)
    return state === undefined
      ? []
      : wrap({ kind: 'lifecycle', state, ...defined('reason', emptyToUndefined(stringOf(data.reason))), ...defined('ts', ts) })
  }
  return []
}

function choiceOptions(value: unknown): ChoiceOption[] {
  const options: ChoiceOption[] = []
  for (const entry of arrayOf(value)) {
    const row = asRecord(entry)
    const id = stringOf(row?.id)
    const label = stringOf(row?.label)
    if (id === undefined) continue
    options.push({ id, label: label ?? id })
  }
  return options
}

function stepOf(value: unknown, fallbackSummary: string | undefined): Step | undefined {
  const row = asRecord(value)
  if (row === undefined) return undefined
  const itemId = stringOf(row.item_id) ?? stringOf(row.itemId)
  const kind = stringOf(row.kind) ?? 'tool_call'
  const name = stringOf(row.name) ?? ''
  if (itemId === undefined) return undefined
  return {
    itemId,
    kind,
    name,
    summary: stringOf(row.summary) ?? fallbackSummary ?? name,
    status: stringOf(row.status) ?? 'started',
  }
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

async function readBytes(req: IncomingMessage, cap: number): Promise<Buffer | undefined> {
  const chunks: Buffer[] = []
  let total = 0
  try {
    for await (const chunk of req as AsyncIterable<Buffer | string>) {
      const buffer = typeof chunk === 'string' ? Buffer.from(chunk) : chunk
      total += buffer.byteLength
      if (total > cap) return undefined
      chunks.push(buffer)
    }
  } catch {
    return Buffer.alloc(0)
  }
  return Buffer.concat(chunks)
}

async function readJson(req: IncomingMessage): Promise<unknown> {
  const raw = ((await readBytes(req, MAX_UPLOAD_BYTES)) ?? Buffer.alloc(0)).toString('utf8').trim()
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

function basename(path: string): string {
  const cut = path.lastIndexOf('/')
  return cut === -1 ? path : path.slice(cut + 1)
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

function stringsOf(value: unknown): string[] {
  return arrayOf(value).filter((v): v is string => typeof v === 'string' && v !== '')
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
