/**
 * The engine's half of the browser ⇄ host seam.
 *
 * Two things are under test and nothing else: that the six `engine.*` methods
 * round-trip through the ONE contract both halves import, and that no
 * credential rides along. The card is a browser surface, so "the response body
 * is facts only" is a security property, not a style note.
 */
import { readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { createBff, type EngineFace } from '../src/bff.js'
import { ENGINE_VERSION, apply } from '../src/index.js'
import { DEFAULT_DAEMON_URL } from '../src/settings.js'
import { API_PREFIX, type ApiMethod, type EngineStatus } from '../src/shared/contract.js'
import { FakeFetch, FakeRequest, FakeResponse, FakeWebServer, hostCtx } from './host-fakes.js'

const root = join(dirname(fileURLToPath(import.meta.url)), '..')

const STATUS: EngineStatus = {
  state: 'attached',
  reachable: true,
  supervised: true,
  daemonUrl: 'http://127.0.0.1:7331',
  pinnedVersion: ENGINE_VERSION,
  home: '/home/u/.ccteam',
  daemonHome: '/home/u/.ccteam',
  binary: '/home/u/.local/bin/ccteam',
  binarySource: 'path',
  binaryVersion: ENGINE_VERSION,
  runningVersion: ENGINE_VERSION,
  platform: 'linux-x64',
  pid: 4242,
  webBind: '127.0.0.1:7331',
  uptimeSecs: 12,
  autoStart: true,
  logPath: '/home/u/.ccteam/daemon.log',
  detail: 'attached to the ccteam daemon already running in /home/u/.ccteam (pid 4242).',
}

function fakeEngine(): { face: EngineFace; calls: string[] } {
  const calls: string[] = []
  const face: EngineFace = {
    status: async () => {
      calls.push('status')
      return STATUS
    },
    start: async () => {
      calls.push('start')
      return { ok: true, status: STATUS }
    },
    stop: async () => {
      calls.push('stop')
      return { ok: true, status: { ...STATUS, state: 'stopped', reachable: false } }
    },
    restart: async () => {
      calls.push('restart')
      return { ok: true, status: STATUS }
    },
    update: async () => {
      calls.push('update')
      return { ok: true, status: STATUS }
    },
    log: async lines => {
      calls.push(`log:${String(lines)}`)
      return { ok: true, path: STATUS.logPath, lines: ['a', 'b'] }
    },
  }
  return { face, calls }
}

async function post(bff: ReturnType<typeof createBff>, method: string, body: unknown = {}): Promise<FakeResponse> {
  const res = new FakeResponse()
  await bff.handle(new FakeRequest('POST', `${API_PREFIX}/${method}`, body).asIncomingMessage(), res.asServerResponse())
  return res
}

const ENGINE_METHODS: ApiMethod[] = [
  'engine.status',
  'engine.start',
  'engine.stop',
  'engine.restart',
  'engine.update',
  'engine.log',
]

describe('engine methods through the BFF', () => {
  it('routes every engine method to the engine face and answers the contract shape', async () => {
    const { face, calls } = fakeEngine()
    const bff = createBff({
      daemonUrl: () => 'http://127.0.0.1:7331',
      restToken: () => 'ccteam:deadbeefcafe',
      fetchImpl: new FakeFetch().fetch,
      engine: face,
    })

    for (const method of ENGINE_METHODS) {
      const res = await post(bff, method, method === 'engine.log' ? { lines: 5 } : {})
      expect(res.statusCode, method).toBe(200)
    }

    expect(calls).toEqual(['status', 'start', 'stop', 'restart', 'update', 'log:5'])

    const status = (await post(bff, 'engine.status')).json() as EngineStatus
    expect(status.state).toBe('attached')
    expect(status.pid).toBe(4242)
    expect(status.pinnedVersion).toBe(ENGINE_VERSION)
    // No credential reaches the browser, in any field (D19).
    expect(JSON.stringify(status)).not.toContain('deadbeefcafe')
  })

  it('answers honestly, not emptily, on a runtime with no engine face', async () => {
    const bff = createBff({
      daemonUrl: () => 'http://127.0.0.1:7331',
      restToken: () => '',
      fetchImpl: new FakeFetch().fetch,
    })

    const status = (await post(bff, 'engine.status')).json() as EngineStatus
    expect(status.state).toBe('unsupported')
    expect(status.supervised).toBe(false)
    expect(status.detail).toContain('does not manage a ccteam engine')

    const started = (await post(bff, 'engine.start')).json() as { ok: boolean; errorKind?: string }
    expect(started.ok).toBe(false)
    expect(started.errorKind).toBe('unavailable')

    const log = (await post(bff, 'engine.log')).json() as { ok: boolean; lines: string[] }
    expect(log.ok).toBe(false)
    expect(log.lines).toEqual([])
  })

  it('still refuses a method that is not on the contract', async () => {
    const { face } = fakeEngine()
    const bff = createBff({
      daemonUrl: () => 'http://127.0.0.1:7331',
      restToken: () => '',
      fetchImpl: new FakeFetch().fetch,
      engine: face,
    })

    const res = await post(bff, 'engine.destroy')
    expect(res.statusCode).toBe(404)
  })
})

/**
 * Cordis validates the profile row against this plugin's `Config` schema and
 * fills every default BEFORE `apply` runs, so a row that mentioned nothing
 * still arrives fully populated. For a field whose default is EMPTY that is
 * harmless; for `daemonUrl`, whose default is a real URL, it decides two
 * things at once — whether the settings card can win, and whether the profile
 * looks like one whose engine somebody else owns.
 *
 * Both were wrong on a real machine before these tests existed: a hand-added
 * plugin reported `unsupervisedReason: "pinned"` and ignored the daemon URL
 * its own settings card was showing.
 */
describe('a schema default is a blank row, not a pin', () => {
  function applied(config: Record<string, unknown>, card: Record<string, unknown> = {}) {
    const server = new FakeWebServer()
    const ctx = hostCtx({
      webServer: server,
      settings: {
        register: () => ({
          get: () => ({
            daemonUrl: DEFAULT_DAEMON_URL,
            enrollment: '',
            restToken: '',
            defaultProject: '',
            connectionStatus: '',
            autoStart: false,
            enginePath: '',
            ...card,
          }),
        }),
      },
    })
    apply(ctx as never, config as never)
    return server
  }

  async function engineStatusOf(server: FakeWebServer): Promise<EngineStatus> {
    const res = new FakeResponse()
    await server.handler()(
      new FakeRequest('POST', `${API_PREFIX}/engine.status`, {}).asIncomingMessage(),
      res.asServerResponse(),
    )
    return res.json() as EngineStatus
  }

  it('lets the settings card set the daemon URL over the row default', async () => {
    const status = await engineStatusOf(
      applied({ daemonUrl: DEFAULT_DAEMON_URL }, { daemonUrl: 'http://127.0.0.1:19555' }),
    )

    expect(status.daemonUrl).toBe('http://127.0.0.1:19555')
    expect(status.daemonUrlSource).toBe('configured')
    // A card-entered URL is the user's own instruction, so it stays supervised.
    expect(status.supervised).toBe(true)
  })

  it('does not read a row full of defaults as an engine somebody else owns', async () => {
    const status = await engineStatusOf(applied({ daemonUrl: DEFAULT_DAEMON_URL, restToken: '', enrollment: '' }))

    expect(status.unsupervisedReason).toBeUndefined()
    expect(status.supervised).toBe(true)
  })

  it('does read a ccteam-materialized row — one carrying credentials — as exactly that', async () => {
    const status = await engineStatusOf(
      applied({ daemonUrl: 'http://127.0.0.1:7331', restToken: 'ccteam:deadbeef' }),
    )

    expect(status.supervised).toBe(false)
    expect(status.unsupervisedReason).toBe('pinned')
  })
})

/**
 * Pre-1.0 the plugin and the engine move in lockstep (PRD D5). Three places
 * carry that version and a drift between any two ships a plugin that reports a
 * mismatch against an engine it actually agrees with.
 */
describe('the pinned engine version', () => {
  const pkg = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8')) as {
    ccteam: { engine: string }
    optionalDependencies: Record<string, string>
  }

  it('matches package.json ccteam.engine and every platform package', () => {
    expect(ENGINE_VERSION).toBe(pkg.ccteam.engine)
    expect(Object.keys(pkg.optionalDependencies).sort()).toEqual([
      '@ccteam/engine-darwin-arm64',
      '@ccteam/engine-darwin-x64',
      '@ccteam/engine-linux-arm64',
      '@ccteam/engine-linux-x64',
    ])
    for (const [name, spec] of Object.entries(pkg.optionalDependencies)) {
      expect(spec, name).toBe(ENGINE_VERSION)
    }
  })

  it('declares os/cpu on every platform package template, and a 755 bin', () => {
    for (const tuple of ['linux-x64', 'linux-arm64', 'darwin-x64', 'darwin-arm64']) {
      const template = JSON.parse(
        readFileSync(join(root, '..', 'engine-packages', tuple, 'package.json'), 'utf8'),
      ) as { name: string; os: string[]; cpu: string[]; bin: Record<string, string>; files: string[] }
      const [os, cpu] = tuple.split('-')
      expect(template.name).toBe(`@ccteam/engine-${tuple}`)
      expect(template.os).toEqual([os])
      expect(template.cpu).toEqual([cpu])
      expect(template.bin).toEqual({ ccteam: 'bin/ccteam' })
      expect(template.files).toEqual(['bin/ccteam'])
    }
  })
})
