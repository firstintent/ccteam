import { randomUUID } from 'node:crypto'

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

export interface ToolRunContext {
  agent?: DshAgent
  signal?: AbortSignal
  [key: string]: unknown
}

export interface DshAgent {
  id?: string
  session?: { id?: string; events?: unknown[] }
  followup?(message: unknown): void
  whenIdle?(): Promise<void>
  cancel?(cause: { kind: string }): void
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
  maybeNotify(toolName: string, args: unknown, result: McpToolResult, exec: ToolRunContext): void
}

interface McpToolDefinition {
  name: string
  description: string
  inputSchema: unknown
}

export const CCTEAM_TOOL_DEFINITIONS: McpToolDefinition[] = [
  {
    name: 'status',
    description: 'Discovery + health: which of claude / codex / grok / opencode / kimi / pi are installed on your project\'s host, plus per-vendor session_spawn recipes, daemon health, cost/budget, advisory models, and routing notes. Managed Pi sessions get the bridge; plain shell pi does not.',
    inputSchema: {
      type: 'object',
      properties: {},
      required: [],
    },
  },
  {
    name: 'grok_claude_codex_kimi',
    description: 'Alias of status (discovery beacon for hosts that surface tool names only). Which agents this machine can spawn — claude / codex / grok / kimi / opencode / pi — with install/auth state and per-vendor session_spawn recipes. Identical response to status.',
    inputSchema: {
      type: 'object',
      properties: {},
      required: [],
    },
  },
  {
    name: 'chat_send_file',
    description: 'Send a file (image or document) from disk back to YOUR own bound chat (Telegram / Lark / web) — a chat user cannot open a local path, so this is how a generated artifact (chart, report, photo) actually reaches them. Zero addressing params: the daemon resolves your home chat from your session identity. `path` must be on the daemon\'s filesystem. `kind` is inferred from the extension when omitted (png/jpg/jpeg/gif/webp → photo, else document). Delivery reuses the same outbound funnel as text replies (long-message split + durable ledger + failure echo).',
    inputSchema: {
      type: 'object',
      properties: {
        path: { type: 'string', description: 'Absolute path to the file on the daemon\'s filesystem.' },
        caption: { type: 'string', description: 'Optional caption sent with the file.' },
        kind: { type: 'string', enum: ['photo', 'document'], description: 'photo → sendPhoto (compressed image); document → sendDocument (any file). Inferred from the extension when omitted.' },
      },
      required: ['path'],
    },
  },
  {
    name: 'session_spawn',
    description: 'Spawn an agent session — vendor: claude (default) | codex | grok | opencode | kimi | pi — in YOUR OWN project; always mints a NEW s{n} sid. grok = fast live web/X search; claude/codex/pi = coding agents; status shows per-host availability. Pass `task` to dispatch the first task in the same call — identical semantics to session_dispatch. Async managed-parent calls get ONE completion notification when the child\'s turn ends; a hand-started (enrolled) caller has no return transport, gets `notify_deliverable:false`, and must poll `session_collect` (or use `wait_seconds`). The response adds `turn_id` + `status`, plus `result_text`/`elapsed_seconds`/ledger `cost_usd`/`tokens_total` when waited to completion. Instruct children to answer tersely with a structured summary and no code or diff dumps, because answers beyond the return cap are truncated. Auth: your per-session `(sid, secret)` principal — you can only spawn into your own project; the execution host follows the project binding. Returns `{sid, vendor_session_id (vendor-native resume key, may be empty), host, ...}`. Read output later with session_collect{sid, tail:true}.',
    inputSchema: {
      type: 'object',
      properties: {
        role: { type: 'string', description: 'Optional work-role (must exist as `.claude/agents/<role>.md`). Omit or pass "" for a roleless session (bare vendor reads the project CLAUDE.md/AGENTS.md).' },
        vendor: {
          type: 'string',
          enum: ['claude', 'codex', 'grok', 'opencode', 'kimi', 'pi', 'dsh'],
          description: 'Harness vendor (lowercase). Default claude.',
        },
        model: { type: 'string', description: 'Optional explicit model id, passed to the vendor verbatim; overrides the role\'s `model:` frontmatter. Omitted → vendor default. `status` lists each installed vendor\'s observed ids.' },
        effort: { type: 'string', description: 'Optional reasoning-effort token, passed to the vendor verbatim for EVERY vendor — the value set is vendor-specific and the vendor validates it (a bad token fails the spawn with its own error, it is never silently ignored). Omitted → vendor default. `status` lists each installed vendor\'s effort ladder.' },
        project: { type: 'string', description: 'Target project slug. A managed session always spawns into its OWN project and may omit this. A hand-started (enrolled) caller names its workspace here on its first call — that choice sticks for the session, and `status` lists the slugs it can reach. Never inferred from a working directory.' },
        permission_mode: {
          type: 'string',
          enum: ['skip', 'hitl'],
          description: 'Permission posture (default `skip`). `hitl` (human-in-the-loop) makes a non-allowlist tool call pop an approve/deny prompt to the bound IM chat; allowlist/auto-allowed tools never prompt.',
        },
        title: { type: 'string', description: 'Optional short label (≤80 chars) for the ledger / team visualization only — NEVER sent to the agent or concatenated into any prompt.' },
        task: { type: 'string', description: 'Optional FIRST task — dispatched to the fresh child in the same call, exactly like session_dispatch{sid, task} (verbatim user turn, no injection). Omit to spawn only.' },
        wait_seconds: { type: 'integer', description: 'With `task`: request 0–600 seconds (default 0 = async); effective inline wait is capped at 240s. Use inline wait for health probes/short tasks; keep long/repo tasks async (managed parents get a notification; a hand-started agent polls collect). Pending/timeout never cancels the child.' },
        notify: { type: ['string', 'boolean'], description: 'With `task`: for a managed parent, `final` (default) wakes it ONCE when the child\'s vendor turn ends; `all` wakes it on every assistant message (debug firehose); `off` = ledger-only. A hand-started (enrolled) caller has no notification return transport: the response reports `notify_deliverable:false`; poll session_collect. Booleans still parse: true→final, false→off.' },
        idempotency_key: { type: 'string', description: 'Optional client key. A retry with the same key (per-project, within ~1h) replays the ORIGINAL spawn (same sid + same dispatch outcome, zero side effects) instead of creating a second session — safe against MCP-client timeouts. In-memory only: a daemon restart forgets keys.' },
        parent_sid: { type: 'string', description: 'Your OWN sid, when you are a plain local session ccteam mirrors in its ledger (session_list shows you). A managed session never needs this — its parent comes from its principal — but a plain one is anonymous to the bridge, so without it the child mounts as a root and the delegation tree loses the edge. Validated: an unknown sid is an error, not a silent root.' },
      },
      required: [],
    },
  },
  {
    name: 'session_dispatch',
    description: 'Dispatch a task to a session by `sid` (from session_spawn / session_list); the target must run in YOUR OWN project. `task` is forwarded VERBATIM as a user turn (NO system-prompt injection). Async managed-parent calls get ONE completion notification at the vendor turn boundary; a hand-started (enrolled) caller has no return transport, gets `notify_deliverable:false`, and must poll `session_collect` (or use `wait_seconds`). Inline completion returns `{status:"completed"|"failed", result_text, error_kind?, error?, elapsed_seconds, cost_usd?, tokens_total?}`; timeout returns `{status:"pending"}` and never cancels the child. Instruct children to answer tersely with a structured summary and no code or diff dumps, because answers beyond the return cap are truncated. Dispatch to yourself or an ancestor is rejected (cycle). Explicit dispatch, never a proactive kill.',
    inputSchema: {
      type: 'object',
      properties: {
        sid: { type: 'string', description: 'Gateway session id (`s{n}`) from session_spawn / session_list.' },
        task: { type: 'string', description: 'Task / instruction text, forwarded verbatim as a user turn.' },
        wait_seconds: { type: 'integer', description: 'Request 0–600 seconds (default 0 = async); effective inline wait is capped at 240s. Use inline wait for health probes/short tasks; keep long/repo tasks async (managed parents get a notification; a hand-started agent polls collect). Pending/timeout never cancels the child.' },
        notify: { type: ['string', 'boolean'], description: 'For a managed parent, `final` (default) wakes it ONCE when the child\'s vendor turn ends; `all` wakes it on every assistant message (debug firehose); `off` = ledger-only. A hand-started (enrolled) caller has no notification return transport: the response reports `notify_deliverable:false`; poll session_collect. Booleans still parse: true→final, false→off.' },
        title: { type: 'string', description: 'Optional short label (≤80 chars) for the notification / ledger only — NEVER concatenated into the task or any prompt.' },
        idempotency_key: { type: 'string', description: 'Optional client key. A retry with the same key (per-target-child, within ~1h) replays the ORIGINAL dispatch (same turn) instead of double-dispatching. In-memory only: a daemon restart forgets keys.' },
      },
      required: ['sid', 'task'],
    },
  },
  {
    name: 'session_collect',
    description: 'Collect (poll) a session\'s transcript by `sid`. Authenticated by your `(sid, secret)` principal; the target `sid` must run in YOUR OWN project (cross-project collect is rejected). Tails `<project>/.ccteam/chat/<sid>/turns.jsonl` (the ccteam-owned mirror, keyed by sid so parallel sessions never bleed) and returns assistant-side turns; a terminal failure carries `outcome:"failed"`, `error_kind`, and `error`. Also returns the child\'s `vendor_session_id` (native resume key), `activity` (`working` = mid-turn / `idle` = turn done / `stale` / `stuck`), and accrued ledger (`cost_usd` when priced, `tokens_total` when reported). Pass `since` to return only turns AFTER that turn id. Default paging is OLDEST-first; pass `tail:true` for the NEWEST `n` turns. Returns an empty `turns` array when the target hasn\'t answered yet.',
    inputSchema: {
      type: 'object',
      properties: {
        sid: { type: 'string', description: 'Gateway session id (`s{n}`) to collect from.' },
        since: { type: 'string', description: 'Optional turn_id cursor — return only assistant turns recorded AFTER this id.' },
        n: { type: 'integer', description: 'Max turns to return (default 20). Applied after the `since` cursor filter.' },
        tail: { type: 'boolean', description: 'When true, return the NEWEST `n` turns (after the `since` filter) instead of the oldest — use to grab the final answer of a long transcript without paging.' },
        max_chars: { type: 'integer', description: 'Maximum total characters across returned turn contents (default 10000; clamped to 500–50000). Longer contents retain a 70% head / 30% tail excerpt with an explicit ledger pointer.' },
      },
      required: ['sid'],
    },
  },
  {
    name: 'session_list',
    description: 'List the gateway\'s live sessions (the same `s{n}` namespace session_spawn allocates), most recently active first, capped at `limit` (default 30; `truncated`/`total` say when the cap bit). Authenticated by your `(sid, secret)` principal. Each row carries `sid`, `project`, `vendor`, `activity` (`working` = mid-turn / `idle` / `stale` / `stuck` — the honest busy signal), `last_active`, plus — when set — `role`, `is_self` (YOUR OWN row — the only way to find yourself here), `current` (that session is the active one of some chat — NOT you), `waiting_approval` (hitl blocked on a human), the delegation `parent_sid`/`delegation_depth`, non-local `host`, `cost_usd`, `tokens_total` (raw token ledger, present even for vendors with no USD price table), and `title` (null/empty fields are omitted). The response also includes a `tree` field (roots → children by `parent_sid`, over the filtered set) so you can see the delegation topology. Filter with `project` / `activity` to keep the listing small. Use this to find a `sid` to dispatch to or collect from.',
    inputSchema: {
      type: 'object',
      properties: {
        project: { type: 'string', description: 'Only list sessions of this project slug.' },
        activity: { type: 'string', enum: ['working', 'idle', 'stale', 'stuck', 'all'], description: 'Only list sessions with this activity state (default `all`).' },
        limit: { type: 'integer', description: 'Max rows returned, most recently active first (default 30, clamped to 1–500).' },
      },
      required: [],
    },
  },
  {
    name: 'session_stop',
    description: 'Stop a session by `sid` (deregister + close it). Authenticated by your `(sid, secret)` principal; the target `sid` must run in YOUR OWN project (cross-project stop is rejected). This is an EXPLICIT command, NOT a proactive kill — it never file-purges the transcript, so a later session_collect of an already-recorded `turns.jsonl` still works until cleanup. An unknown sid is an error.',
    inputSchema: {
      type: 'object',
      properties: {
        sid: { type: 'string', description: 'Gateway session id (`s{n}`) to stop.' },
      },
      required: ['sid'],
    },
  },
]

export function registerCcteamTools(ctx: ToolRegistryContext, client: CcteamMcpClient, notifier?: DelegationNotifier): void {
  for (const definition of CCTEAM_TOOL_DEFINITIONS) {
    const tool = toDshTool(definition, client, notifier)
    if (typeof ctx.effect === 'function') {
      ctx.effect(() => ctx.tools.register(tool), `ccteam.tool.${definition.name}`)
    } else {
      ctx.tools.register(tool)
    }
  }
}

function toDshTool(definition: McpToolDefinition, client: CcteamMcpClient, notifier?: DelegationNotifier): DshToolDefinition {
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
      const result = await client.callTool(definition.name, args)
      notifier?.maybeNotify(definition.name, args, result, exec)
      return result
    },
  }
}

export class CcteamCompletionNotifier implements DelegationNotifier {
  private readonly client: CcteamMcpClient
  private readonly pollIntervalMs: number
  private readonly maxPolls: number
  private readonly sleep: (ms: number) => Promise<void>
  private closed = false

  constructor(client: CcteamMcpClient, options?: { pollIntervalMs?: number; maxPolls?: number; sleep?: (ms: number) => Promise<void> }) {
    this.client = client
    this.pollIntervalMs = options?.pollIntervalMs ?? 5000
    this.maxPolls = options?.maxPolls ?? 720
    this.sleep = options?.sleep ?? ((ms) => new Promise(resolve => setTimeout(resolve, ms)))
  }

  maybeNotify(toolName: string, args: unknown, result: McpToolResult, exec: ToolRunContext): void {
    if (this.closed) return
    if (toolName !== 'session_spawn' && toolName !== 'session_dispatch') return
    if (toolName === 'session_spawn' && (!isRecord(args) || typeof args.task !== 'string' || args.task.trim() === '')) return
    const origin = exec.agent
    if (!isAgentWithSession(origin)) return
    const sid = extractDelegatedSid(toolName, args, result)
    if (sid === undefined) return
    void this.pollAndFollowup(origin, sid, result).catch(() => undefined)
  }

  private async pollAndFollowup(origin: DshAgent, sid: string, initial: McpToolResult): Promise<void> {
    let latest = resultJson(initial)
    if (!isTerminalDispatch(latest)) {
      for (let i = 0; i < this.maxPolls; i++) {
        if (this.closed) return
        await this.sleep(this.pollIntervalMs)
        if (this.closed) return
        const collected = await this.client.callTool('session_collect', {
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

export function createUserTextMessage(text: string): unknown {
  return Object.freeze({
    id: randomUUID(),
    role: 'user',
    content: [{ type: 'text', text }],
    source: { kind: 'user' },
  })
}

function extractDelegatedSid(toolName: string, args: unknown, result: McpToolResult): string | undefined {
  if (toolName === 'session_dispatch' && isRecord(args) && typeof args.sid === 'string') {
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
