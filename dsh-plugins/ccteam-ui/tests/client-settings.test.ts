/**
 * The settings card form over a fake SettingsScope: projection (available /
 * writable / overridden / configured), staging, save = set/unset per field
 * with the outcome read back from the scope, failure keeps drafts, discard
 * and reset. Zero React, zero wire.
 */
import { describe, expect, it } from 'vitest'
import { CCTEAM_CARD, SettingsCardController } from '../src/client/settings/form.js'
import type { Section } from '../src/client/settings/form.js'

interface ScopeState {
  status: 'loading' | 'ready' | 'unavailable'
  value: Section | undefined
  user: Section | undefined
  writable: boolean
}

/** A SettingsScope double whose Host answers are scripted per test. */
function fakeScope(initial: Partial<ScopeState> = {}) {
  let state: ScopeState = { status: 'ready', value: {}, user: undefined, writable: true, ...initial }
  let revision = 1
  const listeners = new Set<() => void>()
  const writes: Array<{ op: 'set' | 'unset'; field: string; value?: unknown }> = []
  /** How the Host treats a write: `accept` folds it into value+user, `reject` leaves state alone, `throw` rejects. */
  let behaviour: 'accept' | 'reject' | 'throw' = 'accept'
  const notify = () => {
    for (const listener of [...listeners]) listener()
  }
  const secrets = new Map<string, boolean>()
  const redact = () => {
    for (const field of ['restToken', 'enrollment']) {
      if (Object.hasOwn(state.value ?? {}, field)) secrets.set(field, Boolean(state.value?.[field]))
    }
    state = { ...state, value: { ...state.value }, user: { ...state.user } }
    for (const field of ['restToken', 'enrollment']) { delete state.value?.[field]; delete state.user?.[field] }
  }
  redact()
  const scope = {
    getSnapshot: () => ({ ...state, base: undefined, revision }),
    subscribe(listener: () => void) { listeners.add(listener); return () => { listeners.delete(listener) } },
    async mutate(ops: readonly ({ op: 'set'; path: readonly string[]; value: unknown } | { op: 'unset'; path: readonly string[] })[], expectedRevision?: number) {
      for (const op of ops) writes.push({ op: op.op, field: op.path[0]!, ...op.op === 'set' ? { value: op.value } : {} })
      if (behaviour === 'throw') throw new Error('wire down')
      if (behaviour === 'reject' || (expectedRevision !== undefined && expectedRevision !== revision)) return false
      for (const op of ops) {
        const field = op.path[0]!
        if (op.op === 'set') state = { ...state, value: { ...state.value, [field]: op.value }, user: { ...state.user, [field]: op.value } }
        else { delete state.value?.[field]; delete state.user?.[field]; secrets.delete(field) }
      }
      revision++
      redact()
      notify()
      return true
    },
  }
  const mirror = {
    getSnapshot: () => ({ status: 'ready' as const, error: null, view: {
      namespaces: [{ ns: 'ccteam-ui', autoGenerate: false, schema: {}, value: {}, revision: 1, applies: 'live' as const,
        secrets: [...secrets].map(([field, set]) => ({ path: [field], set })) }],
      writable: true, hasDocument: true,
    } }),
    subscribe: (_listener: () => void) => () => {},
  }
  return {
    mirror,
    scope,
    writes,
    behave(next: typeof behaviour) {
      behaviour = next
    },
    host(next: Partial<ScopeState>) {
      state = { ...state, ...next }
      revision++
      redact()
      notify()
    },
  }
}

describe('revision fences', () => {
  it('saves staged text after its own live toggle advances the form revision', async () => {
    const { scope, mirror } = fakeScope()
    const card = new SettingsCardController(scope, CCTEAM_CARD, mirror)
    card.inject().edit('enginePath', '/opt/ccteam')
    await card.setToggle('autoStart', false)
    await card.save()
    expect(card.getSnapshot().failed).toBe(false)
    expect(scope.getSnapshot().value?.enginePath).toBe('/opt/ccteam')
  })

  it('keeps the stale fence when another editor changed the form before the toggle', async () => {
    const { scope, mirror, host } = fakeScope()
    const card = new SettingsCardController(scope, CCTEAM_CARD, mirror)
    card.inject().edit('defaultProject', 'my-project')
    host({ value: { defaultProject: 'external-project' } })
    await card.setToggle('autoStart', false)
    await card.save()
    expect(card.getSnapshot().failed).toBe(true)
    expect(scope.getSnapshot().value?.defaultProject).toBe('external-project')
  })
})

describe('redacted native configuration', () => {
  it('accepts a secret write without receiving the secret in the Host response', async () => {
    const { scope, mirror } = fakeScope()
    const card = new SettingsCardController(scope, CCTEAM_CARD, mirror)
    card.inject().edit('restToken', 'private-token')
    await card.save()
    expect(scope.getSnapshot().value).not.toHaveProperty('restToken')
    expect(card.getSnapshot().fields.restToken).toEqual({ text: '', overridden: false, configured: true })
    expect(card.getSnapshot().failed).toBe(false)
    expect(card.getSnapshot().dirty).toBe(false)
  })
})

describe('projection', () => {
  it('is unavailable (renders nothing) until the Host serves the namespace', () => {
    const { mirror, scope, host } = fakeScope({ status: 'loading' })
    const card = new SettingsCardController(scope, CCTEAM_CARD, mirror)
    expect(card.getSnapshot().available).toBe(false)
    host({ status: 'ready' })
    expect(card.getSnapshot().available).toBe(true)
  })

  it('shows the effective value, marks user-layer fields overridden, and never echoes a secret', () => {
    const { mirror, scope } = fakeScope({
      value: { daemonUrl: 'http://127.0.0.1:7331', restToken: 'ccteam:abc', defaultProject: 'demo' },
      user: { defaultProject: 'demo' },
    })
    const card = new SettingsCardController(scope, CCTEAM_CARD, mirror)
    const { fields } = card.getSnapshot()
    expect(fields.daemonUrl).toEqual({ text: 'http://127.0.0.1:7331', overridden: false, configured: true })
    expect(fields.defaultProject).toEqual({ text: 'demo', overridden: true, configured: true })
    // The token literal never reaches the control; only its presence does.
    expect(fields.restToken).toEqual({ text: '', overridden: false, configured: true })
  })

  it('reports an unset secret as not configured', () => {
    const { mirror, scope } = fakeScope({ value: { daemonUrl: 'http://127.0.0.1:7331' } })
    const card = new SettingsCardController(scope, CCTEAM_CARD, mirror)
    expect(card.getSnapshot().fields.enrollment?.configured).toBe(false)
  })

  it('publishes a new snapshot (new reference) whenever the scope changes, and keeps the reference otherwise', () => {
    const { mirror, scope, host } = fakeScope()
    const card = new SettingsCardController(scope, CCTEAM_CARD, mirror)
    const first = card.getSnapshot()
    expect(card.getSnapshot()).toBe(first)
    let notified = 0
    card.subscribe(() => {
      notified += 1
    })
    host({ writable: false })
    expect(notified).toBe(1)
    expect(card.getSnapshot()).not.toBe(first)
    expect(card.getSnapshot().writable).toBe(false)
  })
})

describe('staging', () => {
  it('edit stages a draft: dirty, overridden preview, no write yet', () => {
    const { mirror, scope, writes } = fakeScope()
    const card = new SettingsCardController(scope, CCTEAM_CARD, mirror)
    const face = card.inject()
    face.edit('daemonUrl', 'http://10.0.0.1:7331')
    expect(card.getSnapshot().dirty).toBe(true)
    expect(card.getSnapshot().fields.daemonUrl).toMatchObject({ text: 'http://10.0.0.1:7331', overridden: true })
    expect(writes).toEqual([])
  })

  it('discard drops every draft; reset stages a clear', () => {
    const { mirror, scope } = fakeScope({ value: { daemonUrl: 'x' }, user: { daemonUrl: 'x' } })
    const card = new SettingsCardController(scope, CCTEAM_CARD, mirror)
    const face = card.inject()
    face.edit('daemonUrl', 'y')
    face.discard()
    expect(card.getSnapshot().dirty).toBe(false)
    expect(card.getSnapshot().fields.daemonUrl?.text).toBe('x')
    face.reset('daemonUrl')
    expect(card.getSnapshot().dirty).toBe(true)
    expect(card.getSnapshot().fields.daemonUrl).toMatchObject({ text: '', overridden: false })
  })
})

describe('save', () => {
  it('writes set for text, unset for blank, skips a blank secret, then reads the outcome back', async () => {
    const { mirror, scope, writes } = fakeScope({ value: { daemonUrl: 'old', defaultProject: 'p' }, user: { defaultProject: 'p' } })
    const card = new SettingsCardController(scope, CCTEAM_CARD, mirror)
    const face = card.inject()
    face.edit('daemonUrl', ' http://new:7331 ')
    face.edit('defaultProject', '')
    face.edit('restToken', '')
    await card.save()
    expect(writes).toEqual([
      { op: 'set', field: 'daemonUrl', value: 'http://new:7331' },
      { op: 'unset', field: 'defaultProject' },
    ])
    const state = card.getSnapshot()
    expect(state.failed).toBe(false)
    expect(state.dirty).toBe(false)
    expect(state.saving).toBe(false)
    expect(state.fields.daemonUrl).toMatchObject({ text: 'http://new:7331', overridden: true })
    expect(state.fields.defaultProject).toMatchObject({ text: '', overridden: false })
  })

  it('writes a secret and reports it configured afterwards', async () => {
    const { mirror, scope, writes } = fakeScope()
    const card = new SettingsCardController(scope, CCTEAM_CARD, mirror)
    const face = card.inject()
    face.edit('enrollment', 'ccteam-enroll:id:secret')
    await card.save()
    expect(writes).toEqual([{ op: 'set', field: 'enrollment', value: 'ccteam-enroll:id:secret' }])
    expect(card.getSnapshot().fields.enrollment).toEqual({ text: '', overridden: false, configured: true })
  })

  it('keeps the drafts and flags failure when the Host did not take the value', async () => {
    const { mirror, scope, behave } = fakeScope({ value: { daemonUrl: 'old' } })
    behave('reject')
    const card = new SettingsCardController(scope, CCTEAM_CARD, mirror)
    const face = card.inject()
    face.edit('daemonUrl', 'new')
    await card.save()
    expect(card.getSnapshot()).toMatchObject({ failed: true, dirty: true, saving: false })
    expect(card.getSnapshot().fields.daemonUrl?.text).toBe('new')
    // The next edit clears the failure flag.
    face.edit('daemonUrl', 'newer')
    expect(card.getSnapshot().failed).toBe(false)
  })

  it('treats a rejected write as a failed save rather than throwing', async () => {
    const { mirror, scope, behave } = fakeScope()
    behave('throw')
    const card = new SettingsCardController(scope, CCTEAM_CARD, mirror)
    card.inject().edit('daemonUrl', 'new')
    await expect(card.save()).resolves.toBeUndefined()
    expect(card.getSnapshot().failed).toBe(true)
  })

  it('is a no-op with nothing staged and while a save is in flight', async () => {
    const { mirror, scope, writes } = fakeScope()
    const card = new SettingsCardController(scope, CCTEAM_CARD, mirror)
    await card.save()
    expect(writes).toEqual([])
    card.inject().edit('daemonUrl', 'a')
    const first = card.save()
    expect(card.getSnapshot().saving).toBe(true)
    await card.save()
    await first
    expect(writes).toHaveLength(1)
  })
})

describe('toggles', () => {
  it('projects a live toggle from the effective value, defaulting per spec, apart from the staged fields', () => {
    const { mirror, scope, host } = fakeScope()
    const card = new SettingsCardController(scope, CCTEAM_CARD, mirror)
    expect(card.getSnapshot().toggles).toEqual({ autoStart: true })
    expect(card.getSnapshot().fields.autoStart).toBeUndefined()
    host({ value: { autoStart: false } })
    expect(card.getSnapshot().toggles.autoStart).toBe(false)
  })

  it('setToggle writes at once (no staging) and reads the outcome back', async () => {
    const { mirror, scope, writes } = fakeScope()
    const card = new SettingsCardController(scope, CCTEAM_CARD, mirror)
    await card.setToggle('autoStart', false)
    expect(writes).toEqual([{ op: 'set', field: 'autoStart', value: false }])
    expect(card.getSnapshot()).toMatchObject({ failed: false, saving: false, dirty: false })
    expect(card.getSnapshot().toggles.autoStart).toBe(false)
  })

  it('setToggle reports failure when the Host keeps the old value', async () => {
    const { mirror, scope, behave } = fakeScope()
    behave('reject')
    const card = new SettingsCardController(scope, CCTEAM_CARD, mirror)
    await card.setToggle('autoStart', false)
    expect(card.getSnapshot().failed).toBe(true)
    expect(card.getSnapshot().toggles.autoStart).toBe(true)
  })
})
