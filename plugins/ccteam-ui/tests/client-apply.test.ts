/**
 * The plugin face, driven through the real `apply` with a recording ctx:
 * dictionaries land once, each seat is contributed through `slots.inject`
 * (the framework waits for the declaring package's slot and owns the
 * registration's lifetime), the registrations carry the shapes the slot core
 * requires (list id / keyed key / locale namespace / inject face), and the
 * injected faces expose bare observable sources under `hooks`.
 */
import { describe, expect, it, vi } from 'vitest'

// The primitives package ships built lib/*.js importing raw .css — node
// cannot load that from node_modules, and these tests exercise wiring, not
// pixels. Stub every member the components pull.
vi.mock('@deepseek-ai/dsh-client-ui-primitives', () => {
  const stub = (): null => null
  return {
    Button: stub,
    DisclosureRow: stub,
    Input: stub,
    Pill: stub,
    StateDot: stub,
    Tooltip: stub,
    writeClipboard: stub,
    IconBranchOutline16: stub,
    IconCheckOutline14: stub,
    IconChevronDownOutline14: stub,
    IconChevronLeftOutline14: stub,
    IconCloseOutline16: stub,
    IconCopyOutline16: stub,
    IconPlusOutline16: stub,
    IconSendOutline16: stub,
    IconSettingsOutline14: stub,
    IconTreeCorner8x10: stub,
    IconWarningOutline16: stub,
  }
})

const { apply, inject, name } = await import('../src/client/index.js')
const { EntryButton } = await import('../src/client/EntryButton.js')
const { Workbench } = await import('../src/client/Workbench.js')
const { SettingsCard } = await import('../src/client/settings/SettingsCard.js')

interface Registration {
  options: Record<string, unknown>
  component: unknown
  disposed: boolean
}

interface Wait {
  key: string
  callback: () => (() => void) | Iterable<() => void>
}

function fakeScope(namespace: string) {
  return {
    namespace,
    getSnapshot: () => ({
      status: 'ready' as const,
      value: {},
      base: undefined,
      user: undefined,
      revision: 1,
      writable: true,
      mode: 'host' as const,
    }),
    subscribe: () => () => {},
    set: () => Promise.resolve(),
    unset: () => Promise.resolve(),
  }
}

/** A recording client context: every service the plugin injects, no React. */
function recordingCtx() {
  const effects: Array<{ label: string | undefined; dispose: (() => void) | void }> = []
  const dictionaries: Array<{ ns: string; locales: string[] }> = []
  const waits: Wait[] = []
  const registrations: Registration[] = []
  const bound: string[] = []
  const ctx = {
    effect(setup: () => (() => void) | void, label?: string) {
      const dispose = setup()
      effects.push({ label, dispose })
      return () => {
        if (typeof dispose === 'function') dispose()
      }
    },
    locale: {
      register(ns: string, dicts: Record<string, Record<string, string>>) {
        dictionaries.push({ ns, locales: Object.keys(dicts).sort() })
        return () => {}
      },
      bind: () => (key: string) => key,
    },
    slots: {
      inject(key: string, callback: Wait['callback']) {
        waits.push({ key, callback })
        return () => {}
      },
      register(options: Record<string, unknown>, component: unknown) {
        const registration: Registration = { options, component, disposed: false }
        registrations.push(registration)
        return () => {
          registration.disposed = true
        }
      },
    },
    settingsScope: {
      bind(spec: { namespace: string }) {
        bound.push(spec.namespace)
        return fakeScope(spec.namespace)
      },
    },
  }
  return { ctx, effects, dictionaries, waits, registrations, bound }
}

/** Run every pending inject callback as the framework would once the slot is declared. */
function declareAll(waits: Wait[]): void {
  for (const wait of waits) {
    const effect = wait.callback()
    if (typeof effect === 'function') continue
    for (const _dispose of effect) {
      // Iterating installs each registration (generator effects are lazy).
    }
  }
}

describe('plugin face', () => {
  it('is a DSH client plugin: name, required services, apply', () => {
    expect(name).toBe('ccteam-ui')
    expect(inject).toEqual(['slots', 'locale', 'settingsScope'])
    expect(typeof apply).toBe('function')
  })
})

describe('apply', () => {
  it('registers the ccteam dictionaries once, both locales, through the locale service', () => {
    const { ctx, dictionaries, effects } = recordingCtx()
    apply(ctx as never)
    expect(dictionaries).toEqual([{ ns: 'ccteam', locales: ['en', 'zh'] }])
    expect(effects.map(effect => effect.label)).toEqual([
      'ccteam-ui: dictionaries',
      'ccteam-ui: persistence',
      'ccteam-ui: team event stream',
      'ccteam-ui: engine poller',
    ])
  })

  it('contributes every seat through slots.inject, never a direct register at apply time', () => {
    const { ctx, waits, registrations } = recordingCtx()
    apply(ctx as never)
    expect(waits.map(wait => wait.key)).toEqual(['sidebar.footer.action', 'shell.overlay', 'settings.plugin.item'])
    // Nothing is registered until the framework reports the slot declared.
    expect(registrations).toEqual([])
  })

  it('registers the entry, the panel, and the one ccteam card once the slots are declared', () => {
    const { ctx, waits, registrations, bound } = recordingCtx()
    apply(ctx as never)
    declareAll(waits)

    const byName = (slot: string) => registrations.filter(r => r.options.name === slot)
    const [entry] = byName('sidebar.footer.action')
    expect(entry?.component).toBe(EntryButton)
    expect(entry?.options).toMatchObject({ id: 'ccteam', order: 0, locale: 'ccteam' })

    const [panel] = byName('shell.overlay')
    expect(panel?.component).toBe(Workbench)
    expect(panel?.options).toMatchObject({ id: 'ccteam', order: 0, locale: 'ccteam' })

    const cards = byName('settings.plugin.item')
    expect(cards.map(card => card.options.key)).toEqual(['ccteam-ui'])
    for (const card of cards) {
      expect(card.component).toBe(SettingsCard)
      expect(card.options.locale).toBe('ccteam')
    }
    expect(bound).toEqual(['ccteam-ui'])
    expect(registrations).toHaveLength(3)
  })

  it('injects the console face: one observable store under hooks, dispatch, and the BFF client', () => {
    const { ctx, waits, registrations } = recordingCtx()
    apply(ctx as never)
    declareAll(waits)
    const entry = registrations.find(r => r.options.name === 'sidebar.footer.action')!
    const panel = registrations.find(r => r.options.name === 'shell.overlay')!
    const faceOf = (registration: Registration) =>
      (registration.options.inject as () => { hooks: { console: { getSnapshot(): unknown; subscribe(fn: () => void): () => void } }; dispatch: unknown; api: unknown })()
    const entryFace = faceOf(entry)
    const panelFace = faceOf(panel)
    // Both seats share the ONE store: the badge the entry shows is the panel's state.
    expect(entryFace.hooks.console).toBe(panelFace.hooks.console)
    expect(typeof entryFace.hooks.console.getSnapshot).toBe('function')
    expect(typeof entryFace.hooks.console.subscribe).toBe('function')
    expect(typeof entryFace.dispatch).toBe('function')
    expect(typeof (entryFace.api as { call: unknown }).call).toBe('function')
    const snapshot = entryFace.hooks.console.getSnapshot() as { open: boolean; selection: unknown }
    expect(snapshot.open).toBe(false)
    expect(snapshot.selection).toEqual({ kind: 'none' })
  })

  it('injects each card face with its namespace spec and a bare observable under hooks.card', () => {
    const { ctx, waits, registrations } = recordingCtx()
    apply(ctx as never)
    declareAll(waits)
    for (const card of registrations.filter(r => r.options.name === 'settings.plugin.item')) {
      const face = (card.options.inject as () => {
        hooks: { card: { getSnapshot(): { available: boolean; fields: Record<string, unknown> }; subscribe: unknown } }
        spec: { namespace: string; fields: Array<{ field: string; kind: string }> }
        edit: unknown
        save: unknown
        discard: unknown
        reset: unknown
      })()
      expect(face.spec.namespace).toBe(card.options.key)
      expect(typeof face.hooks.card.subscribe).toBe('function')
      const state = face.hooks.card.getSnapshot()
      expect(state.available).toBe(true)
      // Toggles are live controls, projected apart from the staged fields.
      expect(Object.keys(state.fields).sort()).toEqual(face.spec.fields.filter(f => f.kind !== 'toggle').map(f => f.field).sort())
      for (const action of [face.edit, face.save, face.discard, face.reset]) expect(typeof action).toBe('function')
    }
  })

  it('disposes registrations through the disposer each inject callback returns', () => {
    const { ctx, waits, registrations } = recordingCtx()
    apply(ctx as never)
    const disposers: Array<() => void> = []
    for (const wait of waits) {
      const effect = wait.callback()
      if (typeof effect === 'function') disposers.push(effect)
      else disposers.push(...effect)
    }
    expect(registrations.every(r => !r.disposed)).toBe(true)
    for (const dispose of disposers) dispose()
    expect(registrations.every(r => r.disposed)).toBe(true)
  })
})
