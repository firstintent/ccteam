import { randomUUID } from 'node:crypto'
import type { SessionCredentialStore } from './credentials.js'

export interface ContentBlock {
  type: string
  text?: string
  [key: string]: unknown
}

export interface McpToolResult {
  content: ContentBlock[]
  isError: boolean
  [key: string]: unknown
}

export interface CcteamMcpClientOptions {
  daemonUrl: string
  credential: () => string | undefined
  clientName: string
  clientVersion: string
  fetchImpl?: typeof fetch
}

interface JsonRpcSuccess {
  jsonrpc: '2.0'
  id: string
  result: unknown
}

interface JsonRpcFailure {
  jsonrpc: '2.0'
  id: string | null
  error: {
    code: number
    message: string
    data?: unknown
  }
}

type JsonRpcResponse = JsonRpcSuccess | JsonRpcFailure

export class CcteamMcpClient {
  private readonly daemonUrl: string
  private readonly credential: () => string | undefined
  private readonly clientName: string
  private readonly clientVersion: string
  private readonly fetchImpl: typeof fetch
  private sessionId: string | undefined
  private initializing: Promise<void> | undefined
  private closed = false

  constructor(options: CcteamMcpClientOptions) {
    this.daemonUrl = options.daemonUrl.replace(/\/+$/, '')
    this.credential = options.credential
    this.clientName = options.clientName
    this.clientVersion = options.clientVersion
    this.fetchImpl = options.fetchImpl ?? fetch
  }

  async initialize(): Promise<void> {
    this.assertOpen()
    if (this.initializing !== undefined) {
      return this.initializing
    }
    this.initializing = (async () => {
      const result = await this.request('initialize', {
        protocolVersion: '2024-11-05',
        capabilities: {},
        clientInfo: {
          name: this.clientName,
          version: this.clientVersion,
        },
      }, { skipEnsureInitialized: true })
      if (result === undefined) {
        throw new Error('ccteam MCP initialize returned no result')
      }
    })()
    return this.initializing
  }

  async callTool(name: string, args: unknown): Promise<McpToolResult> {
    this.assertOpen()
    await this.initialize()
    const result = await this.request('tools/call', {
      name,
      arguments: args ?? {},
    })
    if (!isRecord(result)) {
      throw new Error('ccteam MCP tools/call returned a non-object result')
    }
    const content = Array.isArray(result.content)
      ? result.content.map(normalizeContentBlock)
      : [{ type: 'text', text: JSON.stringify(result) }]
    return {
      ...result,
      content,
      isError: result.isError === true,
    }
  }

  private async request(method: string, params: unknown, options?: { skipEnsureInitialized?: boolean }): Promise<unknown> {
    this.assertOpen()
    if (options?.skipEnsureInitialized !== true) {
      await this.initialize()
    }
    const credential = this.credential()
    if (credential === undefined || credential.trim() === '') {
      throw new Error('ccteam MCP credential is not configured. Set the enrollment string in DSH Settings or start this profile through ccteam.')
    }

    const id = randomUUID()
    let response: Response
    try {
      response = await this.fetchImpl(`${this.daemonUrl}/mcp`, {
        method: 'POST',
        headers: {
          'content-type': 'application/json',
          'authorization': `Bearer ${credential}`,
          ...(this.sessionId === undefined ? {} : { 'mcp-session-id': this.sessionId }),
        },
        body: JSON.stringify({
          jsonrpc: '2.0',
          id,
          method,
          params,
        }),
      })
    } catch (error) {
      throw new Error(`ccteam daemon unreachable at ${this.daemonUrl}. Run: ccteam start. ${errorMessage(error)}`)
    }

    const headerSessionId = response.headers.get('mcp-session-id') ?? response.headers.get('Mcp-Session-Id')
    if (headerSessionId !== null && headerSessionId.trim() !== '') {
      this.sessionId = headerSessionId
    }

    const text = await response.text()
    let payload: unknown
    try {
      payload = text.length === 0 ? undefined : JSON.parse(text)
    } catch {
      throw new Error(`ccteam MCP returned invalid JSON (${response.status}): ${text}`)
    }

    if (!response.ok) {
      const message = (jsonRpcErrorMessage(payload) ?? text.trim()) || response.statusText
      throw new Error(message)
    }
    if (!isRecord(payload)) {
      throw new Error('ccteam MCP returned a non-object JSON-RPC response')
    }
    const rpc = payload as unknown as JsonRpcResponse
    if ('error' in rpc && rpc.error !== undefined) {
      throw new Error(rpc.error.message)
    }
    return (rpc as JsonRpcSuccess).result
  }

  close(): void {
    this.closed = true
  }

  private assertOpen(): void {
    if (this.closed) throw new Error('ccteam MCP client is disposed')
  }
}

/**
 * One MCP client per distinct daemon credential: a bearer IS an identity on the
 * daemon, so two hires sharing one `Mcp-Session-Id` would share one ledger node.
 * The enrollment client (mode 2, hand-installed plugin) stays shared.
 */
export class CcteamMcpClientPool {
  private readonly daemonUrl: () => string
  private readonly enrollment: () => string | undefined
  private readonly credentials: SessionCredentialStore | undefined
  private readonly clientName: string
  private readonly clientVersion: string
  private readonly fetchImpl: typeof fetch | undefined
  private readonly byCredential = new Map<string, CcteamMcpClient>()
  private enrollmentClient: CcteamMcpClient | undefined
  private readonly offRemoved: (() => void) | undefined

  constructor(options: {
    daemonUrl: () => string
    enrollment: () => string | undefined
    credentials?: SessionCredentialStore
    clientName: string
    clientVersion: string
    fetchImpl?: typeof fetch
  }) {
    this.daemonUrl = options.daemonUrl
    this.enrollment = options.enrollment
    this.credentials = options.credentials
    this.clientName = options.clientName
    this.clientVersion = options.clientVersion
    this.fetchImpl = options.fetchImpl
    this.offRemoved = options.credentials?.onRemoved((_sessionId, removed) => {
      if (removed.bearer === undefined) return
      const key = this.key(this.urlFor(removed.mcpUrl), removed.bearer)
      this.byCredential.get(key)?.close()
      this.byCredential.delete(key)
    })
  }

  /** Resolve the caller: its own session bearer when known, else enrollment. */
  clientFor(exec: ToolRunContext): CcteamMcpClient {
    const meta = this.credentials?.get(sessionIdOfAgent(exec.agent))
    const bearer = meta?.bearer
    if (bearer === undefined || bearer.trim() === '') return this.forEnrollment()
    return this.forCredential(this.urlFor(meta?.mcpUrl), bearer)
  }

  private forEnrollment(): CcteamMcpClient {
    if (this.enrollmentClient === undefined) {
      this.enrollmentClient = this.build(this.daemonUrl(), this.enrollment)
    }
    return this.enrollmentClient
  }

  private forCredential(daemonUrl: string, bearer: string): CcteamMcpClient {
    const key = this.key(daemonUrl, bearer)
    const cached = this.byCredential.get(key)
    if (cached !== undefined) return cached
    const client = this.build(daemonUrl, () => bearer)
    this.byCredential.set(key, client)
    return client
  }

  private build(daemonUrl: string, credential: () => string | undefined): CcteamMcpClient {
    return new CcteamMcpClient({
      daemonUrl,
      credential,
      clientName: this.clientName,
      clientVersion: this.clientVersion,
      ...(this.fetchImpl === undefined ? {} : { fetchImpl: this.fetchImpl }),
    })
  }

  private urlFor(mcpUrl: string | undefined): string {
    const explicit = mcpUrl === undefined ? '' : mcpUrl.trim()
    if (explicit === '') return this.daemonUrl()
    // `_meta.ccteam.mcpUrl` is the ENDPOINT url (`http://…:7331/mcp`), the same
    // shape ccteam writes into every vendor's curated MCP config. The client
    // appends `/mcp` to its base itself, so keep the base here — forwarding the
    // endpoint verbatim double-suffixed every per-session call to `/mcp/mcp`,
    // which is not the exempt MCP route: with web auth enabled the daemon
    // answered a plain-text 401 `auth required` (owner-reported real-machine
    // regression, v0.10.3).
    return explicit.replace(/\/+$/, '').replace(/\/mcp$/, '')
  }

  private key(daemonUrl: string, bearer: string): string {
    return `${daemonUrl} ${bearer}`
  }

  close(): void {
    this.offRemoved?.()
    this.enrollmentClient?.close()
    this.enrollmentClient = undefined
    for (const client of this.byCredential.values()) client.close()
    this.byCredential.clear()
  }
}

export function sessionIdOfAgent(agent: DshAgent | undefined): string | undefined {
  if (agent === undefined) return undefined
  if (typeof agent.id === 'string' && agent.id !== '') return agent.id
  const sessionId = agent.session?.id
  return typeof sessionId === 'string' && sessionId !== '' ? sessionId : undefined
}

export interface ToolRunContext {
  agent?: DshAgent
  signal?: AbortSignal
  [key: string]: unknown
}

export interface DshAgent {
  id?: string
  session?: { id?: string; events?: unknown[] }
  inbox?: { remove?(messageId: string): boolean }
  followup?(message: unknown): void
  whenIdle?(): Promise<void>
  cancel?(cause: { kind: string }, options?: { keepInbox?: boolean }): void
  [key: string]: unknown
}

export interface ToolRegistryContext {
  tools: {
    register(definition: DshToolDefinition): () => void
  }
  effect?<T extends (() => void | Promise<void>) | void>(setup: () => T, label?: string): () => void
}

export interface DshToolDefinition {
  name: string
  description: string
  parameters: unknown
  output: {
    schema: Record<string, unknown>
    render(args: unknown, value: McpToolResult): ContentBlock[]
  }
  execute(args: unknown, exec: ToolRunContext): Promise<McpToolResult>
}

export interface DelegationNotifier {
  maybeNotify(toolName: string, args: unknown, result: McpToolResult, exec: ToolRunContext, client: CcteamMcpClient): void
}

/** Resolves which daemon identity a tool call runs under. */
export type McpClientForExec = (exec: ToolRunContext) => CcteamMcpClient

interface McpToolDefinition {
  name: string
  description: string
  inputSchema: unknown
}

export const CCTEAM_TOOL_DEFINITIONS: McpToolDefinition[] = [
  {
    name: 'status',
    description: 'Which agents this project\'s host can hire and what the team spent today. Brief by default; `detail` adds model ids + effort ladders (models), install/auth/budget per vendor (vendors), your routing notes (routing), or everything (full).',
    inputSchema: {
      type: 'object',
      properties: {
        detail: { type: 'string', enum: ['brief', 'models', 'vendors', 'routing', 'full'], description: 'Default brief.' },
      },
    },
  },
  {
    name: 'grok_claude_codex_kimi',
    description: 'Alias of `status` (brief): which agents — grok, claude, codex, kimi, opencode, pi, dsh — this machine can hire.',
    inputSchema: {
      type: 'object',
      properties: {},
    },
  },
  {
    name: 'chat_send_file',
    description: 'Send a local file (image or document) to your own bound chat — a chat user cannot open a path.',
    inputSchema: {
      type: 'object',
      properties: {
        path: { type: 'string', description: 'Absolute path on the daemon\'s filesystem.' },
        caption: { type: 'string', description: 'Optional caption.' },
        kind: { type: 'string', enum: ['photo', 'document'], description: 'Default: images → photo, else document.' },
      },
      required: ['path'],
    },
  },
  {
    name: 'agent',
    description: 'Hire an agent (claude, codex, grok, opencode, kimi, pi, dsh) or task one you already have. No `sid` → spawn a new session and dispatch `task` to it; with `sid` → follow up on that session. `wait` returns the answer inline; 0 (default) is async: one completion notification when the task\'s turn ends, or poll `agent_read{sid}` when the reply says notify_deliverable:false. Tell children to answer tersely, never to dump code or diffs.',
    inputSchema: {
      type: 'object',
      properties: {
        task: { type: 'string', description: 'Task text, forwarded verbatim as a user turn.' },
        sid: { type: 'string', description: 'Existing session to task; omit to hire a new one.' },
        vendor: {
          type: 'string',
          enum: ['claude', 'codex', 'grok', 'opencode', 'kimi', 'pi', 'dsh'],
          description: 'Harness for a new session (default claude).',
        },
        wait: { type: 'integer', description: 'Seconds to block inline, 0-240 (default 0 = async).' },
        model: { type: 'string', description: 'Model id, passed to the vendor verbatim.' },
        effort: { type: 'string', description: 'Reasoning-effort token, passed verbatim to the vendor.' },
        role: { type: 'string', description: 'Work-role `.claude/agents/<role>.md`; omit for roleless.' },
        project: { type: 'string', description: 'Workspace slug. Required on an enrolled client\'s first call.' },
        title: { type: 'string', description: 'Ledger label (<=80 chars); never sent to the agent.' },
        notify: {
          type: 'string',
          enum: ['final', 'brief', 'all', 'off'],
          description: 'Turn-end wake: final (2000-char excerpt, default), brief (500), all, off.',
        },
        tools: {
          type: 'string',
          enum: ['full', 'read', 'none'],
          description: 'New session\'s ccteam tool face (default full).',
        },
        mode: { type: 'string', description: 'Vendor session mode. DSH only: standard|ptc|minimal|creator.' },
        permission_mode: {
          type: 'string',
          enum: ['skip', 'hitl'],
          description: 'hitl asks your chat to approve tool calls (default skip).',
        },
        idempotency_key: { type: 'string', description: 'Retry key: a retry replays the original call (~1h).' },
        parent_sid: { type: 'string', description: 'Your own sid when ccteam does not manage you.' },
      },
      required: ['task'],
    },
  },
  {
    name: 'agent_read',
    description: 'Read the team. No `sid` → roster of sessions you can reach, most recently active first; a `released` row is idle-but-real and resumes on your next `agent{sid}` call, so reuse it instead of spawning a twin. With `sid` → that session\'s transcript, newest first unless `since` pages forward; empty means no answer yet.',
    inputSchema: {
      type: 'object',
      properties: {
        sid: { type: 'string', description: 'Read this session\'s transcript instead of the roster.' },
        n: { type: 'integer', description: 'Max rows/turns (default 10, max 500).' },
        tail: { type: 'boolean', description: 'With `sid`: newest first (default true; false + `since` pages forward).' },
        since: { type: 'string', description: 'With `sid`: only turns after this turn_id cursor.' },
        max_chars: { type: 'integer', description: 'With `sid`: char budget across returned turns (default 4000, 500-50000).' },
        project: { type: 'string', description: 'Roster filter: this project slug only.' },
        activity: {
          type: 'string',
          enum: ['working', 'idle', 'stale', 'stuck', 'all'],
          description: 'Roster filter (default all).',
        },
        tree: { type: 'boolean', description: 'Roster: add delegation topology over the returned rows.' },
      },
    },
  },
  {
    name: 'agent_stop',
    description: 'Stop a session you delegated. Explicit command, never a proactive kill; `agent_read{sid}` still reads its transcript.',
    inputSchema: {
      type: 'object',
      properties: {
        sid: { type: 'string', description: 'Session to stop.' },
      },
      required: ['sid'],
    },
  }
]

export function registerCcteamTools(ctx: ToolRegistryContext, clientFor: McpClientForExec, notifier?: DelegationNotifier): void {
  for (const definition of CCTEAM_TOOL_DEFINITIONS) {
    const tool = toDshTool(definition, clientFor, notifier)
    if (typeof ctx.effect === 'function') {
      ctx.effect(() => ctx.tools.register(tool), `ccteam.tool.${definition.name}`)
    } else {
      ctx.tools.register(tool)
    }
  }
}

function toDshTool(definition: McpToolDefinition, clientFor: McpClientForExec, notifier?: DelegationNotifier): DshToolDefinition {
  return {
    name: definition.name,
    description: definition.description,
    parameters: definition.inputSchema,
    output: {
      schema: {},
      render(_args, value) {
        return value.content
      },
    },
    async execute(args, exec) {
      const client = clientFor(exec)
      const result = await client.callTool(definition.name, args)
      notifier?.maybeNotify(definition.name, args, result, exec, client)
      return result
    },
  }
}

export class CcteamCompletionNotifier implements DelegationNotifier {
  private readonly pollIntervalMs: number
  private readonly maxPolls: number
  private readonly sleep: (ms: number) => Promise<void>
  private closed = false

  constructor(options?: { pollIntervalMs?: number; maxPolls?: number; sleep?: (ms: number) => Promise<void> }) {
    this.pollIntervalMs = options?.pollIntervalMs ?? 5000
    this.maxPolls = options?.maxPolls ?? 720
    this.sleep = options?.sleep ?? ((ms) => new Promise(resolve => setTimeout(resolve, ms)))
  }

  maybeNotify(toolName: string, args: unknown, result: McpToolResult, exec: ToolRunContext, client: CcteamMcpClient): void {
    if (this.closed) return
    // `agent` always carries a task: no `sid` hires and dispatches in one call,
    // with `sid` it is a follow-up. Either way the parent wants the answer back.
    if (toolName !== 'agent') return
    const origin = exec.agent
    if (!isAgentWithSession(origin)) return
    const sid = extractDelegatedSid(args, result)
    if (sid === undefined) return
    void this.pollAndFollowup(origin, sid, result, client).catch(() => undefined)
  }

  private async pollAndFollowup(origin: DshAgent, sid: string, initial: McpToolResult, client: CcteamMcpClient): Promise<void> {
    let latest = resultJson(initial)
    if (!isTerminalDispatch(latest)) {
      for (let i = 0; i < this.maxPolls; i++) {
        if (this.closed) return
        await this.sleep(this.pollIntervalMs)
        if (this.closed) return
        const collected = await client.callTool('agent_read', {
          sid,
          tail: true,
          n: 1,
        })
        latest = resultJson(collected)
        if (isTerminalCollect(latest)) break
      }
    }
    const text = renderDelegationSummary(sid, latest)
    if (text.trim() === '') return
    if (this.closed) return
    if (typeof origin.whenIdle === 'function') {
      await origin.whenIdle()
    }
    if (this.closed) return
    origin.followup?.(createUserTextMessage(text))
  }

  close(): void {
    this.closed = true
  }
}

/** A ccteam-minted user turn; its `id` is what turn attribution binds on. */
export interface UserTextMessage {
  readonly id: string
  readonly role: 'user'
  readonly content: readonly [{ readonly type: 'text'; readonly text: string }]
  readonly source: { readonly kind: 'user' }
}

export function createUserTextMessage(text: string): UserTextMessage {
  return Object.freeze({
    id: randomUUID(),
    role: 'user',
    content: Object.freeze([Object.freeze({ type: 'text', text })]),
    source: Object.freeze({ kind: 'user' }),
  }) as UserTextMessage
}

function extractDelegatedSid(args: unknown, result: McpToolResult): string | undefined {
  // A follow-up names its target; a hire learns the fresh sid from the reply.
  if (isRecord(args) && typeof args.sid === 'string' && args.sid.trim() !== '') {
    return args.sid
  }
  const body = resultJson(result)
  if (isRecord(body) && typeof body.sid === 'string') return body.sid
  return undefined
}

function isAgentWithSession(agent: DshAgent | undefined): agent is DshAgent {
  return typeof agent?.session?.id === 'string' && typeof agent.followup === 'function'
}

function isTerminalDispatch(value: unknown): boolean {
  if (!isRecord(value)) return false
  return value.status === 'completed' || value.status === 'failed'
}

function isTerminalCollect(value: unknown): boolean {
  if (!isRecord(value)) return false
  if (value.outcome === 'failed') return true
  return value.activity !== 'working'
}

function renderDelegationSummary(sid: string, value: unknown): string {
  const body = isRecord(value) ? value : {}
  const failure = body.outcome === 'failed' || body.status === 'failed'
  const title = failure ? `ccteam session ${sid} failed.` : `ccteam session ${sid} completed.`
  const resultText = typeof body.result_text === 'string'
    ? body.result_text
    : latestTurnText(body.turns)
  const details = resultText ?? JSON.stringify(value, null, 2)
  return `${title}\n\n${details}`
}

function latestTurnText(turns: unknown): string | undefined {
  if (!Array.isArray(turns) || turns.length === 0) return undefined
  const last = turns[turns.length - 1]
  if (isRecord(last)) {
    for (const key of ['text', 'content', 'message', 'result_text']) {
      const value = last[key]
      if (typeof value === 'string' && value.trim() !== '') return value
    }
  }
  return JSON.stringify(last)
}

function resultJson(result: McpToolResult): unknown {
  const text = result.content
    .map(block => typeof block.text === 'string' ? block.text : '')
    .join('\n')
    .trim()
  if (text === '') return result
  try {
    return JSON.parse(text)
  } catch {
    return { result_text: text }
  }
}

function normalizeContentBlock(value: unknown): ContentBlock {
  if (isRecord(value) && typeof value.type === 'string') {
    return value as ContentBlock
  }
  return { type: 'text', text: String(value) }
}

function jsonRpcErrorMessage(payload: unknown): string | undefined {
  if (!isRecord(payload)) return undefined
  const error = payload.error
  if (isRecord(error) && typeof error.message === 'string') return error.message
  return undefined
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message
  return String(error)
}
