/**
 * SSE plumbing for the host half: a line parser over a `fetch` response body,
 * and a refcounted fan-out hub that keeps at most ONE upstream connection per
 * upstream stream no matter how many browser tabs are watching it.
 *
 * Nothing here knows a ccteam field name — the hub forwards whatever its
 * `translate` callback produces (see bff.ts).
 */

/** One parsed upstream SSE frame. */
export interface SseFrame {
  /** The `event:` name, or undefined for a bare `data:` frame. */
  event?: string
  /** Concatenated `data:` lines. */
  data: string
}

/**
 * Parse an SSE byte stream into frames. Handles CRLF, multi-line `data:`,
 * comment lines (`:` heartbeats) and the optional space after the colon.
 */
export async function* parseSse(
  body: ReadableStream<Uint8Array>,
  signal?: AbortSignal,
): AsyncGenerator<SseFrame> {
  const reader = body.getReader()
  const decoder = new TextDecoder()
  let buffer = ''
  const onAbort = (): void => {
    void reader.cancel().catch(() => {
      // Cancelling an already-errored stream is not actionable.
    })
  }
  signal?.addEventListener('abort', onAbort, { once: true })
  try {
    for (;;) {
      const { done, value } = await reader.read()
      if (done) break
      buffer += decoder.decode(value, { stream: true })
      buffer = buffer.replace(/\r\n/g, '\n')
      let cut = buffer.indexOf('\n\n')
      while (cut !== -1) {
        const block = buffer.slice(0, cut)
        buffer = buffer.slice(cut + 2)
        const frame = parseBlock(block)
        if (frame !== undefined) yield frame
        cut = buffer.indexOf('\n\n')
      }
    }
    const tail = parseBlock(buffer)
    if (tail !== undefined) yield tail
  } finally {
    signal?.removeEventListener('abort', onAbort)
    reader.releaseLock()
  }
}

function parseBlock(block: string): SseFrame | undefined {
  let event: string | undefined
  const data: string[] = []
  for (const line of block.split('\n')) {
    if (line === '' || line.startsWith(':')) continue
    const colon = line.indexOf(':')
    const field = colon === -1 ? line : line.slice(0, colon)
    let value = colon === -1 ? '' : line.slice(colon + 1)
    if (value.startsWith(' ')) value = value.slice(1)
    if (field === 'event') event = value
    else if (field === 'data') data.push(value)
  }
  if (data.length === 0) return undefined
  return { event, data: data.join('\n') }
}

export interface UpstreamSource {
  /** Open the upstream stream; rejects or returns a non-OK response on failure. */
  open(signal: AbortSignal): Promise<Response>
  /** Map one upstream frame to zero or more downstream payloads. */
  translate(frame: SseFrame): unknown[]
}

export interface HubOptions {
  /** Capped exponential backoff between upstream reconnect attempts. */
  retryBaseMs?: number
  retryMaxMs?: number
  logger?: { warn(message: string): void }
  /** Injected for tests; defaults to setTimeout. */
  sleep?: (ms: number, signal: AbortSignal) => Promise<void>
}

type Subscriber = (payload: unknown) => void

interface Channel {
  subscribers: Set<Subscriber>
  abort: AbortController
  /** Resolves when the pump loop has fully exited (test synchronisation). */
  done: Promise<void>
}

/**
 * Refcounted upstream multiplexer. `subscribe` opens the upstream on the first
 * subscriber and returns an unsubscribe function; the last unsubscribe aborts
 * the upstream. An upstream drop reconnects with capped backoff for as long as
 * at least one subscriber remains.
 */
export class SseHub {
  private readonly channels = new Map<string, Channel>()
  private readonly retryBaseMs: number
  private readonly retryMaxMs: number
  private readonly logger: { warn(message: string): void } | undefined
  private readonly sleep: (ms: number, signal: AbortSignal) => Promise<void>
  private closed = false

  constructor(options: HubOptions = {}) {
    this.retryBaseMs = options.retryBaseMs ?? 500
    this.retryMaxMs = options.retryMaxMs ?? 15_000
    this.logger = options.logger
    this.sleep = options.sleep ?? defaultSleep
  }

  /** Number of live upstream connections — the fan-out invariant under test. */
  get upstreamCount(): number {
    return this.channels.size
  }

  subscribe(key: string, source: UpstreamSource, onPayload: Subscriber): () => void {
    if (this.closed) return () => {}
    let channel = this.channels.get(key)
    if (channel === undefined) {
      const abort = new AbortController()
      const created: Channel = { subscribers: new Set(), abort, done: Promise.resolve() }
      created.done = this.pump(key, source, created)
      this.channels.set(key, created)
      channel = created
    }
    channel.subscribers.add(onPayload)
    let released = false
    return () => {
      if (released) return
      released = true
      const live = this.channels.get(key)
      if (live === undefined) return
      live.subscribers.delete(onPayload)
      if (live.subscribers.size === 0) {
        this.channels.delete(key)
        live.abort.abort()
      }
    }
  }

  /** Tear down every upstream (plugin disposal). */
  close(): void {
    this.closed = true
    for (const [key, channel] of this.channels) {
      this.channels.delete(key)
      channel.subscribers.clear()
      channel.abort.abort()
    }
  }

  /** Await pump exit for a key — test-only synchronisation. */
  async settled(key: string, pending?: Promise<void>): Promise<void> {
    await (pending ?? this.channels.get(key)?.done ?? Promise.resolve())
  }

  private async pump(key: string, source: UpstreamSource, channel: Channel): Promise<void> {
    let attempt = 0
    while (!channel.abort.signal.aborted) {
      try {
        const response = await source.open(channel.abort.signal)
        if (!response.ok || response.body === null) {
          throw new Error(`upstream ${key} responded ${response.status}`)
        }
        attempt = 0
        for await (const frame of parseSse(response.body, channel.abort.signal)) {
          if (channel.abort.signal.aborted) break
          for (const payload of source.translate(frame)) {
            for (const subscriber of [...channel.subscribers]) subscriber(payload)
          }
        }
      } catch (error) {
        if (channel.abort.signal.aborted) break
        this.logger?.warn(`ccteam-ui: upstream ${key} failed: ${describe(error)}`)
      }
      if (channel.abort.signal.aborted) break
      const delay = Math.min(this.retryMaxMs, this.retryBaseMs * 2 ** attempt)
      attempt += 1
      try {
        await this.sleep(delay, channel.abort.signal)
      } catch {
        break
      }
    }
  }
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}

function defaultSleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      signal.removeEventListener('abort', onAbort)
      resolve()
    }, ms)
    const onAbort = (): void => {
      clearTimeout(timer)
      reject(new Error('aborted'))
    }
    signal.addEventListener('abort', onAbort, { once: true })
  })
}
