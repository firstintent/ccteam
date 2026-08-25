/** Store reducers: view stack + Esc, badge, recents, receipts, persistence, tree utils. */
import { describe, expect, it } from 'vitest'
import type { TeamGraph, TeamNode } from '../src/shared/contract.js'
import {
  MAX_RECENTS,
  MAX_WIDTH,
  MIN_WIDTH,
  STORAGE_KEYS,
  attachPersistence,
  clampWidth,
  createStore,
  dotState,
  findNode,
  flattenNodes,
  formatCost,
  initialState,
  loadPersisted,
  planSpawnOutcome,
  projectSlugs,
  reduce,
  vendorGlyph,
} from '../src/client/store.js'
import type { Action, ConsoleState, StorageLike } from '../src/client/store.js'

function run(actions: Action[], start: ConsoleState = initialState()): ConsoleState {
  return actions.reduce(reduce, start)
}

describe('view stack', () => {
  it('starts at the tree and drills into chat and spawn', () => {
    const state = run([{ type: 'open_panel' }, { type: 'open_chat', sid: 's1' }])
    expect(state.stack).toEqual([{ kind: 'tree' }, { kind: 'chat', sid: 's1' }])
  })

  it('Esc walks back: chat -> tree -> closed', () => {
    let state = run([{ type: 'open_panel' }, { type: 'open_chat', sid: 's1' }])
    state = reduce(state, { type: 'back' })
    expect(state.stack).toEqual([{ kind: 'tree' }])
    expect(state.open).toBe(true)
    state = reduce(state, { type: 'back' })
    expect(state.open).toBe(false)
    // Esc while closed is inert.
    expect(reduce(state, { type: 'back' })).toBe(state)
  })

  it('opening a chat from spawn replaces the spawn view (Esc returns to the tree, not the form)', () => {
    const state = run([
      { type: 'open_panel' },
      { type: 'open_spawn' },
      { type: 'open_chat', sid: 's9' },
    ])
    expect(state.stack).toEqual([{ kind: 'tree' }, { kind: 'chat', sid: 's9' }])
  })

  it('switching chats replaces the top instead of stacking chats', () => {
    const state = run([
      { type: 'open_panel' },
      { type: 'open_chat', sid: 's1' },
      { type: 'open_chat', sid: 's2' },
    ])
    expect(state.stack).toEqual([{ kind: 'tree' }, { kind: 'chat', sid: 's2' }])
  })
})

describe('badge', () => {
  it('increments on turn_done while closed and clears on open', () => {
    let state = run([{ type: 'turn_done' }, { type: 'turn_done', sid: 's1' }])
    expect(state.badge).toBe(2)
    state = reduce(state, { type: 'open_panel' })
    expect(state.badge).toBe(0)
  })

  it('does not count turns completing while the panel is open', () => {
    const state = run([{ type: 'open_panel' }, { type: 'turn_done' }])
    expect(state.badge).toBe(0)
  })

  it('toggle_panel opening clears the badge too', () => {
    const state = run([{ type: 'turn_done' }, { type: 'toggle_panel' }])
    expect(state.open).toBe(true)
    expect(state.badge).toBe(0)
  })

  it('turn_done settles the working session back to idle', () => {
    const state = run([
      { type: 'activity', sid: 's1', activity: 'working' },
      { type: 'turn_done', sid: 's1' },
    ])
    expect(state.chats['s1']?.activity).toBe('idle')
  })
})

describe('recents', () => {
  it('dedupes to the front and caps the strip', () => {
    const state = run([
      { type: 'open_chat', sid: 's1' },
      { type: 'open_chat', sid: 's2' },
      { type: 'open_chat', sid: 's3' },
      { type: 'open_chat', sid: 's1' },
      { type: 'open_chat', sid: 's4' },
    ])
    expect(state.recents).toEqual(['s4', 's1', 's3'])
    expect(state.recents.length).toBe(MAX_RECENTS)
  })
})

describe('connection + width', () => {
  it('maps StatusResponse onto the phase', () => {
    expect(run([{ type: 'status_loaded', status: { connected: true } }]).connection.phase).toBe('ok')
    expect(
      run([{ type: 'status_loaded', status: { connected: false, reason: 'unconfigured' } }]).connection.phase,
    ).toBe('unconfigured')
    expect(
      run([{ type: 'status_loaded', status: { connected: false, reason: 'unreachable' } }]).connection.phase,
    ).toBe('unreachable')
    expect(run([{ type: 'status_failed' }]).connection.phase).toBe('unreachable')
  })

  it('clamps the dragged width to the band', () => {
    expect(run([{ type: 'set_width', width: 10 }]).width).toBe(MIN_WIDTH)
    expect(run([{ type: 'set_width', width: 5000 }]).width).toBe(MAX_WIDTH)
    expect(run([{ type: 'set_width', width: 444 }]).width).toBe(444)
    expect(clampWidth(Number.NaN)).toBeGreaterThan(0)
  })
})

describe('transcript merging + receipts', () => {
  it('send_started appends an optimistic row that the canonical server row replaces', () => {
    let state = run([{ type: 'send_started', sid: 's1', text: 'hello' }])
    expect(state.chats['s1']?.rows).toEqual([{ turnId: 'local-1', role: 'user', content: 'hello' }])
    state = reduce(state, { type: 'event_row', sid: 's1', row: { turnId: 't9', role: 'user', content: 'hello' } })
    expect(state.chats['s1']?.rows).toEqual([{ turnId: 't9', role: 'user', content: 'hello' }])
  })

  it('rows dedupe by turnId (history + stream double-delivery)', () => {
    const row = { turnId: 't1', role: 'assistant', content: 'hi' } as const
    const state = run([
      { type: 'history_loaded', sid: 's1', rows: [row] },
      { type: 'event_row', sid: 's1', row },
    ])
    expect(state.chats['s1']?.rows).toEqual([row])
  })

  it('history_loaded keeps in-flight optimistic rows', () => {
    const state = run([
      { type: 'send_started', sid: 's1', text: 'pending' },
      { type: 'history_loaded', sid: 's1', rows: [{ turnId: 't1', role: 'assistant', content: 'old' }] },
    ])
    expect(state.chats['s1']?.rows).toEqual([
      { turnId: 't1', role: 'assistant', content: 'old' },
      { turnId: 'local-1', role: 'user', content: 'pending' },
    ])
  })

  it('a queued receipt surfaces queuedBehind and clears when the turn starts flowing', () => {
    let state = run([
      { type: 'send_started', sid: 's1', text: 'x' },
      { type: 'send_settled', sid: 's1', receipt: { ok: true, queued: true, queuedBehind: 's7' } },
    ])
    expect(state.chats['s1']?.notices).toEqual([{ id: 1, kind: 'queued', queuedBehind: 's7' }])
    state = reduce(state, {
      type: 'event_row',
      sid: 's1',
      row: { turnId: 't2', role: 'assistant', content: 'on it' },
    })
    expect(state.chats['s1']?.notices).toEqual([])
  })

  it('a failed receipt surfaces errorKind and is never swallowed; the next send resets notices', () => {
    let state = run([
      { type: 'send_started', sid: 's1', text: 'x' },
      { type: 'send_settled', sid: 's1', receipt: { ok: false, errorKind: 'SESSION_BUSY', error: 'try later' } },
    ])
    expect(state.chats['s1']?.notices).toEqual([
      { id: 1, kind: 'error', errorKind: 'SESSION_BUSY', message: 'try later' },
    ])
    state = reduce(state, { type: 'send_started', sid: 's1', text: 'again' })
    expect(state.chats['s1']?.notices).toEqual([])
  })

  it('a clean ok receipt leaves no notice', () => {
    const state = run([
      { type: 'send_started', sid: 's1', text: 'x' },
      { type: 'send_settled', sid: 's1', receipt: { ok: true } },
    ])
    expect(state.chats['s1']?.notices).toEqual([])
  })
})

describe('store + persistence', () => {
  function memoryStorage(seed: Record<string, string> = {}): StorageLike & { data: Record<string, string> } {
    const data = { ...seed }
    return {
      data,
      getItem: key => (key in data ? data[key]! : null),
      setItem: (key, value) => {
        data[key] = value
      },
    }
  }

  it('notifies subscribers only on real transitions', () => {
    const store = createStore(initialState())
    let ticks = 0
    store.subscribe(() => {
      ticks += 1
    })
    store.dispatch({ type: 'close_panel' }) // already closed: no-op
    store.dispatch({ type: 'open_panel' })
    expect(ticks).toBe(1)
  })

  it('round-trips open/width/recents/project through storage', () => {
    const storage = memoryStorage()
    const store = createStore(initialState())
    attachPersistence(store, storage)
    store.dispatch({ type: 'open_panel' })
    store.dispatch({ type: 'set_width', width: 500 })
    store.dispatch({ type: 'open_chat', sid: 's3' })
    store.dispatch({ type: 'set_spawn_project', project: 'acme' })
    expect(storage.data[STORAGE_KEYS.open]).toBe('1')
    expect(storage.data[STORAGE_KEYS.width]).toBe('500')
    expect(JSON.parse(storage.data[STORAGE_KEYS.recents]!)).toEqual(['s3'])
    expect(storage.data[STORAGE_KEYS.project]).toBe('acme')

    const restored = initialState(loadPersisted(storage))
    expect(restored.open).toBe(true)
    expect(restored.width).toBe(500)
    expect(restored.recents).toEqual(['s3'])
    expect(restored.spawnProject).toBe('acme')
  })

  it('survives poisoned storage', () => {
    const storage = memoryStorage({ [STORAGE_KEYS.recents]: '{not json', [STORAGE_KEYS.width]: 'wat' })
    expect(loadPersisted(storage)).toEqual({})
    expect(loadPersisted(undefined)).toEqual({})
  })
})

describe('tree utilities', () => {
  const node = (sid: string, children: TeamNode[] = []): TeamNode => ({
    sid,
    project: 'p',
    vendor: 'claude',
    activity: 'idle',
    children,
  })

  it('flattens delegation children under their parents with depth', () => {
    const rows = flattenNodes([node('s1', [node('s2', [node('s3')]), node('s4')]), node('s5')])
    expect(rows.map(row => [row.node.sid, row.depth])).toEqual([
      ['s1', 0],
      ['s2', 1],
      ['s3', 2],
      ['s4', 1],
      ['s5', 0],
    ])
  })

  it('finds nodes anywhere in the graph', () => {
    const graph: TeamGraph = { projects: [{ slug: 'p', nodes: [node('s1', [node('s2')])] }] }
    expect(findNode(graph, 's2')?.sid).toBe('s2')
    expect(findNode(graph, 'nope')).toBeUndefined()
    expect(findNode(null, 's1')).toBeUndefined()
  })

  it('maps activity onto StateDot states', () => {
    expect(dotState('working')).toBe('ongoing')
    expect(dotState('idle')).toBe('done')
    expect(dotState('stale')).toBe('warning')
    expect(dotState('stuck')).toBe('error')
    expect(dotState(undefined)).toBe('done')
  })

  it('formats cost compactly and hides the unknown', () => {
    expect(formatCost(0.4219)).toBe('$0.42')
    expect(formatCost(12.34)).toBe('$12.3')
    expect(formatCost(undefined)).toBeNull()
  })

  it('monograms vendors as text glyphs', () => {
    expect(vendorGlyph('claude')).toBe('cl')
    expect(vendorGlyph('dsh')).toBe('ds')
  })

  it('lists project slugs in graph order', () => {
    const graph: TeamGraph = {
      projects: [
        { slug: 'alpha', nodes: [] },
        { slug: 'beta', nodes: [] },
      ],
    }
    expect(projectSlugs(graph)).toEqual(['alpha', 'beta'])
    expect(projectSlugs(null)).toEqual([])
  })
})

describe('spawn outcome', () => {
  it('a clean spawn goes straight into the chat', () => {
    expect(planSpawnOutcome({ ok: true, sid: 's9' })).toEqual({ kind: 'chat', sid: 's9' })
  })

  it('a sid on a FAILED spawn still navigates in — the session exists upstream — with the error stated', () => {
    expect(planSpawnOutcome({ ok: false, sid: 's9', error: 'first task rejected' })).toEqual({
      kind: 'chat',
      sid: 's9',
      errorMessage: 'first task rejected',
    })
  })

  it('no sid keeps the form up with the actionable error', () => {
    expect(planSpawnOutcome({ ok: false, error: 'name a project: alpha, beta' })).toEqual({
      kind: 'form_error',
      message: 'name a project: alpha, beta',
    })
    expect(planSpawnOutcome({ ok: false })).toEqual({ kind: 'form_error', message: 'unknown' })
  })
})
