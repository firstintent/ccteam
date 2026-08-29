import Schema from '@deepseek-ai/schemastery'
import { describe, expect, it, vi } from 'vitest'
import { apply } from '../src/index.js'
import {
  DEFAULT_DAEMON_URL,
  SETTINGS_NAMESPACE,
  registerCcteamSettings,
  type CcteamSettings,
} from '../src/settings.js'
import { FakeFetch, FakeRequest, FakeResponse, FakeWebServer, hostCtx } from './host-fakes.js'

interface Registration {
  ns: string
  schema: Schema<unknown>
  options: { applies?: string; base?: Record<string, unknown> } | undefined
}

interface SchemaNode {
  type: string
  meta?: { default?: unknown; role?: string; description?: string }
  dict?: Record<string, number>
}

/**
 * `Schema.toJSON()` serializes a ref graph — `{uid, refs}` where an object
 * node's `dict` maps each key to a ref id — so field assertions resolve
 * through `refs` rather than reading `dict` as inline nodes.
 */
function describeSchema(schema: Schema<unknown>): {
  type: string
  fields: Record<string, SchemaNode>
} {
  const json = schema.toJSON() as unknown as { uid: number; refs: Record<string, SchemaNode> }
  const root = json.refs[String(json.uid)]!
  const fields: Record<string, SchemaNode> = {}
  for (const [key, ref] of Object.entries(root.dict ?? {})) {
    fields[key] = json.refs[String(ref)]!
  }
  return { type: root.type, fields }
}

function fakeSettings(stored: Partial<CcteamSettings> = {}) {
  const registrations: Registration[] = []
  const service = {
    register<T>(
      ns: string,
      schema: Schema<T>,
      options?: { applies?: 'live' | 'restart'; base?: Partial<T> },
    ) {
      registrations.push({
        ns,
        schema: schema as Schema<unknown>,
        options: options as Registration['options'],
      })
      return {
        get: () => ({
          daemonUrl: DEFAULT_DAEMON_URL,
          enrollment: '',
          restToken: '',
          defaultProject: '',
          connectionStatus: '',
          ...stored,
        }) as T,
      }
    },
  }
  return { service, registrations }
}

describe('settings registration', () => {
  it('registers one namespace whose schema carries the documented fields', () => {
    const { service, registrations } = fakeSettings()
    registerCcteamSettings({ settings: service })

    expect(registrations).toHaveLength(1)
    const [registration] = registrations
    expect(registration!.ns).toBe(SETTINGS_NAMESPACE)
    expect(registration!.ns).toBe('ccteam-ui')

    // Structural assertion on the serialized schema, not a substring match:
    // the bug class here is "right text, wrong shape".
    const { type, fields } = describeSchema(registration!.schema)
    expect(type).toBe('object')
    expect(Object.keys(fields).sort()).toEqual([
      'autoStart',
      'connectionStatus',
      'daemonUrl',
      'defaultProject',
      'enginePath',
      'enrollment',
      'restToken',
    ])
    expect(fields.daemonUrl!.type).toBe('string')
    expect(fields.daemonUrl!.meta!.default).toBe(DEFAULT_DAEMON_URL)
  })

  /**
   * `autoStart` is ON by default because `dsh plugin --profile <name> add @ccteam/ccteam-ui` is
   * meant to be the whole install (PRD G2). It is also the one key whose
   * default is load-bearing in the OTHER direction: it lives on the settings
   * card and NOT in the profile row schema, because Cordis fills every row
   * default before `apply` and a row-level boolean would arrive `true` for
   * every profile, leaving the user's `false` unreachable.
   */
  it('defaults auto-start on and leaves the engine path unset', () => {
    const { service, registrations } = fakeSettings()
    registerCcteamSettings({ settings: service })

    const { fields } = describeSchema(registrations[0]!.schema)
    expect(fields.autoStart!.type).toBe('boolean')
    expect(fields.autoStart!.meta!.default).toBe(true)
    expect(fields.enginePath!.type).toBe('string')
    expect(fields.enginePath!.meta!.default).toBe('')
  })

  it('marks both credentials secret so the settings UI never renders them in the clear', () => {
    const { service, registrations } = fakeSettings()
    registerCcteamSettings({ settings: service })

    const { fields } = describeSchema(registrations[0]!.schema)
    for (const field of ['restToken', 'enrollment'] as const) {
      expect(fields[field]!.meta!.role).toBe('secret')
      expect(fields[field]!.meta!.default).toBe('')
    }
    // Exactly the credentials claim the secret role — nothing else, and nothing missing.
    const secrets = Object.entries(fields)
      .filter(([, field]) => field.meta?.role === 'secret')
      .map(([key]) => key)
      .sort()
    expect(secrets).toEqual(['enrollment', 'restToken'])
  })

  it('passes plugin config through as the settings base', () => {
    const { service, registrations } = fakeSettings()
    registerCcteamSettings({ settings: service }, {
      daemonUrl: 'http://10.0.0.4:7331',
      restToken: 'ccteam:abc',
    })

    expect(registrations[0]!.options?.base).toEqual({
      daemonUrl: 'http://10.0.0.4:7331',
      restToken: 'ccteam:abc',
    })
  })

  it('still resolves values when the settings service is absent', () => {
    const scope = registerCcteamSettings({}, { daemonUrl: 'http://elsewhere:1234' })
    const value = scope.get()

    expect(value.daemonUrl).toBe('http://elsewhere:1234')
    expect(value.restToken).toBe('')
    expect(value.connectionStatus).toContain('ccteam start')
  })

  it('defaults the daemon URL when neither config nor settings supply one', () => {
    expect(registerCcteamSettings({}).get().daemonUrl).toBe(DEFAULT_DAEMON_URL)
  })
})

describe('config-over-settings precedence', () => {
  /**
   * Drives the REAL `apply()` wiring: stub the global fetch, run the route the
   * plugin registered, and read back which daemon URL and token it used. This
   * exercises the precedence closures rather than restating them.
   */
  async function probe(
    config: Partial<CcteamSettings>,
    stored: Partial<CcteamSettings>,
    payload: unknown = {},
    method = 'status',
  ): Promise<FakeFetch> {
    const fake = new FakeFetch()
      .on('/api/v1/capabilities', { json: { harnesses: [] } })
      .on('/sessions', { status: 201, json: { sid: 's1' } })
    vi.stubGlobal('fetch', fake.fetch)
    try {
      const server = new FakeWebServer()
      const { service } = fakeSettings(stored)
      apply(hostCtx({ webServer: server, settings: service }) as never, config)
      const res = new FakeResponse()
      await server.handler()(
        new FakeRequest('POST', `/ccteam/api/${method}`, payload).asIncomingMessage(),
        res.asServerResponse(),
      )
    } finally {
      vi.unstubAllGlobals()
    }
    return fake
  }

  it('prefers a config value over the settings value', async () => {
    const fake = await probe(
      { daemonUrl: 'http://from-config:7331', restToken: 'ccteam:from-config' },
      {
        daemonUrl: 'http://from-settings:7331',
        restToken: 'ccteam:from-settings',
        defaultProject: 'settings-project',
      },
    )

    expect(fake.calls[0]!.url).toBe('http://from-config:7331/api/v1/capabilities')
    expect(fake.calls[0]!.authorization).toBe('Bearer ccteam:from-config')
  })

  it('falls back to the settings value for anything config does not pin', async () => {
    const fake = await probe(
      { daemonUrl: 'http://from-config:7331' },
      { restToken: 'ccteam:from-settings', defaultProject: 'settings-project' },
    )

    expect(fake.calls[0]!.url).toBe('http://from-config:7331/api/v1/capabilities')
    expect(fake.calls[0]!.authorization).toBe('Bearer ccteam:from-settings')
  })

  it('takes the spawn project from the settings card when config is silent', async () => {
    const fake = await probe(
      {},
      { defaultProject: 'settings-project' },
      { vendor: 'claude' },
      'session.spawn',
    )

    expect(fake.calls[0]!.url).toContain('/api/v1/projects/settings-project/sessions')
  })

  /**
   * The regression this exists for: Cordis validates the row config against
   * this plugin's `Config` schema BEFORE apply, so a key the row omits arrives
   * as its schema default (`''`), not as undefined. Under a plain `??` that
   * empty string wins and the settings card is dead — which is exactly how a
   * hand-started `dsh web` is documented to supply its credentials (real
   * machine, DSH 0.1.1-rc.2: "ccteam MCP credential is not configured" with a
   * perfectly good enrollment string in DSH Settings).
   */
  it('treats an empty config value as unpinned, so the settings card still decides', async () => {
    const fake = await probe(
      { daemonUrl: '', restToken: '   ' },
      { daemonUrl: 'http://from-settings:7331', restToken: 'ccteam:from-settings' },
    )

    expect(fake.calls[0]!.url).toBe('http://from-settings:7331/api/v1/capabilities')
    expect(fake.calls[0]!.authorization).toBe('Bearer ccteam:from-settings')
  })

  it('falls back to the built-in daemon URL when neither layer has one', async () => {
    const fake = await probe({ daemonUrl: '' }, { daemonUrl: '' })

    expect(fake.calls[0]!.url).toBe(`${DEFAULT_DAEMON_URL}/api/v1/capabilities`)
  })

  it('apply() wires the settings card and the BFF route together, once', () => {
    const server = new FakeWebServer()
    const { service, registrations } = fakeSettings({ restToken: 'ccteam:stored' })
    apply(hostCtx({ webServer: server, settings: service }) as never)

    expect(registrations).toHaveLength(1)
    expect(server.routes).toHaveLength(1)
    expect(server.routes[0]!.path).toBe('/ccteam/api')
  })

  it('reads the token live, so editing the settings card needs no restart', async () => {
    const fake = new FakeFetch().on('/api/v1/capabilities', { json: { harnesses: [] } })
    let token = 'ccteam:first'
    const { createBff } = await import('../src/bff.js')
    const bff = createBff({
      daemonUrl: () => 'http://127.0.0.1:7331',
      restToken: () => token,
      fetchImpl: fake.fetch,
    })

    const call = async (): Promise<void> => {
      const res = new FakeResponse()
      await bff.handle(
        new FakeRequest('POST', '/ccteam/api/status', {}).asIncomingMessage(),
        res.asServerResponse(),
      )
    }
    await call()
    token = 'ccteam:second'
    await call()

    expect(fake.calls.map(entry => entry.authorization)).toEqual([
      'Bearer ccteam:first',
      'Bearer ccteam:second',
    ])
  })
})
