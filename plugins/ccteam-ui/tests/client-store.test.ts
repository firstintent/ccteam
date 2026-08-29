/** Store reducers: selection, chats (canonical/live/choice), badge, recents, persistence, tree utils. */
import { describe, expect, it } from 'vitest'
import {
  MAX_RECENTS,
  STORAGE_KEYS,
  attachPersistence,
  chatOf,
  createStore,
  dotState,
  effortsFor,
  filterRows,
  findNode,
  flattenNodes,
  initialState,
  lifecycleCopyKey,
  loadPersisted,
  planSpawnOutcome,
  reduce,
} from '../src/client/store.js'
import type { Action, ConsoleState, StorageLike } from '../src/client/store.js'
import { formatCost, formatElapsed, formatTokens, modelDirective, relativeTime, titleFromTask, vendorGlyph } from '../src/client/format.js'
import type { TeamGraph, TeamNode } from '../src/shared/contract.js'

function run(actions: Action[], start = initialState()): ConsoleState {
  return actions.reduce(reduce, start)
}

const NOW = 1_700_000_000_000

describe('selection', () => {
  it('selecting a session records it as a recent and clears the step selection', () => {
    let state = run([{ type: 'open_panel' }, { type: 'select_step', sid: 's0', itemId: 'x' }])
    state = reduce(state, { type: 'select_session', sid: 's1' })
    expect(state.selection).toEqual({ kind: 'session', sid: 's1' })
    expect(state.recents).toEqual(['s1'])
    expect(state.details.step).toBeNull()
    expect(state.details.open).toBe(true)
  })

  it('re-selecting the current session is a no-op', () => {
    const state = run([{ type: 'select_session', sid: 's1' }])
    expect(reduce(state, { type: 'select_session', sid: 's1' })).toBe(state)
  })

  it('new-session and clear switch the main column', () => {
    let state = run([{ type: 'select_session', sid: 's1' }, { type: 'select_new' }])
    expect(state.selection).toEqual({ kind: 'new' })
    state = reduce(state, { type: 'clear_selection' })
    expect(state.selection).toEqual({ kind: 'none' })
  })

  it('details open on toggle and close on Esc-path actions', () => {
    let state = run([{ type: 'toggle_details' }])
    expect(state.details.open).toBe(true)
    state = reduce(state, { type: 'close_details' })
    expect(state.details.open).toBe(false)
    expect(reduce(state, { type: 'close_details' })).toBe(state)
  })
})

describe('badge', () => {
  it('counts completed turns only while closed and resets on open', () => {
    let state = run([{ type: 'turn_done' }, { type: 'turn_done' }])
    expect(state.badge).toBe(2)
    state = reduce(state, { type: 'open_panel' })
    expect(state.badge).toBe(0)
    state = reduce(state, { type: 'turn_done' })
    expect(state.badge).toBe(0)
  })

  it('turn_done settles a working chat without content back to idle', () => {
    let state = run([
      { type: 'session_event', sid: 's1', event: { kind: 'progress', content: '', done: false }, now: NOW },
    ])
    expect(chatOf(state, 's1').activity).toBe('working')
    state = reduce(state, { type: 'turn_done', sid: 's1' })
    expect(chatOf(state, 's1').activity).toBe('idle')
    expect(chatOf(state, 's1').live).toBeNull()
  })
})

describe('recents', () => {
  it('keeps the most recent first, deduped, capped', () => {
    const state = run([
      { type: 'select_session', sid: 's1' },
      { type: 'select_session', sid: 's2' },
      { type: 'select_session', sid: 's3' },
      { type: 'select_session', sid: 's1' },
      { type: 'select_session', sid: 's4' },
      { type: 'select_session', sid: 's5' },
      { type: 'select_session', sid: 's6' },
    ])
    expect(state.recents.length).toBeLessThanOrEqual(MAX_RECENTS)
    expect(state.recents[0]).toBe('s6')
    expect(new Set(state.recents).size).toBe(state.recents.length)
  })
})

describe('chat: live turn', () => {
  it('progress content is a snapshot, steps upsert by item id and complete on done', () => {
    let state = run([
      { type: 'session_event', sid: 's1', event: { kind: 'activity', step: { itemId: 't1', kind: 'tool_call', name: 'Bash', summary: 'ls', status: 'started' } }, now: NOW },
      { type: 'session_event', sid: 's1', event: { kind: 'progress', content: 'looking', done: false }, now: NOW },
      { type: 'session_event', sid: 's1', event: { kind: 'progress', content: 'looking at files', done: false }, now: NOW },
      { type: 'session_event', sid: 's1', event: { kind: 'activity', step: { itemId: 't1', kind: 'tool_call', name: 'Bash', summary: 'ls', status: 'completed' } }, now: NOW },
    ])
    const live = chatOf(state, 's1').live
    expect(live?.content).toBe('looking at files')
    expect(live?.steps).toHaveLength(1)
    expect(live?.steps[0]?.status).toBe('completed')
    state = reduce(state, { type: 'session_event', sid: 's1', event: { kind: 'activity', step: { itemId: 't2', kind: 'command_exec', name: 'cargo', summary: 'cargo test', status: 'started' } }, now: NOW })
    state = reduce(state, { type: 'session_event', sid: 's1', event: { kind: 'progress', content: '', done: true }, now: NOW })
    expect(chatOf(state, 's1').live?.steps.every(s => s.status === 'completed')).toBe(true)
  })

  it('an answer settles the live turn into an ephemeral row carrying its steps', () => {
    let state = run([
      { type: 'session_event', sid: 's1', event: { kind: 'activity', step: { itemId: 't1', kind: 'tool_call', name: 'Bash', summary: 'ls', status: 'started' } }, now: NOW },
      { type: 'session_event', sid: 's1', event: { kind: 'answer', id: 'e9', content: 'done.' }, now: NOW },
    ])
    const chat = chatOf(state, 's1')
    expect(chat.live).toBeNull()
    expect(chat.activity).toBe('idle')
    const settled = chat.rows[0]
    expect(settled?.kind).toBe('assistant')
    if (settled?.kind === 'assistant') {
      expect(settled.ephemeral).toBe(true)
      expect(settled.content).toBe('done.')
      expect(settled.steps).toHaveLength(1)
      expect(settled.steps[0]?.status).toBe('completed')
    }
    // The canonical page then absorbs the ephemeral row: same text, steps kept.
    state = reduce(state, { type: 'history_loaded', sid: 's1', rows: [{ turnId: 'u1:user', role: 'user', content: 'go' }, { turnId: 'u1:assistant', role: 'assistant', content: 'done.' }], hasMore: false })
    const rows = chatOf(state, 's1').rows
    expect(rows.map(r => r.kind)).toEqual(['user', 'assistant'])
    const canon = rows[1]
    if (canon?.kind === 'assistant') {
      expect(canon.ephemeral).toBeUndefined()
      expect(canon.steps).toHaveLength(1)
    }
  })

  it('steps attached to a canonical row survive later history reloads', () => {
    let state = run([
      { type: 'session_event', sid: 's1', event: { kind: 'activity', step: { itemId: 't1', kind: 'tool_call', name: 'Bash', summary: 'ls', status: 'started' } }, now: NOW },
      { type: 'session_event', sid: 's1', event: { kind: 'answer', id: 'e9', content: 'done.' }, now: NOW },
      { type: 'history_loaded', sid: 's1', rows: [{ turnId: 'u1:assistant', role: 'assistant', content: 'done.' }], hasMore: false },
      { type: 'history_loaded', sid: 's1', rows: [{ turnId: 'u1:assistant', role: 'assistant', content: 'done.' }, { turnId: 'u2:user', role: 'user', content: 'more' }], hasMore: false },
    ])
    const first = chatOf(state, 's1').rows[0]
    expect(first?.kind).toBe('assistant')
    if (first?.kind === 'assistant') expect(first.steps.map(s => s.itemId)).toEqual(['t1'])
  })

  it('an answer with options becomes a choice row that waits, and resolving it resumes work', () => {
    let state = run([
      { type: 'session_event', sid: 's1', event: { kind: 'answer', id: 'p1', content: 'Allow?', options: [{ id: 'y', label: 'Yes' }, { id: 'n', label: 'No' }], token: 'tok' }, now: NOW },
    ])
    let chat = chatOf(state, 's1')
    expect(chat.waiting).toBe(true)
    expect(chat.rows[0]?.kind).toBe('choice')
    state = reduce(state, { type: 'choice_resolving', sid: 's1', id: 'choice-p1' })
    state = reduce(state, { type: 'choice_resolved', sid: 's1', id: 'choice-p1', selection: 'y' })
    chat = chatOf(state, 's1')
    expect(chat.waiting).toBe(false)
    expect(chat.activity).toBe('working')
    const row = chat.rows[0]
    if (row?.kind === 'choice') expect(row.resolved).toBe('y')
    // A resolved choice does not survive the next canonical page.
    state = reduce(state, { type: 'history_loaded', sid: 's1', rows: [], hasMore: false })
    expect(chatOf(state, 's1').rows).toEqual([])
  })

  it('a duplicate answer id is not appended twice', () => {
    const state = run([
      { type: 'session_event', sid: 's1', event: { kind: 'answer', id: 'e1', content: 'a' }, now: NOW },
      { type: 'session_event', sid: 's1', event: { kind: 'answer', id: 'e1', content: 'a' }, now: NOW },
    ])
    expect(chatOf(state, 's1').rows).toHaveLength(1)
  })

  it('bookkeeping lifecycle states (renamed) add no row', () => {
    const state = run([
      { type: 'send_started', sid: 's1', text: 'x' },
      { type: 'session_event', sid: 's1', event: { kind: 'lifecycle', state: 'renamed', reason: 'user' }, now: NOW },
    ])
    expect(chatOf(state, 's1').rows.map(r => r.kind)).toEqual(['user'])
  })

  it('lifecycle end states drop the live turn and keep the transition on the row', () => {
    const state = run([
      { type: 'session_event', sid: 's1', event: { kind: 'progress', content: 'x', done: false }, now: NOW },
      { type: 'session_event', sid: 's1', event: { kind: 'lifecycle', state: 'evicted', reason: 'idle' }, now: NOW },
    ])
    const chat = chatOf(state, 's1')
    expect(chat.live).toBeNull()
    expect(chat.activity).toBe('idle')
    const last = chat.rows.at(-1)
    expect(last?.kind).toBe('system')
    expect(last?.kind === 'system' ? last.lifecycle : undefined).toEqual({ state: 'evicted', reason: 'idle' })
  })

  it('lifecycle copy distinguishes idle release from a capacity eviction and a user stop', () => {
    expect(lifecycleCopyKey('evicted', 'idle')).toBe('chat.lifecycle.released')
    expect(lifecycleCopyKey('evicted', 'capacity')).toBe('chat.lifecycle.evicted')
    expect(lifecycleCopyKey('evicted', undefined)).toBe('chat.lifecycle.evicted')
    expect(lifecycleCopyKey('stopped', undefined)).toBe('chat.lifecycle.stopped')
    expect(lifecycleCopyKey('resumed', undefined)).toBe('chat.lifecycle.resumed')
    expect(lifecycleCopyKey('crashed', undefined)).toBeNull()
  })
})

describe('chat: sending and history', () => {
  it('an optimistic user row is replaced by its canonical row', () => {
    let state = run([{ type: 'send_started', sid: 's1', text: 'hello' }])
    expect(chatOf(state, 's1').rows[0]).toMatchObject({ kind: 'user', content: 'hello', local: true })
    state = reduce(state, { type: 'history_loaded', sid: 's1', rows: [{ turnId: 'u1:user', role: 'user', content: 'hello' }], hasMore: false })
    const rows = chatOf(state, 's1').rows
    expect(rows).toHaveLength(1)
    expect(rows[0]).toMatchObject({ kind: 'user', id: 'u1:user' })
  })

  it('queued and failed receipts become notices; an answer clears queued ones', () => {
    let state = run([
      { type: 'send_started', sid: 's1', text: 'a' },
      { type: 'send_settled', sid: 's1', receipt: { ok: true, queued: true, queuedBehind: 's0' } },
    ])
    expect(chatOf(state, 's1').notices).toEqual([{ id: 1, kind: 'queued', queuedBehind: 's0' }])
    state = reduce(state, { type: 'send_settled', sid: 's1', receipt: { ok: false, errorKind: 'bad_request', error: 'nope' } })
    expect(chatOf(state, 's1').notices).toHaveLength(2)
    state = reduce(state, { type: 'session_event', sid: 's1', event: { kind: 'answer', id: 'e1', content: 'ok' }, now: NOW })
    expect(chatOf(state, 's1').notices.map(n => n.kind)).toEqual(['error'])
    expect(reduce(state, { type: 'send_settled', sid: 's1', receipt: { ok: true } })).toBe(state)
  })

  it('older pages prepend without duplicating rows and keep the cursor', () => {
    let state = run([
      { type: 'history_loaded', sid: 's1', rows: [{ turnId: 'u2:user', role: 'user', content: 'second' }], hasMore: true, nextBefore: 'c1' },
    ])
    state = reduce(state, { type: 'history_loading', sid: 's1', older: true })
    expect(chatOf(state, 's1').loadingOlder).toBe(true)
    state = reduce(state, { type: 'history_loaded', sid: 's1', rows: [{ turnId: 'u1:user', role: 'user', content: 'first' }, { turnId: 'u2:user', role: 'user', content: 'second' }], hasMore: false, older: true })
    const chat = chatOf(state, 's1')
    expect(chat.rows.map(r => r.id)).toEqual(['u1:user', 'u2:user'])
    expect(chat.hasMore).toBe(false)
    expect(chat.loadingOlder).toBe(false)
  })

  it('a delegation frame narrates into an open parent chat only', () => {
    let state = run([{ type: 'delegation', relation: 'spawned', parentSid: 's1', childSid: 's2', title: 'child' }])
    expect(state.chats['s1']).toBeUndefined()
    state = run([{ type: 'send_started', sid: 's1', text: 'x' }, { type: 'delegation', relation: 'done', parentSid: 's1', childSid: 's2', title: 'child' }])
    expect(chatOf(state, 's1').notices[0]).toMatchObject({ kind: 'info', message: 'done:child' })
    expect(state.graphStale).toBe(true)
  })
})

describe('spawn draft', () => {
  it('a vendor switch clears model/effort and a project switch clears the role', () => {
    let state = run([{ type: 'set_draft', draft: { vendor: 'claude', model: 'm', effort: 'high', project: 'p', role: 'cto' } }])
    state = reduce(state, { type: 'set_draft', draft: { vendor: 'codex' } })
    expect(state.spawn.draft).toMatchObject({ vendor: 'codex', model: null, effort: null, role: 'cto' })
    state = reduce(state, { type: 'set_draft', draft: { project: 'q' } })
    expect(state.spawn.draft.role).toBeNull()
  })

  it('spawn outcomes: sid present wins even on failure', () => {
    expect(planSpawnOutcome({ ok: true, sid: 's1' })).toEqual({ kind: 'chat', sid: 's1' })
    expect(planSpawnOutcome({ ok: false, sid: 's1', error: 'task failed' })).toEqual({ kind: 'chat', sid: 's1', errorMessage: 'task failed' })
    expect(planSpawnOutcome({ ok: false, error: 'no project' })).toEqual({ kind: 'form_error', message: 'no project' })
  })

  it('efforts come from the model row, else the vendor ladder', () => {
    const catalog = { vendors: [{ vendor: 'claude', efforts: ['low', 'high'], models: [{ id: 'a', efforts: ['max'] }, { id: 'b', efforts: [] }] }] }
    expect(effortsFor(catalog, 'claude', 'a')).toEqual(['max'])
    expect(effortsFor(catalog, 'claude', 'b')).toEqual(['low', 'high'])
    expect(effortsFor(catalog, 'claude', null)).toEqual(['low', 'high'])
    expect(effortsFor(catalog, 'codex', null)).toEqual([])
    expect(effortsFor(null, 'claude', null)).toEqual([])
  })
})

describe('layout', () => {
  it('docks by default, clamps the width to the pane and to DSH\'s reserve, toggles full', () => {
    let state = initialState()
    expect(state.layout).toEqual({ mode: 'docked', dockWidth: 520 })
    state = reduce(state, { type: 'set_dock_width', width: 100 })
    expect(state.layout.dockWidth).toBe(360)
    state = reduce(state, { type: 'set_dock_width', width: 5000 })
    expect(state.layout.dockWidth).toBe(1600)
    state = reduce(state, { type: 'set_dock_width', width: 1000, viewport: 1200 })
    expect(state.layout.dockWidth).toBe(720)
    expect(reduce(state, { type: 'set_dock_width', width: 720, viewport: 1200 })).toBe(state)
    state = reduce(state, { type: 'toggle_mode' })
    expect(state.layout.mode).toBe('full')
    state = reduce(state, { type: 'set_mode', mode: 'docked' })
    expect(state.layout.mode).toBe('docked')
    expect(reduce(state, { type: 'set_mode', mode: 'docked' })).toBe(state)
    expect(initialState({ mode: 'full', dockWidth: 640 }).layout).toEqual({ mode: 'full', dockWidth: 640 })
  })
})

describe('model switch', () => {
  it('spells the /model directive exactly as a human would type it', () => {
    expect(modelDirective('opus', null)).toBe('/model opus')
    expect(modelDirective(' opus ', 'high')).toBe('/model opus high')
    expect(modelDirective('opus', '  ')).toBe('/model opus')
    expect(modelDirective('', 'high')).toBe('/model')
  })
})

describe('persistence', () => {
  function memoryStorage(seed: Record<string, string> = {}): StorageLike & { data: Record<string, string> } {
    const data = { ...seed }
    return {
      data,
      getItem: key => data[key] ?? null,
      setItem: (key, value) => {
        data[key] = value
      },
    }
  }

  it('mirrors open, recents, draft project/vendor and column prefs', () => {
    const storage = memoryStorage()
    const store = createStore(initialState())
    attachPersistence(store, storage)
    store.dispatch({ type: 'open_panel' })
    store.dispatch({ type: 'select_session', sid: 's1' })
    store.dispatch({ type: 'set_draft', draft: { project: 'p', vendor: 'grok' } })
    store.dispatch({ type: 'toggle_team' })
    store.dispatch({ type: 'set_dock_width', width: 600 })
    store.dispatch({ type: 'toggle_mode' })
    expect(storage.data[STORAGE_KEYS.mode]).toBe('full')
    expect(storage.data[STORAGE_KEYS.dockWidth]).toBe('600')
    expect(storage.data[STORAGE_KEYS.open]).toBe('1')
    expect(JSON.parse(storage.data[STORAGE_KEYS.recents]!)).toEqual(['s1'])
    expect(storage.data[STORAGE_KEYS.project]).toBe('p')
    expect(storage.data[STORAGE_KEYS.vendor]).toBe('grok')
    expect(storage.data[STORAGE_KEYS.team]).toBe('0')

    const loaded = initialState(loadPersisted(storage))
    expect(loaded.open).toBe(true)
    expect(loaded.recents).toEqual(['s1'])
    expect(loaded.spawn.draft.project).toBe('p')
    expect(loaded.spawn.draft.vendor).toBe('grok')
    expect(loaded.teamOpen).toBe(false)
    expect(loaded.layout).toEqual({ mode: 'full', dockWidth: 600 })
  })

  it('poisoned storage still boots with defaults', () => {
    const storage = memoryStorage({ [STORAGE_KEYS.recents]: '{not json', [STORAGE_KEYS.open]: '1' })
    expect(loadPersisted(storage)).toEqual({})
    expect(loadPersisted(undefined)).toEqual({})
  })
})

describe('tree utils', () => {
  const node = (sid: string, children: TeamNode[] = [], extra: Partial<TeamNode> = {}): TeamNode => ({
    sid,
    project: 'p',
    vendor: 'claude',
    activity: 'idle',
    children,
    ...extra,
  })
  const graph: TeamGraph = { projects: [{ slug: 'p', nodes: [node('s1', [node('s2', [node('s3')])], { title: 'root' }), node('s4', [], { vendor: 'codex', model: 'gpt-5' })] }] }

  it('flattens parents before children with depth', () => {
    expect(flattenNodes(graph.projects[0]!.nodes).map(r => [r.node.sid, r.depth])).toEqual([['s1', 0], ['s2', 1], ['s3', 2], ['s4', 0]])
  })

  it('filters by title/sid/vendor/model keeping ancestors', () => {
    const rows = flattenNodes(graph.projects[0]!.nodes)
    expect(filterRows(rows, 's3').map(r => r.node.sid)).toEqual(['s1', 's2', 's3'])
    expect(filterRows(rows, 'GPT').map(r => r.node.sid)).toEqual(['s4'])
    expect(filterRows(rows, '')).toBe(rows)
    expect(filterRows(rows, 'zzz')).toEqual([])
  })

  it('expand-only and collapse-all fold the whole project list', () => {
    const two: TeamGraph = { projects: [{ slug: 'a', nodes: [node('s1')] }, { slug: 'b', nodes: [node('s2')] }, { slug: 'c', nodes: [] }] }
    const loaded = run([{ type: 'graph_loaded', graph: two }])
    expect(reduce(loaded, { type: 'expand_only', slug: 'b' }).collapsed).toEqual({ a: true, b: false, c: true })
    expect(reduce(loaded, { type: 'collapse_all' }).collapsed).toEqual({ a: true, b: true, c: true })
    expect(reduce(initialState(), { type: 'collapse_all' }).collapsed).toEqual({})
  })

  it('expand-all reopens every project, the global fold\'s inverse of collapse-all', () => {
    const two: TeamGraph = { projects: [{ slug: 'a', nodes: [] }, { slug: 'b', nodes: [] }] }
    const loaded = run([{ type: 'graph_loaded', graph: two }, { type: 'collapse_all' }])
    expect(loaded.collapsed).toEqual({ a: true, b: true })
    expect(reduce(loaded, { type: 'expand_all' }).collapsed).toEqual({ a: false, b: false })
    expect(reduce(initialState(), { type: 'expand_all' }).collapsed).toEqual({})
  })

  it('finds nodes anywhere and maps activity to dot states', () => {
    expect(findNode(graph, 's3')?.sid).toBe('s3')
    expect(findNode(graph, 'nope')).toBeUndefined()
    expect(findNode(null, 's1')).toBeUndefined()
    expect(dotState('working')).toBe('ongoing')
    expect(dotState('stale')).toBe('warning')
    expect(dotState('stuck')).toBe('error')
    expect(dotState(undefined)).toBe('done')
  })
})

describe('format', () => {
  it('cost, tokens, elapsed, relative time, title, glyph', () => {
    expect(formatCost(undefined)).toBeNull()
    expect(formatCost(0.004)).toBeNull()
    expect(formatCost(0.123)).toBe('$0.12')
    expect(formatCost(12.34)).toBe('$12.3')
    expect(formatTokens(999)).toBe('999')
    expect(formatTokens(1234)).toBe('1.2k')
    expect(formatTokens(45_000)).toBe('45k')
    expect(formatTokens(2_500_000)).toBe('2.5M')
    expect(formatElapsed(12_000)).toBe('12s')
    expect(formatElapsed(65_000)).toBe('1m 05s')
    expect(formatElapsed(3_720_000)).toBe('1h 02m')
    expect(relativeTime(undefined, NOW)).toBeNull()
    expect(relativeTime(new Date(NOW - 10_000).toISOString(), NOW)).toEqual({ unit: 'now' })
    expect(relativeTime(new Date(NOW - 120_000).toISOString(), NOW)).toEqual({ unit: 'minutes', value: 2 })
    expect(relativeTime(new Date(NOW - 2 * 86_400_000).toISOString(), NOW)).toEqual({ unit: 'days', value: 2 })
    expect(titleFromTask('\n  Fix the login bug\nmore')).toBe('Fix the login bug')
    expect(titleFromTask('   ')).toBeUndefined()
    expect(titleFromTask('x'.repeat(80))).toHaveLength(60)
    expect(vendorGlyph('claude')).toBe('cl')
  })
})
