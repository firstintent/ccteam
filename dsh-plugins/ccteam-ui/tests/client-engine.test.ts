/**
 * The engine as the workbench reasons about it: selectors (dot / label /
 * inert sentence / first-run gate / version relation / enablement / poll
 * cadence), the engine reducers, the action runners (confirmable actions open
 * the Modal and call nothing), and the one poller. Zero React, zero wire.
 */
import { describe, expect, it } from 'vitest'
import {
  ENGINE_POLL_IDLE_MS,
  ENGINE_POLL_TRANSITION_MS,
  compareVersions,
  confirmEngineAction,
  engineDot,
  engineEnablement,
  engineInertKey,
  enginePollMs,
  engineStateKey,
  firstRunState,
  hostDetailShown,
  needsConfirmation,
  refreshEngine,
  relationOf,
  requestEngineAction,
  runEngineAction,
  startEnginePoller,
  stripAnsi,
  truncateMiddle,
  versionRelation,
} from '../src/client/engine.js'
import type { PollScheduler } from '../src/client/engine.js'
import { createStore, initialState, reduce } from '../src/client/store.js'
import type { Action, ConsoleState } from '../src/client/store.js'
import type { ApiClient } from '../src/client/api.js'
import type { EngineStatus } from '../src/shared/contract.js'

function status(over: Partial<EngineStatus> = {}): EngineStatus {
  return {
    state: 'running',
    reachable: true,
    supervised: true,
    daemonUrl: 'http://127.0.0.1:7331',
    pinnedVersion: '0.10.3',
    home: '/home/u/.ccteam',
    binary: '/home/u/.local/bin/ccteam',
    binaryVersion: '0.10.3',
    runningVersion: '0.10.3',
    pid: 42,
    webBind: '127.0.0.1:7331',
    autoStart: true,
    logPath: '/home/u/.ccteam/daemon.log',
    detail: 'ccteam 0.10.3 is running (pid 42).',
    ...over,
  }
}

function fakeApi(handlers: Record<string, (body: unknown) => unknown>) {
  const calls: Array<{ method: string; body: unknown }> = []
  const api = {
    call: async (method: string, body: unknown) => {
      calls.push({ method, body })
      const handler = handlers[method]
      if (handler === undefined) throw new Error(`no handler for ${method}`)
      return handler(body)
    },
    events: () => ({ close() {} }),
    upload: async () => ({ ok: false }),
    attachmentUrl: () => '',
  } as unknown as ApiClient
  return { api, calls }
}

function recorder() {
  const actions: Action[] = []
  return { actions, dispatch: (action: Action) => { actions.push(action) } }
}

function run(actions: Action[], start: ConsoleState = initialState()): ConsoleState {
  return actions.reduce(reduce, start)
}

const settle = () => new Promise<void>(resolve => setTimeout(resolve, 0))

describe('engine dot / label / inert sentence', () => {
  it('maps every state onto its dot color', () => {
    expect(engineDot(null)).toBe('neutral')
    expect(engineDot(status({ state: 'running' }))).toBe('done')
    expect(engineDot(status({ state: 'attached' }))).toBe('done')
    expect(engineDot(status({ state: 'starting' }))).toBe('ongoing')
    expect(engineDot(status({ state: 'installing' }))).toBe('ongoing')
    expect(engineDot(status({ state: 'stopped', reachable: false }))).toBe('neutral')
    expect(engineDot(status({ state: 'missing', reachable: false }))).toBe('warning')
    expect(engineDot(status({ state: 'unsupported' }))).toBe('error')
    expect(engineDot(status({ state: 'mismatch', mismatch: 'home' }))).toBe('error')
    expect(engineDot(status({ state: 'mismatch', mismatch: 'version' }))).toBe('error')
  })

  it('keys the label by state, splitting mismatch by what mismatched', () => {
    expect(engineStateKey(null)).toBe('engine.state.unknown')
    expect(engineStateKey(status({ state: 'attached' }))).toBe('engine.state.attached')
    expect(engineStateKey(status({ state: 'mismatch', mismatch: 'home' }))).toBe('engine.state.mismatchHome')
    expect(engineStateKey(status({ state: 'mismatch', mismatch: 'version' }))).toBe('engine.state.mismatchVersion')
  })

  it('has one sentence per inert reason and none while supervised', () => {
    expect(engineInertKey(status())).toBeNull()
    expect(engineInertKey(status({ supervised: false, unsupervisedReason: 'managed' }))).toBe('engine.inert.managed')
    expect(engineInertKey(status({ supervised: false, unsupervisedReason: 'remote' }))).toBe('engine.inert.remote')
    expect(engineInertKey(status({ supervised: false, unsupervisedReason: 'unsupported', state: 'unsupported' }))).toBe('engine.inert.unsupported')
    expect(engineInertKey(status({ supervised: false }))).toBe('engine.inert.pinned')
  })
})

describe('versionRelation', () => {
  it('orders versions numerically, pre-releases below their release', () => {
    expect(compareVersions('0.10.3', '0.10.4')).toBe(-1)
    expect(compareVersions('1.0.0', '0.99.9')).toBe(1)
    expect(compareVersions('v0.10.3', '0.10.3')).toBe(0)
    expect(compareVersions('0.10.4-alpha.0', '0.10.4')).toBe(-1)
    expect(compareVersions('0.10.4-alpha.1', '0.10.4-alpha.0')).toBe(1)
    expect(compareVersions('0.10.4-beta', '0.10.4-alpha')).toBe(1)
    expect(compareVersions('garbage', '0.10.3')).toBeUndefined()
  })

  it('says which side is older, and unknown when it cannot tell', () => {
    expect(versionRelation('0.10.2', '0.10.3')).toEqual({ kind: 'engine-older', engine: '0.10.2', pinned: '0.10.3' })
    expect(versionRelation('0.10.4', '0.10.3')).toEqual({ kind: 'plugin-older', plugin: '0.10.3', engine: '0.10.4' })
    expect(versionRelation('0.10.3', '0.10.3')).toEqual({ kind: 'match' })
    expect(versionRelation(undefined, '0.10.3')).toEqual({ kind: 'unknown' })
    expect(versionRelation('0.10.3', '')).toEqual({ kind: 'unknown' })
    expect(versionRelation('dev', '0.10.3')).toEqual({ kind: 'unknown' })
  })

  it('prefers the running daemon over the installed binary', () => {
    expect(versionRelation('0.10.3', '0.10.3', '0.10.1')).toEqual({ kind: 'engine-older', engine: '0.10.1', pinned: '0.10.3' })
    expect(relationOf(null)).toEqual({ kind: 'unknown' })
    expect(relationOf(status({ runningVersion: '0.10.9' }))).toEqual({ kind: 'plugin-older', plugin: '0.10.3', engine: '0.10.9' })
  })
})

describe('firstRunState', () => {
  const projects = [{ slug: 'demo' }]

  it('gates on the engine for every state that is not a live daemon', () => {
    for (const state of ['missing', 'stopped', 'starting', 'installing', 'unsupported'] as const) {
      expect(firstRunState(status({ state, reachable: false }), projects), state).toBe('engine-not-ready')
    }
    expect(firstRunState(status({ state: 'mismatch', mismatch: 'home' }), projects)).toBe('engine-not-ready')
  })

  it('treats running, attached, and a version mismatch as a live daemon', () => {
    expect(firstRunState(status({ state: 'running' }), projects)).toBe('ready')
    expect(firstRunState(status({ state: 'attached' }), projects)).toBe('ready')
    expect(firstRunState(status({ state: 'mismatch', mismatch: 'version' }), projects)).toBe('ready')
  })

  it('asks for a workspace only with a loaded, empty catalog; unknown engine is not a gate', () => {
    expect(firstRunState(status(), [])).toBe('no-project')
    expect(firstRunState(status(), null)).toBe('ready')
    expect(firstRunState(status({ state: 'stopped', reachable: false }), [])).toBe('engine-not-ready')
    expect(firstRunState(null, [])).toBe('ready')
  })
})

describe('engineEnablement', () => {
  it('start when stopped or missing; stop/restart while live; update only on an older engine mismatch', () => {
    expect(engineEnablement(status({ state: 'stopped', reachable: false }), null)).toEqual({ start: true, stop: false, restart: false, update: false })
    expect(engineEnablement(status({ state: 'missing', reachable: false, binary: undefined }), null)).toEqual({ start: true, stop: false, restart: false, update: false })
    expect(engineEnablement(status({ state: 'running' }), null)).toEqual({ start: false, stop: true, restart: true, update: false })
    expect(engineEnablement(status({ state: 'attached' }), null)).toEqual({ start: false, stop: true, restart: true, update: false })
    expect(engineEnablement(status({ state: 'mismatch', mismatch: 'version', runningVersion: '0.10.1', binaryVersion: '0.10.1' }), null))
      .toEqual({ start: false, stop: false, restart: false, update: true })
    // The engine is the NEWER side: the repair is `dsh plugin update`, not an engine update.
    expect(engineEnablement(status({ state: 'mismatch', mismatch: 'version', runningVersion: '0.11.0' }), null).update).toBe(false)
    expect(engineEnablement(status({ state: 'mismatch', mismatch: 'home' }), null)).toEqual({ start: false, stop: false, restart: false, update: false })
  })

  it('offers nothing while inert, in flight, or unknown', () => {
    expect(engineEnablement(status({ supervised: false, unsupervisedReason: 'pinned' }), null)).toEqual({ start: false, stop: false, restart: false, update: false })
    expect(engineEnablement(status({ state: 'stopped', reachable: false }), 'start')).toEqual({ start: false, stop: false, restart: false, update: false })
    expect(engineEnablement(null, null)).toEqual({ start: false, stop: false, restart: false, update: false })
  })

  it('shows the host detail only where it adds to the client copy', () => {
    expect(hostDetailShown(null, false)).toBe(false)
    expect(hostDetailShown(status({ state: 'mismatch', mismatch: 'home' }), true)).toBe(true)
    expect(hostDetailShown(status({ state: 'mismatch', mismatch: 'version' }), false)).toBe(true)
    expect(hostDetailShown(status({ state: 'unsupported' }), true)).toBe(true)
    // A live daemon: the sentence carries pid + home, which a facts line already shows.
    expect(hostDetailShown(status({ state: 'running' }), false)).toBe(true)
    expect(hostDetailShown(status({ state: 'attached' }), true)).toBe(false)
    for (const state of ['missing', 'stopped', 'starting', 'installing'] as const) {
      expect(hostDetailShown(status({ state, reachable: false }), false), state).toBe(false)
    }
    expect(hostDetailShown(status({ state: 'mismatch', mismatch: 'home', detail: '' }), false)).toBe(false)
  })

  it('confirms the two actions that take a daemon down', () => {
    expect(needsConfirmation('stop')).toBe(true)
    expect(needsConfirmation('restart')).toBe(true)
    expect(needsConfirmation('start')).toBe(false)
    expect(needsConfirmation('update')).toBe(false)
  })
})

describe('poll cadence', () => {
  it('is 1s through a transition (starting / installing / an action in flight) and 5s otherwise', () => {
    expect(enginePollMs({ status: status(), pending: null })).toBe(ENGINE_POLL_IDLE_MS)
    expect(enginePollMs({ status: null, pending: null })).toBe(ENGINE_POLL_IDLE_MS)
    expect(enginePollMs({ status: status({ state: 'starting' }), pending: null })).toBe(ENGINE_POLL_TRANSITION_MS)
    expect(enginePollMs({ status: status({ state: 'installing' }), pending: null })).toBe(ENGINE_POLL_TRANSITION_MS)
    expect(enginePollMs({ status: status(), pending: 'stop' })).toBe(ENGINE_POLL_TRANSITION_MS)
  })

  it('strips ANSI color escapes from a daemon log line', () => {
    expect(stripAnsi('\u001b[2m2026-08-29T12:13:47Z\u001b[0m \u001b[32m INFO\u001b[0m ccteam start')).toBe('2026-08-29T12:13:47Z  INFO ccteam start')
    expect(stripAnsi('plain')).toBe('plain')
  })

  it('truncates the middle of a long path and leaves short text alone', () => {
    expect(truncateMiddle('/home/u/.ccteam', 40)).toBe('/home/u/.ccteam')
    const shortened = truncateMiddle('/tmp/ccteam-ui-dod/plug4/home/.ccteam', 25)
    expect(shortened).toHaveLength(25)
    expect(shortened.startsWith('/tmp/ccteam')).toBe(true)
    expect(shortened.endsWith('.ccteam')).toBe(true)
    expect(shortened).toContain('…')
  })
})

describe('engine reducers', () => {
  it('counts watchers (never below zero) and keeps the last status across a failed poll', () => {
    let state = run([{ type: 'engine_watch' }, { type: 'engine_watch' }, { type: 'engine_unwatch' }])
    expect(state.engine.watchers).toBe(1)
    state = run([{ type: 'engine_unwatch' }, { type: 'engine_unwatch' }], state)
    expect(state.engine.watchers).toBe(0)
    state = reduce(state, { type: 'engine_loaded', status: status() })
    state = reduce(state, { type: 'engine_failed', message: 'HTTP 502' })
    expect(state.engine.status?.state).toBe('running')
    expect(state.engine.pollError).toBe('HTTP 502')
    state = reduce(state, { type: 'engine_loaded', status: status({ state: 'attached' }) })
    expect(state.engine.pollError).toBeNull()
  })

  it('stages a confirmation, and an action start clears it together with the last error', () => {
    let state = run([{ type: 'engine_confirm', action: 'stop' }])
    expect(state.engine.confirm).toBe('stop')
    state = reduce(state, { type: 'engine_confirm_cancel' })
    expect(state.engine.confirm).toBeNull()
    state = run([
      { type: 'engine_action_failed', action: 'start', message: 'boom' },
      { type: 'engine_confirm', action: 'restart' },
      { type: 'engine_action_started', action: 'restart' },
    ], state)
    expect(state.engine).toMatchObject({ pending: 'restart', confirm: null, error: null })
  })

  it('settles an action with the host status, surfacing a refusal as inline error text', () => {
    let state = run([{ type: 'engine_action_started', action: 'stop' }])
    state = reduce(state, { type: 'engine_action_settled', action: 'stop', result: { ok: true, status: status({ state: 'stopped', reachable: false }) } })
    expect(state.engine).toMatchObject({ pending: null, error: null })
    expect(state.engine.status?.state).toBe('stopped')
    state = reduce(state, { type: 'engine_action_settled', action: 'start', result: { ok: false, status: status({ state: 'stopped', reachable: false }), errorKind: 'packageMissing', error: 'no platform package' } })
    expect(state.engine.error).toBe('no platform package')
    state = reduce(state, { type: 'engine_action_failed', action: 'start', message: 'HTTP 500' })
    expect(state.engine).toMatchObject({ pending: null, error: 'HTTP 500' })
  })

  it('dismisses the banner for the session and opens/closes the engine panel', () => {
    let state = run([{ type: 'engine_dismiss_banner' }])
    expect(state.engine.bannerDismissed).toBe(true)
    expect(reduce(state, { type: 'engine_dismiss_banner' })).toBe(state)
    state = reduce(state, { type: 'select_engine' })
    expect(state.selection).toEqual({ kind: 'engine' })
    expect(reduce(state, { type: 'select_engine' })).toBe(state)
    state = reduce(state, { type: 'clear_selection' })
    expect(state.selection).toEqual({ kind: 'none' })
  })

  it('project creation lands the project in the catalog, points the draft at it, and opens the hero', () => {
    let state = run([{ type: 'projects_loaded', projects: [{ slug: 'zeta' }] }, { type: 'project_create_started' }])
    expect(state.projectCreate).toEqual({ busy: true, error: null })
    state = reduce(state, { type: 'project_create_failed', message: 'project already exists: demo' })
    expect(state.projectCreate).toEqual({ busy: false, error: 'project already exists: demo' })
    state = reduce(state, { type: 'project_create_done', project: { slug: 'demo', host: 'local' } })
    expect(state.projectCreate).toEqual({ busy: false, error: null })
    expect(state.catalogs.projects?.map(p => p.slug)).toEqual(['demo', 'zeta'])
    expect(state.spawn.draft.project).toBe('demo')
    expect(state.selection).toEqual({ kind: 'new' })
    expect(state.graphStale).toBe(true)
  })
})

describe('action runners', () => {
  it('stop and restart open the Modal without calling the host; start runs at once', async () => {
    const { api, calls } = fakeApi({ 'engine.start': () => ({ ok: true, status: status() }) })
    const { actions, dispatch } = recorder()
    await requestEngineAction(dispatch, api, 'stop')
    await requestEngineAction(dispatch, api, 'restart')
    expect(calls).toEqual([])
    expect(actions).toEqual([{ type: 'engine_confirm', action: 'stop' }, { type: 'engine_confirm', action: 'restart' }])
    await requestEngineAction(dispatch, api, 'start')
    expect(calls.map(c => c.method)).toEqual(['engine.start'])
    expect(actions.slice(2)).toEqual([
      { type: 'engine_action_started', action: 'start' },
      { type: 'engine_action_settled', action: 'start', result: { ok: true, status: status() } },
    ])
  })

  it('OK runs exactly the confirmed action; nothing confirmed runs nothing', async () => {
    const { api, calls } = fakeApi({ 'engine.stop': () => ({ ok: true, status: status({ state: 'stopped', reachable: false }) }) })
    const { actions, dispatch } = recorder()
    await confirmEngineAction(dispatch, api, { confirm: null })
    expect(calls).toEqual([])
    await confirmEngineAction(dispatch, api, { confirm: 'stop' })
    expect(calls.map(c => c.method)).toEqual(['engine.stop'])
    expect(actions[0]).toEqual({ type: 'engine_action_started', action: 'stop' })
    expect(actions[1]).toMatchObject({ type: 'engine_action_settled', action: 'stop' })
  })

  it('reports a transport failure as the action error and a failed poll as pollError', async () => {
    const { api } = fakeApi({})
    const { actions, dispatch } = recorder()
    expect(await runEngineAction(dispatch, api, 'update')).toBeNull()
    expect(actions).toEqual([
      { type: 'engine_action_started', action: 'update' },
      { type: 'engine_action_failed', action: 'update', message: 'no handler for engine.update' },
    ])
    expect(await refreshEngine(dispatch, api)).toBeNull()
    expect(actions[2]).toEqual({ type: 'engine_failed', message: 'no handler for engine.status' })
  })
})

function fakeScheduler() {
  let next = 1
  const pending = new Map<number, { callback: () => void; ms: number }>()
  const scheduler: PollScheduler = {
    setTimeout(callback, ms) {
      const handle = next
      next += 1
      pending.set(handle, { callback, ms })
      return handle
    },
    clearTimeout(handle) {
      pending.delete(handle as number)
    },
  }
  return {
    scheduler,
    delays: () => [...pending.values()].map(entry => entry.ms),
    async fire() {
      const [handle, entry] = [...pending.entries()][0]!
      pending.delete(handle)
      entry.callback()
      await settle()
    },
  }
}

describe('engine poller', () => {
  it('refreshes at once when the first seat watches, at the cadence after, and stops when nothing watches', async () => {
    const answers: EngineStatus[] = [status(), status({ state: 'starting', reachable: false }), status()]
    let served = 0
    const { api, calls } = fakeApi({ 'engine.status': () => answers[Math.min(served++, answers.length - 1)] })
    const store = createStore(initialState())
    const timers = fakeScheduler()
    const stop = startEnginePoller(store, api, { scheduler: timers.scheduler })
    await settle()
    expect(calls).toEqual([])

    store.dispatch({ type: 'engine_watch' })
    await settle()
    expect(calls).toHaveLength(1)
    expect(store.getSnapshot().engine.status?.state).toBe('running')
    expect(timers.delays()).toEqual([ENGINE_POLL_IDLE_MS])

    await timers.fire()
    expect(calls).toHaveLength(2)
    expect(store.getSnapshot().engine.status?.state).toBe('starting')
    expect(timers.delays()).toEqual([ENGINE_POLL_TRANSITION_MS])

    store.dispatch({ type: 'engine_unwatch' })
    expect(timers.delays()).toEqual([])
    await settle()
    expect(calls).toHaveLength(2)
    stop()
  })

  it('shortens a pending idle wait when an action starts, and reports reachability flips', async () => {
    const answers: EngineStatus[] = [status(), status({ state: 'stopped', reachable: false })]
    let served = 0
    const { api, calls } = fakeApi({ 'engine.status': () => answers[Math.min(served++, answers.length - 1)] })
    const store = createStore(initialState())
    const timers = fakeScheduler()
    const flips: boolean[] = []
    const stop = startEnginePoller(store, api, { scheduler: timers.scheduler, onReachableChange: reachable => flips.push(reachable) })
    store.dispatch({ type: 'engine_watch' })
    await settle()
    expect(timers.delays()).toEqual([ENGINE_POLL_IDLE_MS])
    store.dispatch({ type: 'engine_action_started', action: 'stop' })
    expect(timers.delays()).toEqual([ENGINE_POLL_TRANSITION_MS])
    await timers.fire()
    expect(calls).toHaveLength(2)
    expect(flips).toEqual([false])
    stop()
    store.dispatch({ type: 'engine_watch' })
    await settle()
    expect(calls).toHaveLength(2)
    expect(timers.delays()).toEqual([])
  })
})
