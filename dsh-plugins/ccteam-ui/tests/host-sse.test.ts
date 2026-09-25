import { describe, expect, it } from 'vitest'
import { createBff, translateGlobal, translateSession } from '../src/bff.js'
import { API_PREFIX } from '../src/shared/contract.js'
import { FakeRequest, FakeResponse, FakeSseUpstream, settle } from './host-fakes.js'

const TOKEN = 'ccteam:deadbeefcafe'

/** A fake upstream SSE endpoint that records every open and hands back a stream. */
class SseServer {
  readonly opens: string[] = []
  readonly authorizations: Array<string | undefined> = []
  readonly streams: FakeSseUpstream[] = []

  readonly fetch = async (input: string, init: RequestInit = {}): Promise<Response> => {
    this.opens.push(input)
    const headers = (init.headers ?? {}) as Record<string, string>
    this.authorizations.push(headers.authorization)
    const stream = new FakeSseUpstream(this.streams.length + 1)
    this.streams.push(stream)
    return stream.response()
  }

  /** Streams opened against a URL containing `match`. */
  openedFor(match: string): number {
    return this.opens.filter(url => url.includes(match)).length
  }

  async waitForStreams(count: number): Promise<void> {
    for (let i = 0; i < 200 && this.streams.length < count; i += 1) await settle(2)
    if (this.streams.length < count) {
      throw new Error(`expected ${count} upstream streams, saw ${this.streams.length}`)
    }
    await settle()
  }
}

function harness(server: SseServer, heartbeatMs = 60_000) {
  return createBff({
    daemonUrl: () => 'http://127.0.0.1:7331',
    restToken: () => TOKEN,
    fetchImpl: server.fetch,
    heartbeatMs,
    retryBaseMs: 1,
    retryMaxMs: 2,
  })
}

function connect(
  bff: ReturnType<typeof harness>,
  sid?: string,
): { req: FakeRequest; res: FakeResponse } {
  const url = sid === undefined ? `${API_PREFIX}/events` : `${API_PREFIX}/events?sid=${sid}`
  const req = new FakeRequest('GET', url)
  const res = new FakeResponse()
  void bff.handle(req.asIncomingMessage(), res.asServerResponse())
  return { req, res }
}

describe('SSE fan-out', () => {
  it('answers as an event stream that proxies will not buffer', async () => {
    const server = new SseServer()
    const bff = harness(server)
    const { res } = connect(bff)
    await server.waitForStreams(1)

    expect(res.statusCode).toBe(200)
    expect(res.headers['content-type']).toBe('text/event-stream')
    expect(res.headers['cache-control']).toBe('no-cache, no-transform')
    expect(res.headers['x-accel-buffering']).toBe('no')
    bff.close()
  })

  it('serves two downstream clients from ONE upstream connection', async () => {
    const server = new SseServer()
    const bff = harness(server)

    const first = connect(bff)
    await server.waitForStreams(1)
    const second = connect(bff)
    await settle()

    // Second subscriber joins the existing channel: still one upstream.
    expect(server.openedFor('/api/v1/agents/events')).toBe(1)
    expect(bff.upstreamCount()).toBe(1)

    server.streams[0]!.push({ id: '1', sid: 's1', slug: 'ccteam', kind: 'session_lifecycle', content: '', state: 'stopped' }, 'session_lifecycle')
    await settle()

    const expected = [{ kind: 'graph' }, { kind: 'session', sid: 's1', event: { kind: 'lifecycle', state: 'stopped' } }]
    expect(first.res.frames()).toEqual(expected)
    expect(second.res.frames()).toEqual(expected)
    bff.close()
  })

  it('opens a second upstream for a watched sid and refcounts it independently', async () => {
    const server = new SseServer()
    const bff = harness(server)

    const watcher = connect(bff, 's1')
    await server.waitForStreams(2)

    expect(server.openedFor('/api/v1/agents/events')).toBe(1)
    expect(server.openedFor('/api/v1/sessions/s1/events')).toBe(1)
    expect(bff.upstreamCount()).toBe(2)

    const sessionStream = server.streams.find(stream =>
      server.opens[stream.opened - 1]!.includes('/sessions/s1/events'))!
    sessionStream.push({
      id: 'e9', sid: 's1', slug: 'ccteam', kind: 'answer',
      content: 'done', ts: '2026-08-25T10:00:00Z',
    })
    await settle()

    expect(watcher.res.frames()).toEqual([{
      kind: 'session',
      sid: 's1',
      event: { kind: 'answer', id: 'e9', content: 'done', ts: '2026-08-25T10:00:00Z' },
    }])
    bff.close()
  })

  it('the LAST downstream disconnect tears the upstream down', async () => {
    const server = new SseServer()
    const bff = harness(server)

    const first = connect(bff)
    await server.waitForStreams(1)
    const second = connect(bff)
    await settle()

    first.req.emit('close')
    await settle()
    // One subscriber left: upstream stays up.
    expect(bff.upstreamCount()).toBe(1)
    expect(server.streams[0]!.aborted).toBe(false)

    second.req.emit('close')
    await settle(10)
    expect(bff.upstreamCount()).toBe(0)
    expect(server.streams[0]!.aborted).toBe(true)
    expect(second.res.ended).toBe(true)
    bff.close()
  })

  it('reconnects while a downstream remains, and stops once none do', async () => {
    const server = new SseServer()
    const bff = harness(server)

    const client = connect(bff)
    await server.waitForStreams(1)
    server.streams[0]!.fail('upstream dropped')
    await server.waitForStreams(2)

    expect(server.openedFor('/api/v1/agents/events')).toBe(2)
    server.streams[1]!.push({ id: '2', sid: 's1', kind: 'delegation', content: '' }, 'delegation')
    await settle()
    expect(client.res.frames()).toEqual([{ kind: 'graph' }])

    const opensBefore = server.opens.length
    client.req.emit('close')
    await settle(10)
    server.streams[1]!.fail('dropped again')
    await settle(10)
    // No subscriber remains, so the drop must not trigger another dial.
    expect(server.opens.length).toBe(opensBefore)
    bff.close()
  })

  it('sends heartbeat comments so idle streams survive proxies', async () => {
    const server = new SseServer()
    const bff = harness(server, 5)
    const { res } = connect(bff)
    await server.waitForStreams(1)

    await new Promise(resolve => setTimeout(resolve, 40))
    expect(res.body()).toContain(': ping\n\n')
    bff.close()
  })

  it('never leaks the REST token downstream', async () => {
    const server = new SseServer()
    const bff = harness(server)
    const { res } = connect(bff, 's1')
    await server.waitForStreams(2)

    // The token IS presented upstream...
    expect(server.authorizations.every(value => value === `Bearer ${TOKEN}`)).toBe(true)

    for (const stream of server.streams) {
      stream.push({
        id: '1', sid: 's1', slug: 'ccteam', kind: 'answer',
        content: 'all done', ts: '2026-08-25T10:00:00Z',
      })
    }
    await settle()

    // ...and the BFF adds it to nothing it writes downstream. Grep the whole
    // serialized body rather than individual fields.
    const body = res.body()
    expect(body.length).toBeGreaterThan(0)
    expect(body).not.toContain(TOKEN)
    expect(body).not.toContain('deadbeefcafe')
    expect(body.toLowerCase()).not.toContain('authorization')
    expect(body.toLowerCase()).not.toContain('bearer')
    // Nor does it ever reach the process environment (plugin 1's D19 rule).
    expect(Object.values(process.env).some(value => (value ?? '').includes(TOKEN))).toBe(false)
    bff.close()
  })

  it('close() drops every upstream at once', async () => {
    const server = new SseServer()
    const bff = harness(server)
    connect(bff, 's1')
    await server.waitForStreams(2)
    expect(bff.upstreamCount()).toBe(2)

    bff.close()
    await settle(10)
    expect(bff.upstreamCount()).toBe(0)
    expect(server.streams.every(stream => stream.aborted)).toBe(true)
  })
})

describe('frame translation', () => {
  const frame = (data: unknown, event?: string) => ({
    ...(event === undefined ? {} : { event }),
    data: JSON.stringify(data),
  })

  /** The upstream `TurnStatus` object a structured turn's closing answer carries. */
  const STATUS = {
    model: 'opus',
    context: { used_tokens: 42_000, window_tokens: 200_000, source: 'derived' },
    turn: 3,
    cost_usd: 0.25,
    tokens_total: 9000,
  }

  it('treats every answer not marked interim as a completed turn, status or not', () => {
    const done = [{ kind: 'graph' }, { kind: 'turn_done', sid: 's1' }]
    expect(translateGlobal(frame({ kind: 'answer', sid: 's1', content: 'hi', status: STATUS }))).toEqual(done)
    // The status-only closing frame (nothing left to say) ends the turn too.
    expect(translateGlobal(frame({ kind: 'answer', sid: 's1', content: '', status: STATUS }))).toEqual(done)
    // A terminal-protocol reply, a slash-command receipt, a notice: no status, still the end.
    expect(translateGlobal(frame({ kind: 'answer', sid: 's1', content: 'switched model → opus' }))).toEqual(done)
    expect(translateGlobal(frame({ kind: 'answer', sid: 's1', content: 'x', interim: false }))).toEqual(done)
  })

  it('an interim answer completes nothing: no turn_done, no tree re-read', () => {
    expect(translateGlobal(frame({ kind: 'answer', sid: 's1', content: 'looking at the tests', interim: true }))).toEqual([])
  })

  it('one long turn feeds the badge exactly once, however much it says mid-turn', () => {
    const turn = [
      frame({ kind: 'answer', sid: 's1', id: 'a1', content: 'first I will read the code', interim: true }),
      frame({ kind: 'progress', sid: 's1', content: '↳ Read(src/bff.ts)', done: true }),
      frame({ kind: 'answer', sid: 's1', id: 'a2', content: 'now the tests', interim: true }),
      frame({ kind: 'progress', sid: 's1', content: '✅ done', done: true }),
      frame({ kind: 'answer', sid: 's1', id: 'a3', content: 'all green', interim: true }),
      frame({ kind: 'answer', sid: 's1', id: 'a4', content: '', status: STATUS }),
    ]
    const events = turn.flatMap(translateGlobal)
    expect(events.filter(event => event.kind === 'turn_done')).toEqual([{ kind: 'turn_done', sid: 's1' }])
    // A final progress card is not a turn end either, so the tree is re-read once.
    expect(events.filter(event => event.kind === 'graph')).toHaveLength(1)
  })

  it('carries the interim mark and the closing status (an object) through to the chat', () => {
    expect(translateSession('s1', frame({ kind: 'answer', sid: 's1', id: 'a4', content: '', status: STATUS }))).toEqual([{
      kind: 'session',
      sid: 's1',
      event: {
        kind: 'answer',
        id: 'a4',
        content: '',
        status: {
          model: 'opus',
          context: { usedTokens: 42_000, windowTokens: 200_000, source: 'derived' },
          turn: 3,
          costUsd: 0.25,
          tokensTotal: 9000,
        },
      },
    }])
    // Null-valued fields drop out rather than becoming NaN/'' noise.
    expect(translateSession('s1', frame({
      kind: 'answer', sid: 's1', id: 'a5', content: 'x',
      status: { model: null, context: null, turn: 1, cost_usd: null, tokens_total: null },
    }))).toEqual([{ kind: 'session', sid: 's1', event: { kind: 'answer', id: 'a5', content: 'x', status: { turn: 1 } } }])
    expect(translateSession('s1', frame({ kind: 'answer', sid: 's1', id: 'a1', content: 'mid-turn', interim: true }))).toEqual([{
      kind: 'session',
      sid: 's1',
      event: { kind: 'answer', id: 'a1', content: 'mid-turn', interim: true },
    }])
    // Only a literal `true` marks a message interim.
    expect(translateSession('s1', frame({ kind: 'answer', sid: 's1', id: 'a2', content: 'end', interim: 'yes' }))).toEqual([{
      kind: 'session',
      sid: 's1',
      event: { kind: 'answer', id: 'a2', content: 'end' },
    }])
  })

  it('does not mistake a human-in-the-loop prompt for a completed turn', () => {
    const prompt = frame({
      kind: 'answer',
      sid: 's1',
      content: 'approve?',
      options: [{ label: 'yes', id: 'y' }],
      token: 'abc',
    })
    expect(translateGlobal(prompt)).toEqual([])
    // The session stream carries it as a choice: options + the resolution token.
    expect(translateSession('s1', prompt)).toEqual([{
      kind: 'session',
      sid: 's1',
      event: { kind: 'answer', id: '', content: 'approve?', options: [{ id: 'y', label: 'yes' }], token: 'abc' },
    }])
  })

  it('ignores control frames and unparseable payloads', () => {
    expect(translateGlobal({ event: 'reconnect_hint', data: '{}' })).toEqual([])
    expect(translateGlobal({ event: 'gateway_unavailable', data: '{}' })).toEqual([])
    expect(translateGlobal({ data: 'not json' })).toEqual([])
    expect(translateSession('s1', { data: 'not json' })).toEqual([])
  })

  it('carries progress snapshots and structured steps, and does not double-count turn_done', () => {
    expect(translateSession('s1', frame({ kind: 'progress', sid: 's1', content: 'reading' })))
      .toEqual([{ kind: 'session', sid: 's1', event: { kind: 'progress', content: 'reading', done: false } }])
    expect(translateSession('s1', frame({ kind: 'progress', sid: 's1', content: '', done: true })))
      .toEqual([{ kind: 'session', sid: 's1', event: { kind: 'progress', content: '', done: true } }])
    expect(translateSession('s1', frame({
      kind: 'activity',
      sid: 's1',
      content: 'Bash(ls)',
      activity: { kind: 'tool_call', name: 'Bash', summary: 'Bash(ls)', status: 'started', item_id: 't1' },
    }))).toEqual([{
      kind: 'session',
      sid: 's1',
      event: { kind: 'activity', step: { itemId: 't1', kind: 'tool_call', name: 'Bash', summary: 'Bash(ls)', status: 'started' } },
    }])
    expect(translateGlobal(frame({ kind: 'session_lifecycle', sid: 's1', state: 'evicted', reason: 'capacity' }))).toEqual([
      { kind: 'graph' },
      { kind: 'session', sid: 's1', event: { kind: 'lifecycle', state: 'evicted', reason: 'capacity' } },
    ])
    expect(translateGlobal(frame({ kind: 'delegation', relation: 'spawned', parent_sid: 's1', child_sid: 's2', title: 'child' }))).toEqual([
      { kind: 'graph' },
      { kind: 'delegation', relation: 'spawned', parentSid: 's1', childSid: 's2', title: 'child' },
    ])
    // turn_done comes from the global stream only.
    const answer = frame({ kind: 'answer', sid: 's1', id: 'e1', content: 'done' })
    expect(translateSession('s1', answer).some(event => event.kind === 'turn_done')).toBe(false)
  })
})
