/**
 * BFF client: the panel's only network face. Speaks the shared contract to
 * the host half (`POST {API_PREFIX}/{method}` JSON + `GET {API_PREFIX}/events`
 * SSE) — the browser never sees a ccteam URL or credential. Both transports
 * are injectable so tests run without fetch/EventSource.
 */
import { API_PREFIX } from '../shared/contract.js'
import type {
  ApiMethod,
  HistoryRequest,
  HistoryResponse,
  PanelEvent,
  SendReceipt,
  SendRequest,
  SpawnRequest,
  SpawnResponse,
  StatusResponse,
  TeamGraph,
} from '../shared/contract.js'

/** Request/response pair per contract method. */
export interface MethodMap {
  'status': { req: Record<string, never>; res: StatusResponse }
  'team.graph': { req: Record<string, never>; res: TeamGraph }
  'session.history': { req: HistoryRequest; res: HistoryResponse }
  'session.send': { req: SendRequest; res: SendReceipt }
  'session.spawn': { req: SpawnRequest; res: SpawnResponse }
}

// Compile-time proof that MethodMap covers ApiMethod exactly.
type MethodsCovered = [ApiMethod] extends [keyof MethodMap]
  ? ([keyof MethodMap] extends [ApiMethod] ? true : never)
  : never
const METHODS_COVERED: MethodsCovered = true
void METHODS_COVERED

/**
 * The slice of EventSource this module assigns to. The default factory casts
 * the DOM object down to it (the DOM lib's `this`-typed handler properties
 * are not structurally assignable in the reading direction; we only write).
 */
export interface EventSourceLike {
  onmessage: ((event: { data?: unknown }) => void) | null
  onopen: (() => void) | null
  onerror: (() => void) | null
  close(): void
}

/** Live subscription handle returned by {@link ApiClient.events}. */
export interface EventStreamHandle {
  close(): void
}

/** Consumer callbacks for one events subscription. */
export interface EventStreamCallbacks {
  /** One parsed SSE frame. Unknown `kind`s must be ignored by the consumer. */
  onEvent(event: PanelEvent): void
  /** Transport (re)connected. */
  onOpen?(): void
  /** Transport error (EventSource retries on its own). */
  onError?(): void
}

/** Injectable transports (tests) and prefix override. */
export interface ApiDeps {
  fetchFn?: (input: string, init?: RequestInit) => Promise<Response>
  createEventSource?: (url: string) => EventSourceLike
  prefix?: string
}

/** The panel-side BFF client. */
export interface ApiClient {
  /** POST one contract method; rejects on transport or non-2xx status. */
  call<M extends ApiMethod>(method: M, body: MethodMap[M]['req']): Promise<MethodMap[M]['res']>
  /** Subscribe to the event stream (whole team, or one sid when given). */
  events(callbacks: EventStreamCallbacks, sid?: string): EventStreamHandle
}

/**
 * Build the events URL for an optional sid filter.
 * @param prefix - API prefix (contract API_PREFIX in production).
 * @param sid - session filter; omitted subscribes to the whole team stream.
 * @returns the subscription URL.
 */
export function eventsUrl(prefix: string, sid?: string): string {
  return sid === undefined ? `${prefix}/events` : `${prefix}/events?sid=${encodeURIComponent(sid)}`
}

/**
 * Create the BFF client.
 * @param deps - injectable transports; production callers pass nothing.
 * @returns the client.
 */
export function createApi(deps: ApiDeps = {}): ApiClient {
  const prefix = deps.prefix ?? API_PREFIX
  const fetchFn = deps.fetchFn ?? ((input: string, init?: RequestInit) => fetch(input, init))
  const createEventSource = deps.createEventSource
    ?? ((url: string): EventSourceLike =>
      typeof EventSource === 'undefined'
        // Non-DOM run (node boot of the client tree): a dead stream — the
        // panel still works over fetch, it just gets no live frames.
        ? { onmessage: null, onopen: null, onerror: null, close: () => {} }
        : new EventSource(url) as unknown as EventSourceLike)

  return {
    async call(method, body) {
      const response = await fetchFn(`${prefix}/${method}`, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify(body),
      })
      if (!response.ok) {
        throw new Error(`ccteam bff ${method}: HTTP ${response.status}`)
      }
      return (await response.json()) as MethodMap[typeof method]['res']
    },

    events(callbacks, sid) {
      const source = createEventSource(eventsUrl(prefix, sid))
      source.onmessage = (event) => {
        if (typeof event.data !== 'string') return
        let parsed: unknown
        try {
          parsed = JSON.parse(event.data)
        } catch {
          // Malformed frame: skipped so one bad frame cannot kill the
          // subscription; nothing else can throw in this handler.
          return
        }
        if (parsed !== null && typeof parsed === 'object' && typeof (parsed as PanelEvent).kind === 'string') {
          callbacks.onEvent(parsed as PanelEvent)
        }
      }
      source.onopen = () => {
        callbacks.onOpen?.()
      }
      source.onerror = () => {
        callbacks.onError?.()
      }
      return {
        close() {
          source.close()
        },
      }
    },
  }
}
