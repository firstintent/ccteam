/**
 * Version gate: slot mode when both declarations land, whole-mount fallback
 * when either slot is absent or rejects, no error ever escaping. Plus the
 * registration shape driven through the real `apply` with a recording ctx.
 */
import { describe, expect, it, vi } from 'vitest'
import { mountVersionGated } from '../src/client/mount.js'
import type { GateTarget } from '../src/client/mount.js'
import type { CcteamClientContext } from '../src/client/slots.js'

// The primitives package ships built lib/*.js importing raw .css — node
// cannot load that from node_modules, and these tests exercise wiring, not
// pixels. Stub every member my components pull.
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
    IconSendOutline14: stub,
    IconSettingsOutline14: stub,
    IconTreeCorner8x10: stub,
    IconWarningOutline16: stub,
  }
})

const { apply } = await import('../src/client/index.js')

/** Manual deadline scheduler. */
function manualSchedule() {
  const pending: Array<() => void> = []
  return {
    schedule: (callback: () => void, _ms: number) => {
      pending.push(callback)
      return () => {
        const index = pending.indexOf(callback)
        if (index >= 0) pending.splice(index, 1)
      }
    },
    fire() {
      for (const callback of pending.splice(0)) callback()
    },
    pendingCount: () => pending.length,
  }
}

/** A fake slots.inject board: declarations flip per key. */
function fakeSlots(declared: Record<string, boolean>) {
  const waiting = new Map<string, () => () => void>()
  const activeDisposers = new Map<string, () => void>()
  return {
    slots: {
      inject: (key: string, callback: () => () => void) => {
        if (declared[key] === true) {
          activeDisposers.set(key, callback())
        } else {
          waiting.set(key, callback)
        }
        return () => {
          waiting.delete(key)
          activeDisposers.get(key)?.()
          activeDisposers.delete(key)
        }
      },
    },
    declareLater(key: string) {
      const callback = waiting.get(key)
      if (callback !== undefined) activeDisposers.set(key, callback())
    },
  }
}

function target(key: string, behavior: { throws?: string; log: string[] }): GateTarget {
  return {
    key,
    register() {
      if (behavior.throws !== undefined) throw new Error(behavior.throws)
      behavior.log.push(`register:${key}`)
      return () => {
        behavior.log.push(`dispose:${key}`)
      }
    },
  }
}

describe('mountVersionGated', () => {
  it('lands in slot mode when both declarations exist, without touching the fallback', () => {
    const log: string[] = []
    const warns: string[] = []
    const timer = manualSchedule()
    const { slots } = fakeSlots({ a: true, b: true })
    const gate = mountVersionGated(slots, {
      targets: [target('a', { log }), target('b', { log })],
      mountFallback: () => {
        log.push('fallback')
        return () => {}
      },
      warn: message => warns.push(message),
      schedule: timer.schedule,
    })
    expect(gate.mode()).toBe('slots')
    expect(log).toEqual(['register:a', 'register:b'])
    expect(warns).toEqual([])
    // Settling cancels the deadline.
    expect(timer.pendingCount()).toBe(0)
    gate.dispose()
    expect(log).toEqual(['register:a', 'register:b', 'dispose:a', 'dispose:b'])
  })

  it('waits for a late declaration before settling', () => {
    const log: string[] = []
    const timer = manualSchedule()
    const board = fakeSlots({ a: true, b: false })
    const gate = mountVersionGated(board.slots, {
      targets: [target('a', { log }), target('b', { log })],
      mountFallback: () => () => {},
      warn: () => {},
      schedule: timer.schedule,
    })
    expect(gate.mode()).toBe('deciding')
    board.declareLater('b')
    expect(gate.mode()).toBe('slots')
    gate.dispose()
  })

  it('a throwing registration degrades the WHOLE mount: the sibling registration is disposed and the fallback mounts once', () => {
    const log: string[] = []
    const warns: string[] = []
    const timer = manualSchedule()
    const { slots } = fakeSlots({ a: true, b: true })
    const gate = mountVersionGated(slots, {
      targets: [target('a', { log }), target('b', { throws: 'no such slot', log })],
      mountFallback: () => {
        log.push('fallback')
        return () => {
          log.push('fallback:dispose')
        }
      },
      warn: message => warns.push(message),
      schedule: timer.schedule,
    })
    expect(gate.mode()).toBe('fallback')
    expect(log).toEqual(['register:a', 'dispose:a', 'fallback'])
    expect(warns.length).toBe(1)
    expect(warns[0]).toContain('no such slot')
    gate.dispose()
    expect(log[log.length - 1]).toBe('fallback:dispose')
  })

  it('a never-declared slot falls back at the deadline, and a later declaration cannot double-mount', () => {
    const log: string[] = []
    const warns: string[] = []
    const timer = manualSchedule()
    const board = fakeSlots({ a: true, b: false })
    const gate = mountVersionGated(board.slots, {
      targets: [target('a', { log }), target('b', { log })],
      mountFallback: () => {
        log.push('fallback')
        return () => {}
      },
      warn: message => warns.push(message),
      schedule: timer.schedule,
    })
    expect(gate.mode()).toBe('deciding')
    timer.fire()
    expect(gate.mode()).toBe('fallback')
    expect(log).toEqual(['register:a', 'dispose:a', 'fallback'])
    expect(warns[0]).toContain('"b"')
    // The watcher was disposed with the slot side: a late declaration is inert.
    board.declareLater('b')
    expect(log).toEqual(['register:a', 'dispose:a', 'fallback'])
    gate.dispose()
  })

  it('a runtime without slots.inject probes directly and falls back on the throw — nothing escapes', () => {
    const log: string[] = []
    const warns: string[] = []
    const gate = mountVersionGated({}, {
      targets: [target('a', { throws: 'slot "a" is not declared', log })],
      mountFallback: () => {
        log.push('fallback')
        return () => {}
      },
      warn: message => warns.push(message),
      schedule: manualSchedule().schedule,
    })
    expect(gate.mode()).toBe('fallback')
    expect(log).toEqual(['fallback'])
    expect(warns[0]).toContain('not declared')
    gate.dispose()
  })

  it('contains a throwing fallback mount too', () => {
    const warns: string[] = []
    const gate = mountVersionGated({}, {
      targets: [target('a', { throws: 'nope', log: [] })],
      mountFallback: () => {
        throw new Error('portal exploded')
      },
      warn: message => warns.push(message),
      schedule: manualSchedule().schedule,
    })
    expect(gate.mode()).toBe('fallback')
    expect(warns.some(message => message.includes('portal exploded'))).toBe(true)
    gate.dispose()
  })
})

/** Recording fake of the ctx face `apply` consumes. */
function recordingCtx(options: { registerThrows?: boolean } = {}) {
  const registrations: Array<{ options: Record<string, unknown>; component: unknown }> = []
  const dictionaries: Array<{ ns: string; locale: string }> = []
  const injectedKeys: string[] = []
  const warns: string[] = []
  const ctx = {
    effect(callback: () => unknown, _label?: string) {
      const disposer = callback()
      return () => {
        if (typeof disposer === 'function') disposer()
      }
    },
    logger: {
      warn(message: string) {
        warns.push(message)
      },
    },
    locale: {
      register(ns: string, locale: string, _dict: Record<string, string>) {
        dictionaries.push({ ns, locale })
        return () => {}
      },
      bind(_ns: string) {
        return (key: string) => key
      },
    },
    slots: {
      inject(key: string, callback: () => () => void) {
        injectedKeys.push(key)
        const dispose = callback()
        return () => {
          dispose()
        }
      },
      register(registerOptions: Record<string, unknown>, component: unknown) {
        if (options.registerThrows === true) {
          throw new Error(`slot "${String(registerOptions['name'])}" is not declared`)
        }
        registrations.push({ options: registerOptions, component })
        return () => {}
      },
    },
  }
  return { ctx: ctx as unknown as CcteamClientContext, registrations, dictionaries, injectedKeys, warns }
}

describe('apply (registration shape)', () => {
  it('registers the entry and the panel with {name, id: "ccteam"} and the ccteam locale namespace', () => {
    const recording = recordingCtx()
    apply(recording.ctx)
    const shapes = recording.registrations.map(r => ({
      name: r.options['name'],
      id: r.options['id'],
      locale: r.options['locale'],
      hasInject: typeof r.options['inject'] === 'function',
      componentIsFunction: typeof r.component === 'function',
    }))
    expect(shapes).toEqual([
      { name: 'sidebar.footer.action', id: 'ccteam', locale: 'ccteam', hasInject: true, componentIsFunction: true },
      { name: 'shell.overlay', id: 'ccteam', locale: 'ccteam', hasInject: true, componentIsFunction: true },
    ])
    // The injected business face carries the store and the api client.
    const inject = recording.registrations[0]!.options['inject'] as () => Record<string, unknown>
    const face = inject()
    expect(typeof face['store']).toBe('object')
    expect(typeof face['api']).toBe('object')
    // Both registrations ride declaration observation, not blind registration.
    expect(recording.injectedKeys).toEqual(['sidebar.footer.action', 'shell.overlay'])
    // Dictionaries: both shipped locales of the ccteam namespace.
    expect(recording.dictionaries).toEqual([
      { ns: 'ccteam', locale: 'zh' },
      { ns: 'ccteam', locale: 'en' },
    ])
    expect(recording.warns).toEqual([])
  })

  it('never lets a rejected registration escape: apply degrades and only warns', () => {
    const recording = recordingCtx({ registerThrows: true })
    expect(() => {
      apply(recording.ctx)
    }).not.toThrow()
    expect(recording.registrations).toEqual([])
    expect(recording.warns.some(message => message.includes('degrading'))).toBe(true)
  })
})
