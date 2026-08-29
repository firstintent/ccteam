import { randomUUID } from 'node:crypto'
import { connect, type Socket } from 'node:net'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import { vi } from 'vitest'

export interface FakeAgent {
  id: string
  session: { id: string }
  followup: ReturnType<typeof vi.fn>
  cancel: ReturnType<typeof vi.fn>
  whenIdle: ReturnType<typeof vi.fn>
  inbox: { remove: ReturnType<typeof vi.fn> }
  [key: string]: unknown
}

export interface FakeTool {
  name: string
  execute(args: unknown, exec: unknown): Promise<{ content: { type: string; text?: string }[]; isError: boolean }>
}

type Listener = (...args: never[]) => unknown

export function makeFakeAgent(id: string): FakeAgent {
  return {
    id,
    session: { id },
    followup: vi.fn(),
    cancel: vi.fn(),
    whenIdle: vi.fn(async () => undefined),
    inbox: { remove: vi.fn(() => true) },
  }
}

export interface FakeWorkspace {
  path: string
  attachSession: ReturnType<typeof vi.fn>
}

/**
 * A stand-in for the Cordis plugin context: multi-listener events, an agent
 * registry with the live/resume ladder, an optional workspace registry, and the
 * tool + settings surfaces `apply` needs.
 */
export function makeFakeCtx(options?: {
  workspaces?: boolean
  workspaceCreateFails?: boolean
  attachFails?: boolean
  settings?: Record<string, unknown>
  resumeUnavailable?: boolean
  /** `false` hides the `agentPresets` service (ACP-bundle-style runtime). */
  presets?: boolean
  /** When set, `sessionPersistence.inspect` serves this snapshot. */
  persistence?: { meta?: Record<string, unknown>; events?: unknown[] }
  /** `false` hides the `permissionPresets` service. */
  permissions?: boolean
}) {
  const listeners = new Map<string, Set<Listener>>()
  const agents = new Map<string, FakeAgent>()
  const workspaces = new Map<string, FakeWorkspace>()
  const tools: FakeTool[] = []
  const cleanups: unknown[] = []
  const warnings: string[] = []

  const create = vi.fn(async (request: { sessionId: string; meta?: { cwd?: string }; agentOptions?: unknown }) => {
    const agent = makeFakeAgent(request.sessionId)
    agents.set(request.sessionId, agent)
    return { agent, dispose: vi.fn() }
  })
  const resume = vi.fn(async (request: { resumeSessionId: string }) => {
    if (options?.resumeUnavailable === true) {
      throw new Error(`cannot prepare session "${request.resumeSessionId}" while it is live`)
    }
    const agent = makeFakeAgent(request.resumeSessionId)
    agents.set(request.resumeSessionId, agent)
    return { agent, dispose: vi.fn() }
  })
  const workspaceCreate = vi.fn(async (path: string) => {
    if (options?.workspaceCreateFails === true) throw new Error(`no such directory: ${path}`)
    const existing = workspaces.get(path)
    if (existing !== undefined) return existing
    const workspace: FakeWorkspace = {
      path,
      attachSession: vi.fn(async () => {
        if (options?.attachFails === true) throw new Error('attach refused')
      }),
    }
    workspaces.set(path, workspace)
    return workspace
  })

  const agentPresets = {
    // Mirrors the vendor roster: resolve(undefined) falls back to the default id.
    resolve: vi.fn(async (id?: string) => ({ id: id ?? 'standard' })),
    mount: vi.fn(async (_agentCtx: unknown, _id?: string) => undefined),
  }
  const persistenceInspect = vi.fn(async (_sessionId: string) => options?.persistence ?? {})
  const permissionPresets = { set: vi.fn((_session: unknown, _name: string) => undefined) }

  const ctx = {
    tools: {
      register: vi.fn((tool: FakeTool) => {
        tools.push(tool)
        return vi.fn()
      }),
    },
    settings: {
      register: vi.fn(() => ({
        get: () => ({
          daemonUrl: 'http://daemon.test',
          enrollment: '',
          restToken: '',
          defaultProject: '',
          connectionStatus: '',
          ...options?.settings,
        }),
      })),
    },
    agents: {
      create,
      resume,
      get: vi.fn((id: string) => agents.get(id)),
    },
    // Cordis-faithful: services outside the plugin's `inject` list are reached
    // ONLY through `ctx.get`; direct property access throws (see below).
    get: vi.fn((name: string) => {
      if (name === 'workspaceRegistry' && options?.workspaces !== false) {
        return { create: workspaceCreate }
      }
      if (name === 'agentPresets' && options?.presets !== false) {
        return agentPresets
      }
      if (name === 'sessionPersistence' && options?.persistence !== undefined) {
        return { inspect: persistenceInspect }
      }
      if (name === 'permissionPresets' && options?.permissions !== false) {
        return permissionPresets
      }
      return undefined
    }),
    agentDefaultModel: {
      currentSelection: vi.fn(() => ({ provider: 'aliyun', model: 'deepseek-v4-pro' })),
    },
    on: vi.fn((event: string, handler: Listener) => {
      const bucket = listeners.get(event) ?? new Set<Listener>()
      bucket.add(handler)
      listeners.set(event, bucket)
      return vi.fn(() => {
        bucket.delete(handler)
      })
    }),
    effect: vi.fn((setup: () => unknown) => {
      cleanups.push(setup())
      return vi.fn()
    }),
    logger: { warn: vi.fn((message: string) => { warnings.push(message) }) },
  }
  // Mirror Cordis exactly: reading a service property outside `inject` throws.
  // This is what shipped the v0.10.3 real-machine ungrouped-session bug — the
  // fakes exposed `ctx.workspaceRegistry` as a plain property, so the tests
  // stayed green while every real runtime threw here.
  Object.defineProperty(ctx, 'workspaceRegistry', {
    get() {
      throw new Error('cannot get property "workspaceRegistry" without inject')
    },
  })

  /**
   * Cordis-faithful `ctx.inject`: the body runs only once EVERY named service
   * exists (vendor/cordis `Fiber._refresh` — a missing dependency parks the
   * fiber instead of running it). This fake runtime has no `webServer`, so a
   * face injecting it never activates here, which is exactly the behaviour the
   * merged plugin relies on to stay usable on a non-web profile.
   *
   * Defined after the literal so the callback can close over `ctx` without
   * making its own type circular.
   */
  const inject = vi.fn((deps: readonly string[], body: (injected: unknown) => void) => {
    for (const dep of deps) {
      let value: unknown
      try {
        value = (ctx as Record<string, unknown>)[dep]
      } catch {
        // Cordis throws on a service read outside `inject`; treat it as absent.
        return
      }
      if (value === undefined || value === null) return
    }
    body(ctx)
  })
  Object.defineProperty(ctx, 'inject', { value: inject, enumerable: true })

  const emit = (event: string, ...args: unknown[]): void => {
    for (const handler of [...(listeners.get(event) ?? [])]) {
      (handler as (...rest: unknown[]) => unknown)(...args)
    }
  }

  /** Run the `approval/request` waterfall over every registered listener. */
  const requestApproval = async (request: unknown): Promise<unknown> => {
    const handlers = [...(listeners.get('approval/request') ?? [])]
    let index = 0
    const next = async (): Promise<unknown> => {
      if (index >= handlers.length) return 'unavailable'
      const handler = handlers[index++] as (req: unknown, nxt: () => unknown) => unknown
      return await handler(request, next)
    }
    return next()
  }

  const sessionEvent = (sessionId: string, type: string, data: unknown): void => {
    emit('session/event', { id: sessionId }, { type, seq: 1, time: new Date().toISOString(), data })
  }

  return {
    ctx,
    inject,
    tools,
    cleanups,
    warnings,
    agents,
    workspaces,
    create,
    resume,
    workspaceCreate,
    agentPresets,
    persistenceInspect,
    permissionPresets,
    emit,
    sessionEvent,
    requestApproval,
    listenerCount: (event: string) => listeners.get(event)?.size ?? 0,
  }
}

/** Linux caps unix socket paths near 108 bytes: stay directly under tmpdir. */
export function shortSocketPath(): string {
  return join(tmpdir(), `cct-${randomUUID().slice(0, 8)}.sock`)
}

/** Minimal ACP client over one connection: requests, notifications, updates. */
export class AcpClient {
  private readonly socket: Socket
  private buffer = ''
  private nextId = 1
  private readonly pending = new Map<string, { resolve: (value: unknown) => void; reject: (error: Error) => void }>()
  readonly updates: Record<string, unknown>[] = []
  permissionResponder: (params: unknown) => unknown = () => ({ outcome: { outcome: 'selected', optionId: 'allow-once' } })

  private constructor(socket: Socket) {
    this.socket = socket
    socket.setEncoding('utf8')
    socket.on('data', chunk => this.receive(String(chunk)))
  }

  static connect(path: string): Promise<AcpClient> {
    return new Promise((resolve, reject) => {
      const socket = connect(path)
      socket.once('error', reject)
      socket.once('connect', () => {
        socket.off('error', reject)
        socket.on('error', () => undefined)
        resolve(new AcpClient(socket))
      })
    })
  }

  private receive(chunk: string): void {
    this.buffer += chunk
    for (;;) {
      const newline = this.buffer.indexOf('\n')
      if (newline < 0) break
      const line = this.buffer.slice(0, newline).trim()
      this.buffer = this.buffer.slice(newline + 1)
      if (line === '') continue
      const message = JSON.parse(line) as Record<string, unknown>
      if (message.method === 'session/update') {
        this.updates.push(message.params as Record<string, unknown>)
        continue
      }
      if (message.method === 'session/request_permission') {
        const result = this.permissionResponder(message.params)
        this.write({ jsonrpc: '2.0', id: message.id, result })
        continue
      }
      if (message.id === undefined) continue
      const waiter = this.pending.get(String(message.id))
      if (waiter === undefined) continue
      this.pending.delete(String(message.id))
      if (message.error !== undefined) {
        waiter.reject(new Error(JSON.stringify(message.error)))
      } else {
        waiter.resolve(message.result)
      }
    }
  }

  request(method: string, params: unknown): Promise<any> {
    const id = `c${this.nextId++}`
    const promise = new Promise<unknown>((resolve, reject) => {
      this.pending.set(id, { resolve, reject })
    })
    this.write({ jsonrpc: '2.0', id, method, params })
    return promise as Promise<any>
  }

  notify(method: string, params: unknown): void {
    this.write({ jsonrpc: '2.0', method, params })
  }

  private write(message: unknown): void {
    this.socket.write(`${JSON.stringify(message)}\n`)
  }

  close(): void {
    this.socket.destroy()
  }
}

export async function waitFor(predicate: () => boolean, label = 'condition'): Promise<void> {
  for (let i = 0; i < 200; i++) {
    if (predicate()) return
    await new Promise(resolve => setTimeout(resolve, 5))
  }
  throw new Error(`timed out waiting for ${label}`)
}

export async function settle(ticks = 3): Promise<void> {
  for (let i = 0; i < ticks; i++) await new Promise(resolve => setTimeout(resolve, 1))
}
