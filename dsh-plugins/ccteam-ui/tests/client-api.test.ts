/** BFF client: method→URL mapping, JSON wire shape, events URL, frame parsing. */
import { describe, expect, it } from 'vitest'
import { API_PREFIX } from '../src/shared/contract.js'
import { createApi, eventsUrl } from '../src/client/api.js'
import type { EventSourceLike } from '../src/client/api.js'
import type { PanelEvent } from '../src/shared/contract.js'

interface RecordedRequest {
  url: string
  init: RequestInit | undefined
}

function fakeFetch(payload: unknown, status = 200) {
  const requests: RecordedRequest[] = []
  const fetchFn = (url: string, init?: RequestInit): Promise<Response> => {
    requests.push({ url, init })
    return Promise.resolve({
      ok: status >= 200 && status < 300,
      status,
      json: () => Promise.resolve(payload),
    } as Response)
  }
  return { requests, fetchFn }
}

class FakeEventSource implements EventSourceLike {
  onmessage: ((event: { data?: unknown }) => void) | null = null
  onopen: (() => void) | null = null
  onerror: (() => void) | null = null
  closed = false
  constructor(readonly url: string) {}
  close(): void {
    this.closed = true
  }
}

describe('api.call', () => {
  it('POSTs {prefix}/{method} with a JSON body and returns the parsed response', async () => {
    const { requests, fetchFn } = fakeFetch({ connected: true })
    const api = createApi({ fetchFn })
    const result = await api.call('status', {})
    expect(result).toEqual({ connected: true })
    expect(requests).toHaveLength(1)
    const request = requests[0]!
    expect(request.url).toBe(`${API_PREFIX}/status`)
    expect(request.init?.method).toBe('POST')
    expect(request.init?.headers).toEqual({ 'content-type': 'application/json' })
    expect(JSON.parse(String(request.init?.body))).toEqual({})
  })

  it('maps every contract method to its own URL', async () => {
    const { requests, fetchFn } = fakeFetch({})
    const api = createApi({ fetchFn })
    await api.call('team.graph', {})
    await api.call('session.history', { sid: 's7' })
    await api.call('session.send', { sid: 's7', text: 'hi' })
    await api.call('session.spawn', { vendor: 'claude' })
    expect(requests.map(r => r.url)).toEqual([
      `${API_PREFIX}/team.graph`,
      `${API_PREFIX}/session.history`,
      `${API_PREFIX}/session.send`,
      `${API_PREFIX}/session.spawn`,
    ])
    expect(JSON.parse(String(requests[1]!.init?.body))).toEqual({ sid: 's7' })
  })

  it('rejects on non-2xx with the method and status in the message', async () => {
    const { fetchFn } = fakeFetch({}, 502)
    const api = createApi({ fetchFn })
    await expect(api.call('status', {})).rejects.toThrow('status: HTTP 502')
  })
})

describe('api.events', () => {
  it('subscribes without sid to the team stream and with sid to the session stream (encoded)', () => {
    const sources: FakeEventSource[] = []
    const api = createApi({
      createEventSource: (url) => {
        const source = new FakeEventSource(url)
        sources.push(source)
        return source
      },
    })
    api.events({ onEvent: () => {} })
    api.events({ onEvent: () => {} }, 's12')
    api.events({ onEvent: () => {} }, 's 9')
    expect(sources.map(s => s.url)).toEqual([
      `${API_PREFIX}/events`,
      `${API_PREFIX}/events?sid=s12`,
      `${API_PREFIX}/events?sid=s%209`,
    ])
  })

  it('parses JSON frames into onEvent, skips malformed frames, and closes the source', () => {
    const sources: FakeEventSource[] = []
    const events: PanelEvent[] = []
    const api = createApi({
      createEventSource: (url) => {
        const source = new FakeEventSource(url)
        sources.push(source)
        return source
      },
    })
    const handle = api.events({ onEvent: e => events.push(e) })
    const source = sources[0]!
    source.onmessage?.({ data: JSON.stringify({ kind: 'turn_done', sid: 's3' }) })
    source.onmessage?.({ data: 'not json{{' })
    source.onmessage?.({ data: JSON.stringify({ nope: 1 }) })
    source.onmessage?.({ data: JSON.stringify({ kind: 'graph' }) })
    expect(events).toEqual([
      { kind: 'turn_done', sid: 's3' },
      { kind: 'graph' },
    ])
    expect(source.closed).toBe(false)
    handle.close()
    expect(source.closed).toBe(true)
  })

  it('forwards open/error transitions', () => {
    const sources: FakeEventSource[] = []
    const log: string[] = []
    const api = createApi({
      createEventSource: (url) => {
        const source = new FakeEventSource(url)
        sources.push(source)
        return source
      },
    })
    api.events({
      onEvent: () => {},
      onOpen: () => log.push('open'),
      onError: () => log.push('error'),
    })
    sources[0]!.onopen?.()
    sources[0]!.onerror?.()
    expect(log).toEqual(['open', 'error'])
  })
})

describe('eventsUrl', () => {
  it('is a pure function of prefix and sid', () => {
    expect(eventsUrl('/x', undefined)).toBe('/x/events')
    expect(eventsUrl('/x', 's1')).toBe('/x/events?sid=s1')
  })
})
