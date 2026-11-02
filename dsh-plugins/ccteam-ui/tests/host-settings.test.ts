import { updateVolatile } from '@deepseek-ai/cosmokit'
import { afterEach, describe, expect, it, vi } from 'vitest'
import { Config, apply } from '../src/index.js'
import { bindCcteamSettings, DEFAULT_DAEMON_URL } from '../src/settings.js'
import { FakeRequest, FakeResponse, FakeWebServer, hostCtx } from './host-fakes.js'
import { API_PREFIX } from '../src/shared/contract.js'

afterEach(async () => {
  vi.unstubAllGlobals()
})

describe('native live Config', () => {
  it('resolves defaults and exposes one volatile reference per preference', () => {
    const config = Config({})
    expect(bindCcteamSettings({}, config).get()).toEqual({
      daemonUrl: DEFAULT_DAEMON_URL, enrollment: '', restToken: '', defaultProject: '',
      connectionStatus: expect.any(String), autoStart: true, enginePath: '',
    })
    expect(config.engineManaged).toBe(false)
    expect(config.transportSocket).toBeUndefined()
  })

  it('reads the same native references after a Loader volatile update', () => {
    const config = Config({ enrollment: 'first', autoStart: false })
    const settings = bindCcteamSettings({}, config)
    // This is the reference commit operation used by Loader Entry.update.
    updateVolatile(config.enrollment, Config({ enrollment: 'second' }).enrollment)
    expect(settings.get().enrollment).toBe('second')
  })

  it('persists bootstrap enrollment into its actual Loader entry', async () => {
    const update = vi.fn(async () => {})
    const settings = bindCcteamSettings({
      get: name => name === 'settings' ? { update } : undefined,
      fiber: { entry: { id: 'user-selected-ccteam-entry' } },
    }, Config({}))
    await settings.update?.({ enrollment: 'ccteam-enroll:e1:secret' })
    expect(update).toHaveBeenCalledWith('user-selected-ccteam-entry', { enrollment: 'ccteam-enroll:e1:secret' })
  })

  it('serves the workbench without a settings service or agent runtime', async () => {
    const fetch = vi.fn(async () => new Response(JSON.stringify({ projects: [] })))
    vi.stubGlobal('fetch', fetch)
    const webServer = new FakeWebServer()
    apply(hostCtx({ webServer }) as never, Config({
      autoStart: false, daemonUrl: 'http://daemon.test', restToken: 'personal',
    }))
    const response = new FakeResponse()
    await webServer.handler()(new FakeRequest('POST', `${API_PREFIX}/catalog.projects`, {}).asIncomingMessage(), response.asServerResponse())
    expect(fetch).toHaveBeenCalled()
    expect(response.statusCode).toBe(200)
    expect(JSON.stringify(response.json())).not.toContain('personal')
  })

  it('declares credentials as secret volatile fields in the native schema', () => {
    const schema = Config.toJSON() as unknown as { refs: Record<string, { meta?: { role?: string; volatile?: boolean } }> }
    const secrets = Object.values(schema.refs).filter(node => node.meta?.role === 'secret')
    expect(secrets).toHaveLength(2)
    expect(secrets.every(node => node.meta?.volatile === true)).toBe(true)
  })
})
