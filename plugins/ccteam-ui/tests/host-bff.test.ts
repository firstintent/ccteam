import { describe, expect, it } from 'vitest'
import { createBff, registerBff } from '../src/bff.js'
import { API_PREFIX } from '../src/shared/contract.js'
import { FakeFetch, FakeRequest, FakeResponse, FakeWebServer } from './host-fakes.js'

const TOKEN = 'ccteam:deadbeefcafe'

function bff(fake: FakeFetch, token = TOKEN, defaultProject = '') {
  return createBff({
    daemonUrl: () => 'http://127.0.0.1:7331/',
    restToken: () => token,
    defaultProject: () => defaultProject,
    fetchImpl: fake.fetch,
  })
}

async function post(
  instance: ReturnType<typeof bff>,
  method: string,
  body: unknown = {},
): Promise<FakeResponse> {
  const res = new FakeResponse()
  await instance.handle(
    new FakeRequest('POST', `${API_PREFIX}/${method}`, body).asIncomingMessage(),
    res.asServerResponse(),
  )
  return res
}

describe('route registration', () => {
  it('claims the API prefix exactly once and hands back a disposer', () => {
    const server = new FakeWebServer()
    const dispose = registerBff({ webServer: server }, {
      daemonUrl: () => 'http://127.0.0.1:7331',
      restToken: () => TOKEN,
      fetchImpl: new FakeFetch().fetch,
    })

    expect(server.routes).toHaveLength(1)
    expect(server.routes[0]!.kind).toBe('prefix')
    expect(server.routes[0]!.path).toBe('/ccteam/api')

    dispose()
    expect(server.routes).toHaveLength(0)
  })

  it('would throw if it registered the same route twice (real webServer semantics)', () => {
    const server = new FakeWebServer()
    const options = {
      daemonUrl: () => 'http://127.0.0.1:7331',
      restToken: () => TOKEN,
      fetchImpl: new FakeFetch().fetch,
    }
    registerBff({ webServer: server }, options)
    expect(() => registerBff({ webServer: server }, options)).toThrow(/duplicate prefix route/)
  })

  it('hands the disposer to ctx.effect so the fiber tears the route down', () => {
    const server = new FakeWebServer()
    const effects: Array<{ label: string | undefined; teardown: unknown }> = []
    registerBff({
      webServer: server,
      effect: (setup, label) => {
        const teardown = setup()
        effects.push({ label, teardown })
        return () => {}
      },
    }, {
      daemonUrl: () => 'http://127.0.0.1:7331',
      restToken: () => TOKEN,
      fetchImpl: new FakeFetch().fetch,
    })

    expect(effects).toHaveLength(1)
    expect(effects[0]!.label).toBe('ccteam-ui.bff')
    expect(typeof effects[0]!.teardown).toBe('function')
  })
})

describe('method dispatch', () => {
  it('status probes capabilities and maps vendor availability', async () => {
    const fake = new FakeFetch().on('/api/v1/capabilities', {
      json: {
        harnesses: [
          { id: 'claude-code', vendor: 'claude', available: true },
          { id: 'codex', vendor: 'codex', available: false },
        ],
        daemon_timezone: 'UTC',
      },
    })
    const res = await post(bff(fake), 'status')

    expect(res.json()).toEqual({
      connected: true,
      vendors: [
        { vendor: 'claude', installed: true },
        { vendor: 'codex', installed: false },
      ],
    })
    expect(fake.calls[0]!.url).toBe('http://127.0.0.1:7331/api/v1/capabilities')
    expect(fake.calls[0]!.authorization).toBe(`Bearer ${TOKEN}`)
  })

  it('reports unreachable on a network error and unconfigured on a 401', async () => {
    const down = new FakeFetch().on('/api/v1/capabilities', { networkError: 'ECONNREFUSED' })
    expect((await post(bff(down), 'status')).json()).toEqual({
      connected: false,
      reason: 'unreachable',
    })

    // ccteam answers 401 as text/plain "auth required", never JSON.
    const denied = new FakeFetch().on('/api/v1/capabilities', { status: 401, text: 'auth required' })
    expect((await post(bff(denied, ''), 'status')).json()).toEqual({
      connected: false,
      reason: 'unconfigured',
    })
  })

  it('sends a bare hex token as Bearer ccteam:<hex> and omits the header when unset', async () => {
    const fake = new FakeFetch().on('/api/v1/capabilities', { json: { harnesses: [] } })
    await post(bff(fake, 'deadbeefcafe'), 'status')
    expect(fake.calls[0]!.authorization).toBe('Bearer ccteam:deadbeefcafe')

    const anon = new FakeFetch().on('/api/v1/capabilities', { json: { harnesses: [] } })
    await post(bff(anon, '   '), 'status')
    expect(anon.calls[0]!.authorization).toBeUndefined()
  })

  it('team.graph nests the flat node list into a per-project forest', async () => {
    const fake = new FakeFetch().on('/api/v1/agents/graph', {
      json: {
        nodes: [
          {
            sid: 's1', slug: 'ccteam', role: 'cto', vendor: 'claude', host: 'local',
            status: 'working', residency: 'resident', depth: 0, cost_usd: 1.5, tokens_total: 42,
            title: 'root', last_active: '2026-08-25T10:00:00Z', turn_count: 3,
          },
          {
            sid: 's2', slug: 'ccteam', role: '', vendor: 'codex', host: 'local',
            status: 'idle', residency: 'released', depth: 1, parent_sid: 's1',
            last_active: '2026-08-25T10:05:00Z', turn_count: 1,
          },
          {
            sid: 's3', slug: 'other', role: '', vendor: 'dsh', host: 'local',
            status: 'idle', residency: 'bogus', depth: 0, last_active: '', turn_count: 0,
          },
        ],
        edges: [],
        hosts: ['local'],
      },
    })
    const graph = (await post(bff(fake), 'team.graph')).json() as {
      projects: Array<{ slug: string; nodes: unknown[] }>
    }

    expect(fake.calls[0]!.url).toBe('http://127.0.0.1:7331/api/v1/agents/graph')
    expect(graph.projects.map(project => project.slug)).toEqual(['ccteam', 'other'])
    expect(graph.projects[0]!.nodes).toEqual([{
      sid: 's1',
      project: 'ccteam',
      vendor: 'claude',
      activity: 'working',
      residency: 'resident',
      title: 'root',
      role: 'cto',
      host: 'local',
      costUsd: 1.5,
      tokensTotal: 42,
      lastActive: '2026-08-25T10:00:00Z',
      turnCount: 3,
      children: [{
        sid: 's2',
        project: 'ccteam',
        vendor: 'codex',
        activity: 'idle',
        residency: 'released',
        host: 'local',
        parentSid: 's1',
        lastActive: '2026-08-25T10:05:00Z',
        turnCount: 1,
        children: [],
      }],
    }])
  })

  it('session.history splits each two-sided turn into contract rows and pages by cursor', async () => {
    const page = {
      json: {
        sid: 's1',
        events: [
          { turn_id: 's1-1', ts: '2026-08-25T10:00:00Z', role: 'cto', user: 'hi', assistant: 'hello' },
          { turn_id: 's1-2', ts: '2026-08-25T10:01:00Z', role: 'cto', user: 'again', assistant: '' },
        ],
        next_before: null,
        has_more: false,
      },
    }
    const fake = new FakeFetch().on('/api/v1/sessions/s1?', page)
    const all = (await post(bff(fake), 'session.history', { sid: 's1' })).json()

    expect(fake.calls[0]!.url).toBe('http://127.0.0.1:7331/api/v1/sessions/s1?limit=100')
    expect(all).toEqual({
      sid: 's1',
      hasMore: false,
      rows: [
        { turnId: 's1-1:user', role: 'user', content: 'hi', ts: '2026-08-25T10:00:00Z' },
        { turnId: 's1-1:assistant', role: 'assistant', content: 'hello', ts: '2026-08-25T10:00:00Z' },
        { turnId: 's1-2:user', role: 'user', content: 'again', ts: '2026-08-25T10:01:00Z' },
      ],
    })

    // An older page rides the opaque cursor and reports the next one.
    const paged = new FakeFetch().on('/api/v1/sessions/s1?', {
      json: { ...page.json, next_before: 'cursor-2', has_more: true },
    })
    const older = (await post(bff(paged), 'session.history', {
      sid: 's1',
      before: 'cursor-1',
      limit: 50,
    })).json() as { rows: unknown[]; hasMore: boolean; nextBefore?: string }
    expect(paged.calls[0]!.url).toBe('http://127.0.0.1:7331/api/v1/sessions/s1?limit=50&before=cursor-1')
    expect(older.rows).toHaveLength(3)
    expect(older.hasMore).toBe(true)
    expect(older.nextBefore).toBe('cursor-2')
  })

  it('session.send posts JSON to the turn route and reports a plain acceptance', async () => {
    const fake = new FakeFetch().on('/turn', { status: 202, json: { accepted: true } })
    const res = await post(bff(fake), 'session.send', { sid: 's1', text: 'go' })

    expect(res.json()).toEqual({ ok: true })
    const call = fake.calls[0]!
    expect(call.method).toBe('POST')
    expect(call.url).toBe('http://127.0.0.1:7331/api/v1/sessions/s1/turn')
    expect(call.authorization).toBe(`Bearer ${TOKEN}`)
    // FormOrJson upstream: anything but exact application/json is urlencoded.
    expect(call.headers['content-type']).toBe('application/json')
    expect(call.body).toEqual({ text: 'go' })
  })

  it('session.send surfaces the queued receipt instead of swallowing it', async () => {
    const fake = new FakeFetch().on('/turn', {
      status: 202,
      json: {
        accepted: true,
        queued: true,
        queued_behind: 'detached_body',
        turn_id: 'queued-behind-body:s1:2',
      },
    })
    expect((await post(bff(fake), 'session.send', { sid: 's1', text: 'go' })).json()).toEqual({
      ok: true,
      queued: true,
      queuedBehind: 'detached_body',
    })
  })

  it('session.send maps upstream failures to an honest receipt', async () => {
    const notFound = new FakeFetch().on('/turn', {
      status: 404,
      json: { error: 'unknown session: s9' },
    })
    expect((await post(bff(notFound), 'session.send', { sid: 's9', text: 'go' })).json()).toEqual({
      ok: false,
      errorKind: 'not_found',
      error: 'unknown session: s9',
    })

    // A GatewayRequestError carries a stable machine code; prefer it.
    const conflict = new FakeFetch().on('/turn', {
      status: 409,
      json: { ok: false, error: 'body detached', error_code: 'session_body_detached' },
    })
    expect((await post(bff(conflict), 'session.send', { sid: 's1', text: 'x' })).json())
      .toEqual({ ok: false, errorKind: 'session_body_detached', error: 'body detached' })

    const offline = new FakeFetch().on('/turn', { networkError: 'ECONNREFUSED' })
    expect((await post(bff(offline), 'session.send', { sid: 's1', text: 'go' })).json()).toEqual({
      ok: false,
      errorKind: 'unreachable',
      error: 'ECONNREFUSED',
    })
  })

  it('session.spawn creates in the named project, never sending a host key', async () => {
    const fake = new FakeFetch()
      .on('/api/v1/projects/ccteam/sessions', { status: 201, json: { sid: 's7' } })
    expect((await post(bff(fake), 'session.spawn', {
      project: 'ccteam',
      vendor: 'dsh',
      model: 'deepseek-chat',
      mode: 'standard',
    })).json()).toEqual({ ok: true, sid: 's7' })

    const body = fake.calls[0]!.body as Record<string, unknown>
    // `role` must be PRESENT (empty = roleless); `host` present at all is a 400.
    expect(body).toEqual({ role: '', vendor: 'dsh', model: 'deepseek-chat', mode: 'standard' })
    expect('host' in body).toBe(false)
  })

  it('session.spawn follows up with the title and first task', async () => {
    const fake = new FakeFetch()
      .on('/api/v1/projects/ccteam/sessions', { status: 201, json: { sid: 's7' } })
      .on('/api/v1/sessions/s7/turn', { status: 202, json: { accepted: true } })
      .on('/api/v1/sessions/s7', { status: 200, json: { sid: 's7', title: 'review' } })
    expect((await post(bff(fake), 'session.spawn', {
      project: 'ccteam',
      vendor: 'claude',
      title: 'review',
      task: 'review the diff',
    })).json()).toEqual({ ok: true, sid: 's7' })

    expect(fake.calls.map(call => `${call.method} ${new URL(call.url).pathname}`)).toEqual([
      'POST /api/v1/projects/ccteam/sessions',
      'PATCH /api/v1/sessions/s7',
      'POST /api/v1/sessions/s7/turn',
    ])
  })

  it('session.spawn reports the sid when the session exists but its first task failed', async () => {
    const fake = new FakeFetch()
      .on('/api/v1/projects/ccteam/sessions', { status: 201, json: { sid: 's7' } })
      .on('/api/v1/sessions/s7/turn', { status: 502, json: { ok: false, error: 'vendor timeout' } })
    const spawned = (await post(bff(fake), 'session.spawn', {
      project: 'ccteam',
      vendor: 'claude',
      task: 'go',
    })).json() as { ok: boolean; sid?: string; error?: string }

    expect(spawned.ok).toBe(false)
    expect(spawned.sid).toBe('s7')
    expect(spawned.error).toContain('vendor timeout')
  })

  it('session.spawn refuses to guess a project and says so', async () => {
    const fake = new FakeFetch()
    const spawned = (await post(bff(fake), 'session.spawn', { vendor: 'claude' })).json() as {
      ok: boolean
      error?: string
    }

    expect(spawned.ok).toBe(false)
    expect(spawned.error).toContain('no project selected')
    expect(fake.calls).toHaveLength(0)
  })

  it('falls back to the configured default project', async () => {
    const fake = new FakeFetch()
      .on('/api/v1/projects/fallback/sessions', { status: 201, json: { sid: 's8' } })
    expect((await post(bff(fake, TOKEN, 'fallback'), 'session.spawn', { vendor: 'claude' })).json())
      .toEqual({ ok: true, sid: 's8' })
  })
})

describe('unknown routes', () => {
  it('404s an unknown method without touching upstream', async () => {
    const fake = new FakeFetch()
    const res = await post(bff(fake), 'session.destroy')

    expect(res.statusCode).toBe(404)
    expect(res.json()).toEqual({ error: 'unknown method: session.destroy' })
    expect(fake.calls).toHaveLength(0)
  })

  it('404s a GET on a method and a POST on an unrelated path', async () => {
    const instance = bff(new FakeFetch())

    const wrongVerb = new FakeResponse()
    await instance.handle(
      new FakeRequest('GET', `${API_PREFIX}/status`).asIncomingMessage(),
      wrongVerb.asServerResponse(),
    )
    expect(wrongVerb.statusCode).toBe(404)

    const foreign = new FakeResponse()
    await instance.handle(
      new FakeRequest('POST', '/ccteam/apiary/status').asIncomingMessage(),
      foreign.asServerResponse(),
    )
    expect(foreign.statusCode).toBe(404)
    expect(foreign.json()).toEqual({ error: 'not found' })
  })
})
