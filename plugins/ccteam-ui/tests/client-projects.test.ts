/** The first-run 「添加工作区」 flow: path validation, slug derivation, and the create runner. */
import { describe, expect, it } from 'vitest'
import { createProject, isAbsolutePath, projectCreateRequest, slugFromPath } from '../src/client/projects.js'
import type { ApiClient } from '../src/client/api.js'
import type { Action } from '../src/client/store.js'

function fakeApi(reply: () => unknown) {
  const calls: Array<{ method: string; body: unknown }> = []
  const api = {
    call: async (method: string, body: unknown) => {
      calls.push({ method, body })
      return reply()
    },
  } as unknown as ApiClient
  return { api, calls }
}

function recorder() {
  const actions: Action[] = []
  return { actions, dispatch: (action: Action) => { actions.push(action) } }
}

describe('path and slug rules', () => {
  it('accepts only absolute paths', () => {
    expect(isAbsolutePath('/home/u/demo')).toBe(true)
    expect(isAbsolutePath('demo')).toBe(false)
    expect(isAbsolutePath('./demo')).toBe(false)
    expect(isAbsolutePath('~/demo')).toBe(false)
  })

  it('derives the slug from the basename the way ccteam web does', () => {
    expect(slugFromPath('/home/u/My Project/')).toBe('my-project')
    expect(slugFromPath('/srv/ccteam_hub.v2')).toBe('ccteam-hub-v2')
    expect(slugFromPath('/x/--weird--')).toBe('weird')
    expect(slugFromPath(`/x/${'a'.repeat(70)}`)).toHaveLength(60)
    expect(slugFromPath('/x/日本語')).toBe('')
  })

  it('builds the request: trimmed path, trailing slashes dropped, slug typed or derived', () => {
    expect(projectCreateRequest('  /home/u/demo/  ', '')).toEqual({ path: '/home/u/demo', slug: 'demo' })
    expect(projectCreateRequest('/home/u/demo', ' custom ')).toEqual({ path: '/home/u/demo', slug: 'custom' })
    expect(projectCreateRequest('home/u/demo', '')).toBeNull()
    expect(projectCreateRequest('/x/日本語', '')).toBeNull()
  })
})

describe('createProject', () => {
  it('refuses a relative path client-side without touching the BFF', async () => {
    const { api, calls } = fakeApi(() => ({ ok: true }))
    const { actions, dispatch } = recorder()
    expect(await createProject(dispatch, api, 'relative/dir', '')).toBe(false)
    expect(calls).toEqual([])
    expect(actions).toEqual([])
  })

  it('calls projects.create with the trimmed absolute path and lands the created project', async () => {
    const { api, calls } = fakeApi(() => ({ ok: true, project: { slug: 'demo', path: '/home/u/demo', host: 'local' } }))
    const { actions, dispatch } = recorder()
    expect(await createProject(dispatch, api, '  /home/u/demo/ ', '')).toBe(true)
    expect(calls).toEqual([{ method: 'projects.create', body: { path: '/home/u/demo', slug: 'demo' } }])
    expect(actions).toEqual([
      { type: 'project_create_started' },
      { type: 'project_create_done', project: { slug: 'demo', path: '/home/u/demo', host: 'local' } },
    ])
  })

  it('surfaces the daemon error verbatim, and a transport failure as text', async () => {
    const { api } = fakeApi(() => ({ ok: false, errorKind: 'conflict', error: 'project already exists: demo' }))
    const { actions, dispatch } = recorder()
    expect(await createProject(dispatch, api, '/home/u/demo', 'demo')).toBe(false)
    expect(actions[1]).toEqual({ type: 'project_create_failed', message: 'project already exists: demo' })

    const thrown = { call: async () => { throw new Error('HTTP 502') } } as unknown as ApiClient
    const second = recorder()
    expect(await createProject(second.dispatch, thrown, '/home/u/demo', 'demo')).toBe(false)
    expect(second.actions[1]).toEqual({ type: 'project_create_failed', message: 'HTTP 502' })
  })
})
