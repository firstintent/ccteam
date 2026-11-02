import type {} from '@deepseek-ai/dsh-agent-default-model'
import type { PermissionPresetService } from '@deepseek-ai/dsh-permission-presets'
import type { WorkspaceRegistry } from '@deepseek-ai/dsh-workspace'
import type { Context } from '@deepseek-ai/cordis'
import type { AgentRegistry, AgentHandle, AgentOptions, AgentSetup, Agent, AssistantStreamFrame } from '@deepseek-ai/dsh-agent'
import type { Session, SessionId, SessionEvent, SessionEventMap } from '@deepseek-ai/dsh-session'
import type { AgentPresetRegistry } from '@deepseek-ai/dsh-agent-preset-registry'
import { randomUUID } from 'node:crypto'
import { mkdirSync, unlinkSync } from 'node:fs'
import { createServer, type Server, type Socket } from 'node:net'
import { dirname } from 'node:path'
import { parseCcteamMeta, type SessionCredentialStore } from './credentials.js'
import { createUserTextMessage, sessionIdOfAgent, type ContentBlock, type DshAgent } from './tools.js'

export type DshAgents = Pick<AgentRegistry, 'create' | 'resume' | 'get'>
export type DshAgentHandle = AgentHandle

export type DshWorkspace = Awaited<ReturnType<WorkspaceRegistry['create']>>

/**
 * DSH's agent-preset roster (Cordis service `agentPresets`,
 * `@deepseek-ai/dsh-agent-presets`). The web bundle DISABLES the host-plane
 * vendor tools and moves them behind presets, so an agent created without
 * `setup: mount(...)` has no bash/read/write at all — only globally-registered
 * tools like ccteam's own (real-machine `unknown tool "bash"` regression).
 * The vendor owns its roster and default; ccteam forwards native preset ids.
 */
export type DshAgentPresets = Pick<AgentPresetRegistry, 'resolve' | 'mount' | 'presetForSession'>

/**
 * DSH's permission-preset roster (Cordis service `permissionPresets`).
 * `set(session, name)` records the preset and rewrites the sandbox/approval
 * knobs. ccteam hires default to `danger-full-access` (full file access, no
 * approval prompts — the DSH counterpart of ccteam's `skip` posture); a
 * `hitl` hire keeps the vendor's pinned default so approvals keep flowing.
 */
type DshPermissionPresets = Pick<PermissionPresetService, 'set'>

const HIRE_PERMISSION_PRESET = 'danger-full-access'

export type DshWorkspaceRegistry = Pick<WorkspaceRegistry, 'create'>

export type TransportContext = Pick<Context, 'agents' | 'get' | 'agentDefaultModel' | 'on' | 'effect' | 'logger'>

export interface DshSocketTransportOptions {
  version: string
  /** Unix socket path this plugin listens on for ccteam ACP peers. */
  socketPath: string
  credentials: SessionCredentialStore
}

export interface DshTransportOptions {
  version: string
  input: NodeJS.ReadableStream
  output: NodeJS.WritableStream
  credentials?: SessionCredentialStore
  workspaces?: WorkspaceMounter
}

type JsonRpcId = string | number | null

interface JsonRpcRequest {
  jsonrpc?: '2.0'
  id?: JsonRpcId
  method?: string
  params?: unknown
  result?: unknown
  error?: unknown
}

interface Inflight {
  resolve: (value: PromptResult) => void
  reject: (error: RpcError) => void
  usage: Usage
  /** Id of the user message this transport queued; attribution binds on it. */
  messageId: string
  /** The turn that claimed our message — the only turn we report on. */
  ownedTurn?: number
  /** Our message was seen with no turn open yet; bind the next `turn/start`. */
  bindNextTurn: boolean
  /** Tool calls of the owned turn, for `tool/result` correlation. */
  toolCalls: Set<string>
}

interface SessionRecord {
  agent: DshAgent
  stream?: { attemptId: string; revision: number; turn: number }
  thoughtStreamed?: boolean
  /** Turn currently open on this session, ccteam-owned or human-owned. */
  openTurn?: number
  inflight?: Inflight
}

interface PromptResult {
  stopReason: string
  _meta?: Record<string, unknown>
  usage?: Record<string, unknown>
}

interface Usage {
  inputTokens?: number
  outputTokens?: number
  cachedReadTokens?: number
  reasoningTokens?: number
}

class RpcError extends Error {
  readonly code: number
  readonly data?: unknown

  constructor(message: string, code = -32603, data?: unknown) {
    super(message)
    this.name = 'RpcError'
    this.code = code
    this.data = data
  }
}

/**
 * Serializes `workspaceRegistry.create` the way the DSH host does: concurrent
 * creates of one path would otherwise race to own the same canonical directory.
 */
export class WorkspaceMounter {
  private readonly ctx: TransportContext
  private chain: Promise<unknown> = Promise.resolve()

  constructor(ctx: TransportContext) {
    this.ctx = ctx
  }

  async mount(cwd: string, sessionId: string): Promise<void> {
    const registry =
      typeof this.ctx.get === 'function'
        ? (this.ctx.get('workspaceRegistry') as DshWorkspaceRegistry | undefined)
        : undefined
    if (registry === undefined) {
      this.ctx.logger?.warn(`ccteam dsh transport: no workspaceRegistry, session ${sessionId} stays ungrouped`)
      return
    }
    const operation = this.chain.then(() => registry.create(cwd))
    this.chain = operation.then(() => undefined, () => undefined)
    const workspace = await operation
    await workspace.attachSession(sessionId as SessionId)
  }
}

/** Unix-socket ACP listener: one isolated {@link DshAcpServer} per connection. */
export class DshSocketTransport {
  private readonly ctx: TransportContext
  private readonly options: DshSocketTransportOptions
  private readonly workspaces: WorkspaceMounter
  private readonly peers = new Set<{ socket: Socket; teardown: () => Promise<void> }>()
  private server: Server | undefined
  private offDisposed: (() => void) | undefined
  private closed = false

  constructor(ctx: TransportContext, options: DshSocketTransportOptions) {
    this.ctx = ctx
    this.options = options
    this.workspaces = new WorkspaceMounter(ctx)
  }

  /** Bind the socket. Never throws: a bind failure warns and leaves the plugin working. */
  async listen(): Promise<void> {
    if (this.closed || this.server !== undefined) return
    const path = this.options.socketPath
    try {
      mkdirSync(dirname(path), { recursive: true })
    } catch {
      // the parent directory is the caller's business; listen reports the real failure
    }
    try {
      unlinkSync(path)
    } catch {
      // no stale socket file to remove
    }
    const server = createServer(socket => this.accept(socket))
    this.offDisposed = this.ctx.on?.('session/disposed', ((session: unknown) => {
      const sessionId = sessionIdFromSession(session)
      if (sessionId !== undefined) this.options.credentials.delete(sessionId)
    }) as never)
    await new Promise<void>(resolve => {
      const onListenError = (error: unknown) => {
        this.ctx.logger?.warn(`ccteam dsh transport cannot listen on ${path}: ${errorMessage(error)}`)
        try {
          server.close()
        } catch {
          // never bound
        }
        resolve()
      }
      server.once('error', onListenError)
      server.listen(path, () => {
        server.off('error', onListenError)
        server.on('error', error => {
          this.ctx.logger?.warn(`ccteam dsh transport socket error: ${errorMessage(error)}`)
        })
        this.server = server
        resolve()
      })
    })
  }

  private accept(socket: Socket): void {
    if (this.closed) {
      socket.destroy()
      return
    }
    const peer = new DshAcpServer(this.ctx, {
      version: this.options.version,
      input: socket,
      output: socket,
      credentials: this.options.credentials,
      workspaces: this.workspaces,
    })
    const entry = { socket, teardown: peer.start() }
    this.peers.add(entry)
    const drop = () => {
      if (!this.peers.delete(entry)) return
      void entry.teardown().catch(() => undefined)
    }
    socket.on('close', drop)
    socket.on('error', error => {
      this.ctx.logger?.warn(`ccteam dsh transport peer error: ${errorMessage(error)}`)
    })
  }

  async close(): Promise<void> {
    this.closed = true
    this.offDisposed?.()
    this.offDisposed = undefined
    const server = this.server
    this.server = undefined
    const peers = [...this.peers]
    this.peers.clear()
    for (const peer of peers) peer.socket.destroy()
    await Promise.allSettled(peers.map(peer => peer.teardown()))
    if (server !== undefined) {
      await new Promise<void>(resolve => server.close(() => resolve()))
    }
  }
}

/**
 * Start the socket transport, scoped to the plugin effect when available.
 * @returns a teardown that closes the listener and its peers.
 */
export function startDshSocketTransport(ctx: TransportContext, options: DshSocketTransportOptions): () => Promise<void> {
  const transport = new DshSocketTransport(ctx, options)
  const setup = () => {
    void transport.listen()
    return () => transport.close()
  }
  if (typeof ctx.effect === 'function') {
    ctx.effect(setup, 'ccteam.dsh.transport')
  } else {
    setup()
  }
  return () => transport.close()
}

export class DshAcpServer {
  private readonly ctx: TransportContext
  private readonly version: string
  private readonly input: NodeJS.ReadableStream
  private readonly output: NodeJS.WritableStream
  private readonly credentials: SessionCredentialStore | undefined
  private readonly workspaces: WorkspaceMounter
  private readonly sessions = new Map<string, SessionRecord>()
  private readonly pendingClientRequests = new Map<string, {
    resolve: (value: unknown) => void
    reject: (error: RpcError) => void
  }>()
  private buffer = ''
  private closed = false

  constructor(ctx: TransportContext, options: DshTransportOptions) {
    this.ctx = ctx
    this.version = options.version
    this.input = options.input
    this.output = options.output
    this.credentials = options.credentials
    this.workspaces = options.workspaces ?? new WorkspaceMounter(ctx)
  }

  start(): () => Promise<void> {
    const onData = (chunk: Buffer | string) => this.receive(chunk)
    const onClose = () => { this.closed = true }
    this.input.on('data', onData)
    this.input.on('end', onClose)
    this.input.on('error', onClose)

    const offSession = this.ctx.on('session/event', (session, event) => {
      this.onSessionEvent(session, event)
    })
    const offStream = this.ctx.on('agent/assistant-stream', ({ agent, frame }) => {
      this.onAssistantStream(agent, frame)
    })
    const offError = this.ctx.on?.('agent/error', ((payload: unknown) => {
      this.onAgentError(payload)
    }) as never)
    const offApproval = this.ctx.on?.('approval/request', ((request: unknown, next: () => unknown) => {
      return this.onApprovalRequest(request, next)
    }) as never)

    return async () => {
      this.closed = true
      this.input.off('data', onData)
      this.input.off('end', onClose)
      this.input.off('error', onClose)
      offSession?.()
      offStream?.()
      offError?.()
      offApproval?.()
      // Agents stay live and un-cancelled: the human at the DSH UI owns them too.
      const records = [...this.sessions.values()]
      this.sessions.clear()
      for (const record of records) {
        const inflight = record.inflight
        record.inflight = undefined
        inflight?.reject(new RpcError('ccteam transport connection closed', -32603))
      }
    }
  }

  private receive(chunk: Buffer | string): void {
    this.buffer += chunk.toString()
    for (;;) {
      const newline = this.buffer.indexOf('\n')
      if (newline < 0) break
      const line = this.buffer.slice(0, newline).trim()
      this.buffer = this.buffer.slice(newline + 1)
      if (line.length === 0) continue
      void this.handleLine(line).catch(error => {
        this.ctx.logger?.warn(`ccteam dsh transport line failed: ${errorMessage(error)}`)
      })
    }
  }

  private async handleLine(line: string): Promise<void> {
    let message: JsonRpcRequest
    try {
      message = JSON.parse(line) as JsonRpcRequest
    } catch {
      this.writeError(null, -32700, 'parse error')
      return
    }

    if (message.method === undefined) {
      this.handleClientResponse(message)
      return
    }

    if (message.id === undefined) {
      await this.handleNotification(message)
      return
    }

    try {
      const result = await this.handleRequest(message.method, message.params)
      this.write({ jsonrpc: '2.0', id: message.id, result })
    } catch (error) {
      const rpc = error instanceof RpcError ? error : new RpcError(errorMessage(error))
      this.writeError(message.id, rpc.code, rpc.message, rpc.data)
    }
  }

  private handleClientResponse(message: JsonRpcRequest): void {
    if (message.id === undefined) return
    const pending = this.pendingClientRequests.get(String(message.id))
    if (pending === undefined) return
    this.pendingClientRequests.delete(String(message.id))
    if (message.error !== undefined) {
      pending.reject(new RpcError(jsonString(message.error), -32603, message.error))
    } else {
      pending.resolve(message.result)
    }
  }

  private async handleNotification(message: JsonRpcRequest): Promise<void> {
    if (message.method === 'session/cancel') {
      const params = asRecord(message.params)
      const sessionId = stringField(params, 'sessionId')
      if (sessionId === undefined) return
      const record = this.sessions.get(sessionId)
      if (record === undefined) return
      const inflight = record.inflight
      if (inflight === undefined) return
      record.inflight = undefined
      if (this.ownsActiveTurn(record, inflight)) {
        record.agent.cancel?.({ kind: 'user' })
      } else if (record.agent.inbox?.remove?.(inflight.messageId) !== true) {
        // Not still queued (or no inbox surface): fall back to aborting the agent.
        record.agent.cancel?.({ kind: 'user' })
      }
      inflight.resolve({ stopReason: 'cancelled', _meta: { stopReason: 'cancelled' } })
    }
  }

  private async handleRequest(method: string, params: unknown): Promise<unknown> {
    switch (method) {
      case 'initialize':
        return {
          protocolVersion: '0.4',
          agentInfo: { name: 'ccteam-dsh-client', version: this.version },
          agentCapabilities: { loadSession: true },
          authMethods: [],
        }
      case 'session/new':
        return this.newSession(params)
      case 'session/load':
        return this.loadSession(params)
      case 'session/prompt':
        return this.prompt(params)
      case 'session/cancel':
        await this.handleNotification({ method, params })
        return {}
      default:
        throw new RpcError(`method not found: ${method}`, -32601)
    }
  }

  private presetService(): DshAgentPresets | undefined {
    const svc = typeof this.ctx.get === 'function' ? this.ctx.get('agentPresets') : undefined
    const candidate = svc as DshAgentPresets | undefined
    return typeof candidate?.resolve === 'function' && typeof candidate?.mount === 'function'
      ? candidate
      : undefined
  }

  /**
   * Preset for a NEW session: the ccteam-requested id when present, else the
   * vendor's own default. Without the roster service, an explicit request is a
   * hard error (silently creating a toolless agent is the exact bug this
   * exists to prevent), while no request degrades to a bare create with a
   * warning — an ACP-bundle-style runtime keeps its host-plane tools.
   */
  private async composeCreatePreset(
    requested: string | undefined,
    sessionId: string,
  ): Promise<{ id?: string; setup?: (agentCtx: unknown, agent: DshAgent) => Promise<void> }> {
    const presets = this.presetService()
    if (presets === undefined) {
      if (requested !== undefined) {
        throw new RpcError(
          `agent preset "${requested}" was requested but this DSH runtime has no agentPresets service`,
          -32602,
        )
      }
      this.ctx.logger?.warn(
        `ccteam dsh transport: no agentPresets service, session ${sessionId} gets host-plane tools only`,
      )
      return {}
    }
    let id: string | undefined
    try {
      const resolved = await presets.resolve(requested)
      id = typeof resolved?.id === 'string' && resolved.id.trim() !== '' ? resolved.id : requested
    } catch (error) {
      throw errorToRpc(error, `resolve DSH agent preset "${requested ?? '(default)'}"`)
    }
    return {
      id,
      setup: async agentCtx => {
        await presets.mount(agentCtx as Parameters<DshAgentPresets['mount']>[0], id)
      },
    }
  }

  /** DSH owns persisted preset selection, including switches made in its UI. */
  private composeResumeSetup(): AgentSetup | undefined {
    const presets = this.presetService()
    if (presets === undefined) return undefined
    return async (agentCtx, agent) => {
      const stored = presets.presetForSession(agent.session as Session) ?? undefined
      await presets.mount(agentCtx as Parameters<DshAgentPresets['mount']>[0], stored)
    }
  }

  private async newSession(params: unknown): Promise<{ sessionId: string }> {
    const body = requireRecord(params, 'session/new params')
    const cwd = requireString(body, 'cwd')
    const sessionId = randomUUID() as SessionId
    const agentOptions = this.resolveAgentOptions(body.agentOptions)
    const meta = parseCcteamMeta(body)
    if (meta !== undefined) this.credentials?.set(sessionId, meta)
    let handle: DshAgentHandle
    try {
      const preset = await this.composeCreatePreset(meta?.agentPreset, sessionId)
      const request: Parameters<DshAgents['create']>[0] = {
        sessionId,
        meta: { cwd, ...(preset.id === undefined ? {} : { agentPreset: preset.id }) },
      }
      handle = await this.ctx.agents.create({ ...request, ...agentOptions === undefined ? {} : { agentOptions }, ...preset.setup === undefined ? {} : { setup: preset.setup } })
    } catch (error) {
      this.credentials?.delete(sessionId)
      throw errorToRpc(error)
    }
    this.sessions.set(sessionId, { agent: handle.agent })
    if ((meta?.approvalMode ?? 'skip') !== 'hitl') {
      this.applyHirePermissionPreset(sessionId, handle.agent)
    }
    try {
      await this.workspaces.mount(cwd, sessionId)
    } catch (error) {
      this.ctx.logger?.warn(`ccteam dsh transport could not mount ${cwd}: ${errorMessage(error)}`)
    }
    return { sessionId, ...modelInfoFromAgentOptions(agentOptions) }
  }

  /** Full access for a `skip`-posture hire; best-effort, never fails the create. */
  private applyHirePermissionPreset(sessionId: string, agent: DshAgent): void {
    const svc = typeof this.ctx.get === 'function' ? this.ctx.get('permissionPresets') : undefined
    const presets = svc as DshPermissionPresets | undefined
    if (typeof presets?.set !== 'function') {
      this.ctx.logger?.warn(
        `ccteam dsh transport: no permissionPresets service, session ${sessionId} keeps the runtime default`,
      )
      return
    }
    try {
      presets.set(agent.session as Session, HIRE_PERMISSION_PRESET)
    } catch (error) {
      this.ctx.logger?.warn(
        `ccteam dsh transport could not set ${HIRE_PERMISSION_PRESET} on ${sessionId}: ${errorMessage(error)}`,
      )
    }
  }

  private async loadSession(params: unknown): Promise<{ sessionId: string }> {
    const body = requireRecord(params, 'session/load params')
    const sessionId = requireString(body, 'sessionId') as SessionId
    const agentOptions = this.resolveAgentOptions(body.agentOptions)
    const meta = parseCcteamMeta(body)
    if (meta !== undefined) this.credentials?.set(sessionId, meta)
    let agent: DshAgent
    try {
      // Reuse-live-first: `agents.resume` rejects while the session is already live.
      const live = this.ctx.agents.get?.(sessionId)
      if (live !== undefined) {
        agent = live
      } else {
        // The stored preset is authoritative on resume (the human may have
        // switched it in the DSH UI); `_meta.ccteam.agentPreset` is a
        // creation-only field and deliberately ignored here.
        const setup = this.composeResumeSetup()
        const handle = await this.ctx.agents.resume({
          resumeSessionId: sessionId,
          ...agentOptions === undefined ? {} : { agentOptions },
          ...setup === undefined ? {} : { setup },
        })
        agent = handle.agent
      }
    } catch (error) {
      throw errorToRpc(error)
    }
    this.sessions.set(sessionId, { agent })
    return { sessionId, ...modelInfoFromAgentOptions(agentOptions) }
  }

  private async prompt(params: unknown): Promise<PromptResult> {
    const body = requireRecord(params, 'session/prompt params')
    const sessionId = requireString(body, 'sessionId') as SessionId
    const record = this.sessions.get(sessionId)
    if (record === undefined) throw new RpcError(`unknown session: ${sessionId}`, -32602)
    if (record.inflight !== undefined) throw new RpcError('a prompt is already in flight for this session', -32602)
    const text = acpPromptToText(body.prompt)
    if (text.trim() === '') throw new RpcError('empty prompt', -32602)
    const message = createUserTextMessage(text)

    return new Promise<PromptResult>((resolve, reject) => {
      record.inflight = {
        resolve,
        reject,
        usage: {},
        messageId: message.id,
        bindNextTurn: false,
        toolCalls: new Set(),
      }
      try {
        if (typeof record.agent.followup !== 'function') {
          throw new Error('agent cannot accept prompts')
        }
        record.agent.followup(message)
      } catch (error) {
        record.inflight = undefined
        reject(errorToRpc(error, 'prompt was not queued'))
      }
    })
  }

  private onSessionEvent(session: Session, event: SessionEvent): void {
    const sessionId = sessionIdFromSession(session)
    if (sessionId === undefined) return
    const record = this.sessions.get(sessionId)
    if (record === undefined) return
    switch (event.type) {
      case 'turn/start': {
        const data = event.data
        const turn = data.turn
        record.openTurn = turn
        const inflight = record.inflight
        if (inflight !== undefined && inflight.bindNextTurn && turn !== undefined) {
          inflight.bindNextTurn = false
          inflight.ownedTurn = turn
        }
        break
      }
      case 'user/message': {
        const data = event.data
        const inflight = record.inflight
        if (inflight === undefined || inflight.ownedTurn !== undefined) break
        if (data.id !== inflight.messageId) break
        if (record.openTurn === undefined) {
          inflight.bindNextTurn = true
          break
        }
        inflight.ownedTurn = record.openTurn
        break
      }
      case 'assistant/message': {
        const data = event.data
        if (!this.ownsTurn(record, data.turn)) break
        this.onAssistantMessage(sessionId, data, record)
        break
      }
      case 'tool/call': {
        const data = event.data
        if (!this.ownsTurn(record, data.turn)) break
        const callId = data.callId
        if (callId !== undefined) record.inflight?.toolCalls.add(callId)
        this.notify({
          sessionId,
          update: {
            sessionUpdate: 'tool_call',
            toolCallId: callId ?? 'tool',
            name: data.name ?? 'tool',
            title: data.name ?? 'tool',
            rawInput: parseMaybeJson(data.arguments),
            status: 'pending',
          },
        })
        break
      }
      case 'tool/result': {
        const data = event.data
        const callId = data.message.source.callId
        if (!this.ownsToolResult(record, data.turn, callId)) break
        const failed = data.message.isError === true
        this.notify({
          sessionId,
          update: {
            sessionUpdate: 'tool_call_update',
            toolCallId: callId ?? 'tool',
            status: failed ? 'failed' : 'completed',
            content: toolResultText(data),
            isError: failed,
          },
        })
        break
      }
      case 'turn/end': {
        const data = event.data
        const turn = data.turn
        if (turn === undefined || record.openTurn === turn) record.openTurn = undefined
        const inflight = record.inflight
        if (inflight === undefined || !this.ownsTurn(record, turn)) break
        record.inflight = undefined
        this.notify({
          sessionId,
          update: { sessionUpdate: 'turn_completed' },
        })
        const reason = data.reason
        if (isErrorReason(reason)) {
          const failure = asRecord(reason).error
          inflight.reject(new RpcError(`turn failed: ${failureMessage(failure)}`, -32603, failure))
        } else {
          inflight.resolve(promptResultFromReason(reason, inflight.usage))
        }
        break
      }
    }
  }

  /** True while `turn` is the turn that claimed this transport's queued message. */
  private ownsTurn(record: SessionRecord, turn: number | undefined): boolean {
    const owned = record.inflight?.ownedTurn
    return owned !== undefined && turn !== undefined && owned === turn
  }

  /** True while the owned turn is also the turn currently running on the agent. */
  private ownsActiveTurn(record: SessionRecord, inflight?: Inflight): boolean {
    const owned = (inflight ?? record.inflight)?.ownedTurn
    return owned !== undefined && record.openTurn === owned
  }

  private ownsToolResult(record: SessionRecord, turn: number | undefined, callId: string | undefined): boolean {
    if (this.ownsTurn(record, turn)) return true
    if (turn !== undefined) return false
    return callId !== undefined && record.inflight?.toolCalls.has(callId) === true
  }

  private onAssistantMessage(sessionId: string, data: SessionEventMap['assistant/message'], record: SessionRecord): void {
    for (const block of data.message.content) {
      if (block.type === 'text' && block.text !== '') {
        this.notify({ sessionId, update: {
          sessionUpdate: 'agent_message_chunk', content: { type: 'text', text: block.text },
        } })
      } else if (block.type === 'reasoning' && block.text !== '' && !record.thoughtStreamed) {
        this.notify({ sessionId, update: {
          sessionUpdate: 'agent_thought_chunk', content: { type: 'text', text: block.text },
        } })
      }
    }
    record.thoughtStreamed = false
    accumulateUsage(record.inflight?.usage, asRecord(data.usage))
  }

  /** Follow the public live-stream frames; settled messages own token accounting. */
  private onAssistantStream(agent: Agent, frame: AssistantStreamFrame): void {
    const record = this.sessions.get(agent.id)
    if (record === undefined || record.agent !== agent) return
    if (frame.type === 'start') {
      record.stream = { attemptId: frame.attemptId, revision: frame.revision, turn: frame.turn }
      return
    }
    const stream = record.stream
    if (stream === undefined || stream.attemptId !== frame.attemptId || stream.revision !== frame.revision
      || !this.ownsTurn(record, stream.turn)) return
    if (frame.type === 'end') { record.stream = undefined; return }
    if (frame.chunk.type === 'reasoning-delta' && frame.chunk.text !== '') {
      record.thoughtStreamed = true
      this.notify({ sessionId: agent.id, update: {
        sessionUpdate: 'agent_thought_chunk', content: { type: 'text', text: frame.chunk.text },
      } })
    }
  }

  private onAgentError(payload: unknown): void {
    const body = asRecord(payload)
    const agent = body.agent as DshAgent | undefined
    const sessionId = sessionIdOfAgent(agent)
    if (sessionId === undefined) return
    const record = this.sessions.get(sessionId)
    if (record === undefined || record.agent !== agent) return
    const inflight = record.inflight
    if (inflight === undefined) return
    const turn = numberField(body, 'turn')
    if (inflight.ownedTurn !== undefined && turn !== undefined && inflight.ownedTurn !== turn) return
    record.inflight = undefined
    record.openTurn = undefined
    inflight.reject(errorToRpc(body.error, 'turn failed'))
  }

  private onApprovalRequest(request: unknown, next: () => unknown): unknown {
    const body = asRecord(request)
    const agent = body.agent as DshAgent | undefined
    const sessionId = sessionIdOfAgent(agent)
    if (sessionId === undefined) return next()
    const record = this.sessions.get(sessionId)
    // A human-initiated turn on a hired session keeps its own approval route.
    if (record === undefined || record.agent !== agent || !this.ownsActiveTurn(record)) {
      return next()
    }
    if ((this.credentials?.get(sessionId)?.approvalMode ?? 'skip') !== 'hitl') {
      return 'allowed-once'
    }
    return this.requestPermission(sessionId, stringField(body, 'callId') ?? 'tool').then(result => {
      const outcome = asRecord(asRecord(result).outcome)
      const optionId = stringField(outcome, 'optionId') ?? stringField(asRecord(result), 'optionId')
      if (stringField(outcome, 'outcome') === 'cancelled') return 'cancelled'
      return optionId === 'allow-once' ? 'allowed-once' : 'rejected'
    })
  }

  private requestPermission(sessionId: string, toolCallId: string): Promise<unknown> {
    const id = randomUUID()
    const promise = new Promise<unknown>((resolve, reject) => {
      this.pendingClientRequests.set(id, { resolve, reject })
    })
    this.write({
      jsonrpc: '2.0',
      id,
      method: 'session/request_permission',
      params: {
        sessionId,
        toolCall: { toolCallId },
        options: [
          { optionId: 'allow-once', name: 'Allow once', kind: 'allow_once' },
          { optionId: 'reject-once', name: 'Reject', kind: 'reject_once' },
        ],
      },
    })
    return promise
  }

  private notify(params: unknown): void {
    this.write({
      jsonrpc: '2.0',
      method: 'session/update',
      params,
    })
  }

  private write(message: unknown): void {
    if (this.closed) return
    this.output.write(`${JSON.stringify(message)}\n`)
  }

  private writeError(id: JsonRpcId, code: number, message: string, data?: unknown): void {
    this.write({
      jsonrpc: '2.0',
      id,
      error: {
        code,
        message,
        ...(data === undefined ? {} : { data }),
      },
    })
  }

  private resolveAgentOptions(requested: unknown): AgentOptions | undefined {
    const selection = this.ctx.agentDefaultModel?.currentSelection()
    const selected = isModelSelection(selection) ? selection : undefined
    if (requested === undefined) return selected
    if (!isRecord(requested)) throw new RpcError('agentOptions must be an object', -32602)

    const merged: Record<string, unknown> = { ...requested }
    if (stringField(merged, 'provider') === undefined && selected?.provider !== undefined) {
      merged.provider = selected.provider
    }
    if (stringField(merged, 'model') === undefined && selected?.model !== undefined) {
      merged.model = selected.model
    }
    return Object.keys(merged).length === 0 ? undefined : merged as AgentOptions
  }
}

function isModelSelection(value: unknown): value is { provider?: string; model?: string } {
  const body = asRecord(value)
  return stringField(body, 'provider') !== undefined || stringField(body, 'model') !== undefined
}

function modelInfoFromAgentOptions(agentOptions: unknown): Record<string, unknown> {
  const body = asRecord(agentOptions)
  const provider = stringField(body, 'provider')
  const model = stringField(body, 'model')
  const modelId = provider !== undefined && model !== undefined ? `${provider}/${model}` : model
  if (modelId === undefined) return {}
  return {
    models: {
      currentModelId: modelId,
      availableModels: [{ modelId, name: modelId }],
    },
  }
}

function acpPromptToText(prompt: unknown): string {
  if (!Array.isArray(prompt)) return ''
  const parts: string[] = []
  for (const block of prompt) {
    const item = asRecord(block)
    if (item.type === 'text' && typeof item.text === 'string') {
      parts.push(item.text)
    } else if (item.type === 'resource_link') {
      parts.push(`\n[resource_link name=${JSON.stringify(item.name)} uri=${JSON.stringify(item.uri)}]\n`)
    } else {
      throw new RpcError('only text and resource_link prompt content is supported', -32602)
    }
  }
  return parts.join('')
}

function promptResultFromReason(reason: unknown, usage: Usage): PromptResult {
  const stopReason = turnEndToStopReason(reason)
  const meta = {
    stopReason,
    ...usage.inputTokens === undefined ? {} : { inputTokens: usage.inputTokens },
    ...usage.outputTokens === undefined ? {} : { outputTokens: usage.outputTokens },
    ...usage.cachedReadTokens === undefined ? {} : { cachedReadTokens: usage.cachedReadTokens },
    ...usage.reasoningTokens === undefined ? {} : { reasoningTokens: usage.reasoningTokens },
  }
  return {
    stopReason,
    _meta: meta,
    usage: {
      ...usage.inputTokens === undefined ? {} : { inputTokens: usage.inputTokens },
      ...usage.outputTokens === undefined ? {} : { outputTokens: usage.outputTokens },
      ...usage.cachedReadTokens === undefined ? {} : { cachedInputTokens: usage.cachedReadTokens },
      ...usage.reasoningTokens === undefined ? {} : { reasoningTokens: usage.reasoningTokens },
    },
  }
}

function turnEndToStopReason(reason: unknown): string {
  const value = asRecord(reason)
  switch (value.kind) {
    case 'completed':
      return 'end_turn'
    case 'max-tokens':
      return 'max_tokens'
    case 'max-turn-requests':
      return 'max_turn_requests'
    case 'blocked':
      return 'refusal'
    case 'interrupted':
      return 'cancelled'
    case 'aborted':
      return 'end_turn'
    case 'error':
      return 'end_turn'
    default:
      return 'end_turn'
  }
}

function accumulateUsage(target: Usage | undefined, source: Record<string, unknown>): void {
  if (target === undefined) return
  const usage = normalizeUsage(source)
  target.inputTokens = add(target.inputTokens, usage.inputTokens)
  target.outputTokens = add(target.outputTokens, usage.outputTokens)
  target.cachedReadTokens = add(target.cachedReadTokens, usage.cachedReadTokens)
  target.reasoningTokens = add(target.reasoningTokens, usage.reasoningTokens)
}

function normalizeUsage(source: Record<string, unknown>): Usage & { contextWindow?: number } {
  return {
    inputTokens: numberField(source, 'inputTokens'),
    outputTokens: numberField(source, 'outputTokens'),
    cachedReadTokens: numberField(source, 'cacheReadTokens') ?? numberField(source, 'cachedReadTokens'),
    reasoningTokens: numberField(source, 'reasoningTokens'),
    contextWindow: numberField(source, 'contextWindow') ?? numberField(source, 'size'),
  }
}

function add(left: number | undefined, right: number | undefined): number | undefined {
  if (right === undefined) return left
  return (left ?? 0) + right
}

function sessionIdFromSession(session: unknown): string | undefined {
  const header = asRecord(asRecord(session).header)
  return stringField(header, 'id') ?? stringField(asRecord(session), 'id')
}

function toolResultText(data: SessionEventMap['tool/result']): ContentBlock {
  return { type: 'text', text: JSON.stringify(data.message.content) }
}

function isErrorReason(reason: unknown): boolean {
  return asRecord(reason).kind === 'error'
}

function failureMessage(error: unknown): string {
  const body = asRecord(error)
  if (typeof body.message === 'string') return body.message
  return jsonString(error)
}

function errorToRpc(error: unknown, prefix?: string): RpcError {
  if (error instanceof RpcError) return error
  const body = asRecord(error)
  const code = typeof body.code === 'string' ? body.code : undefined
  const message = errorMessage(error)
  const full = prefix === undefined ? message : `${prefix}: ${message}`
  return new RpcError(full, -32603, code === undefined ? undefined : { code })
}

function requireRecord(value: unknown, label: string): Record<string, unknown> {
  if (!isRecord(value)) throw new RpcError(`${label} must be an object`, -32602)
  return value
}

function requireString(value: Record<string, unknown>, key: string): string {
  const field = stringField(value, key)
  if (field === undefined) throw new RpcError(`${key} must be a string`, -32602)
  return field
}

function stringField(value: Record<string, unknown>, key: string): string | undefined {
  const field = value[key]
  return typeof field === 'string' ? field : undefined
}

function numberField(value: Record<string, unknown>, key: string): number | undefined {
  const field = value[key]
  return typeof field === 'number' && Number.isFinite(field) ? field : undefined
}

function parseMaybeJson(value: unknown): unknown {
  if (typeof value !== 'string') return value
  try {
    return JSON.parse(value)
  } catch {
    return value
  }
}

function asRecord(value: unknown): Record<string, unknown> {
  return isRecord(value) ? value : {}
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function jsonString(value: unknown): string {
  try {
    return JSON.stringify(value)
  } catch {
    return String(value)
  }
}

function errorMessage(error: unknown): string {
  if (error instanceof Error) return error.message
  if (isRecord(error) && typeof error.message === 'string') return error.message
  return String(error)
}
