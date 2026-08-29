/**
 * Host-half test doubles. Zero real network and zero real sockets: the BFF
 * takes its `fetch` by injection, and the DSH `webServer` service is replaced
 * by a fake that reproduces the one behaviour the real one enforces —
 * duplicate (kind, path) throws.
 */
import { EventEmitter } from 'node:events'
import type { IncomingMessage, ServerResponse } from 'node:http'

export interface FakeRoute {
  kind: 'exact' | 'prefix'
  path: string
  handler: (req: IncomingMessage, res: ServerResponse) => void | Promise<void>
}

/**
 * Mirrors `WebServer.register` from
 * references/deepseek-harness/packages/host/webserver/src/index.ts: one map per
 * kind, duplicate key throws, registration returns the disposer.
 */
/**
 * Cordis-faithful `ctx.inject`: the body runs only once EVERY named service
 * exists (vendor/cordis `Fiber._refresh`). A host ctx built here serves
 * `webServer` and `settings` but no agent runtime, so the plugin's tool and
 * transport faces correctly never activate against it.
 *
 * @param ctx - the fake plugin context the body should receive.
 * @returns the `inject` implementation to place on that context.
 */
export function fakeInject(ctx: Record<string, unknown>) {
  return (deps: readonly string[], body: (injected: unknown) => void): void => {
    for (const dep of deps) {
      const value = ctx[dep]
      if (value === undefined || value === null) return
    }
    body(ctx)
  }
}

/**
 * A plugin context carrying exactly the web-half services, with `inject` wired
 * the way Cordis wires it.
 *
 * @param services - the services this runtime offers.
 * @returns the context to hand to `apply`.
 */
export function hostCtx(services: Record<string, unknown>): Record<string, unknown> {
  const ctx: Record<string, unknown> = { ...services }
  ctx.inject = fakeInject(ctx)
  return ctx
}

export class FakeWebServer {
  readonly routes: FakeRoute[] = []
  private readonly taken = new Set<string>()

  register(route: FakeRoute): () => void {
    const key = `${route.kind} ${route.path}`
    if (this.taken.has(key)) {
      throw new Error(`webserver: duplicate ${route.kind} route "${route.path}"`)
    }
    this.taken.add(key)
    this.routes.push(route)
    return () => {
      this.taken.delete(key)
      const at = this.routes.indexOf(route)
      if (at !== -1) this.routes.splice(at, 1)
    }
  }

  /** The single handler the BFF is expected to have registered. */
  handler(): (req: IncomingMessage, res: ServerResponse) => void | Promise<void> {
    if (this.routes.length !== 1) {
      throw new Error(`expected exactly one route, got ${this.routes.length}`)
    }
    return this.routes[0]!.handler
  }
}

export interface CapturedCall {
  url: string
  method: string
  authorization: string | undefined
  headers: Record<string, string>
  body: unknown
}

/** A response the fake fetch should return for a matching URL suffix. */
export interface StubReply {
  status?: number
  json?: unknown
  text?: string
  /** Throw instead of replying (network failure). */
  networkError?: string
}

/**
 * Records every upstream call and replies from a routing table keyed by a
 * substring of the request URL. Unmatched URLs are a loud test failure, never
 * a silent empty success.
 */
export class FakeFetch {
  readonly calls: CapturedCall[] = []
  private readonly replies: Array<{ match: string; reply: StubReply | (() => StubReply) }> = []

  on(match: string, reply: StubReply | (() => StubReply)): this {
    this.replies.push({ match, reply })
    return this
  }

  get fetch(): (input: string, init?: RequestInit) => Promise<Response> {
    return async (input: string, init: RequestInit = {}) => {
      const headers = normalizeHeaders(init.headers)
      let body: unknown
      if (typeof init.body === 'string' && init.body.length > 0) {
        body = JSON.parse(init.body) as unknown
      }
      this.calls.push({
        url: input,
        method: (init.method ?? 'GET').toUpperCase(),
        authorization: headers.authorization,
        headers,
        body,
      })
      const entry = this.replies.find(candidate => input.includes(candidate.match))
      if (entry === undefined) {
        throw new Error(`FakeFetch: no stub for ${input}`)
      }
      const reply = typeof entry.reply === 'function' ? entry.reply() : entry.reply
      if (reply.networkError !== undefined) {
        throw new Error(reply.networkError)
      }
      const payload = reply.text ?? JSON.stringify(reply.json ?? {})
      return new Response(payload, {
        status: reply.status ?? 200,
        headers: { 'content-type': 'application/json' },
      })
    }
  }
}

function normalizeHeaders(input: HeadersInit | undefined): Record<string, string> {
  const out: Record<string, string> = {}
  if (input === undefined) return out
  if (input instanceof Headers) {
    input.forEach((value, key) => {
      out[key.toLowerCase()] = value
    })
    return out
  }
  if (Array.isArray(input)) {
    for (const [key, value] of input) out[key.toLowerCase()] = value
    return out
  }
  for (const [key, value] of Object.entries(input)) out[key.toLowerCase()] = String(value)
  return out
}

/**
 * A controllable upstream SSE body: the test pushes frames, the BFF's stream
 * reader consumes them, and `close`/`fail` exercise teardown and retry.
 */
export class FakeSseUpstream {
  private controller: ReadableStreamDefaultController<Uint8Array> | undefined
  readonly opened: number
  readonly stream: ReadableStream<Uint8Array>
  aborted = false

  constructor(opened = 1) {
    this.opened = opened
    this.stream = new ReadableStream<Uint8Array>({
      start: controller => {
        this.controller = controller
      },
      cancel: () => {
        this.aborted = true
      },
    })
  }

  /** Push one upstream SSE frame (named event optional). */
  push(data: unknown, event?: string): void {
    const head = event === undefined ? '' : `event: ${event}\n`
    this.write(`${head}data: ${JSON.stringify(data)}\n\n`)
  }

  write(raw: string): void {
    this.controller?.enqueue(new TextEncoder().encode(raw))
  }

  close(): void {
    try {
      this.controller?.close()
    } catch {
      // Already closed by a cancel() from the consumer side; nothing to do.
    }
  }

  fail(message = 'upstream dropped'): void {
    this.controller?.error(new Error(message))
  }

  response(): Response {
    return new Response(this.stream, {
      status: 200,
      headers: { 'content-type': 'text/event-stream' },
    })
  }
}

/** Collects everything the BFF writes to a downstream SSE response. */
export class FakeResponse extends EventEmitter {
  statusCode = 200
  headers: Record<string, string> = {}
  chunks: string[] = []
  ended = false
  headersSent = false
  destroyed = false
  /** Node sets this on the real ServerResponse; the SSE loop checks it. */
  writableEnded = false

  writeHead(status: number, headers?: Record<string, string>): this {
    this.statusCode = status
    this.headersSent = true
    if (headers !== undefined) {
      for (const [key, value] of Object.entries(headers)) {
        this.headers[key.toLowerCase()] = value
      }
    }
    return this
  }

  setHeader(key: string, value: string): this {
    this.headers[key.toLowerCase()] = value
    return this
  }

  write(chunk: string): boolean {
    this.chunks.push(chunk)
    return true
  }

  end(chunk?: string): this {
    if (chunk !== undefined) this.chunks.push(chunk)
    this.ended = true
    this.writableEnded = true
    this.emit('finish')
    return this
  }

  /** Simulate the browser going away. */
  disconnect(): void {
    this.destroyed = true
    this.emit('close')
  }

  body(): string {
    return this.chunks.join('')
  }

  json(): unknown {
    return JSON.parse(this.body()) as unknown
  }

  /** Parsed `data:` payloads of every downstream SSE frame. */
  frames(): unknown[] {
    const out: unknown[] = []
    for (const line of this.body().split('\n')) {
      if (line.startsWith('data: ')) out.push(JSON.parse(line.slice(6)) as unknown)
    }
    return out
  }

  asServerResponse(): ServerResponse {
    return this as unknown as ServerResponse
  }
}

/** A minimal IncomingMessage: only method, url and close events are read. */
export class FakeRequest extends EventEmitter {
  constructor(
    readonly method: string,
    readonly url: string,
    private readonly payload?: unknown,
  ) {
    super()
  }

  asIncomingMessage(): IncomingMessage {
    return this as unknown as IncomingMessage
  }

  /** Body delivery: the BFF reads the request as an async iterable of chunks. */
  async *[Symbol.asyncIterator](): AsyncGenerator<Buffer> {
    if (this.payload === undefined) return
    yield Buffer.from(typeof this.payload === 'string' ? this.payload : JSON.stringify(this.payload))
  }
}

/** Drain the microtask queue plus any pending timers-free async work. */
export async function settle(times = 6): Promise<void> {
  for (let i = 0; i < times; i += 1) await Promise.resolve()
  await new Promise(resolve => setImmediate(resolve))
}
