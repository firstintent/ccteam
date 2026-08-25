/**
 * Version-gated mounting: the entry button and the panel want the
 * `sidebar.footer.action` and `shell.overlay` slots, but an older DSH may
 * not declare them (SlotCore throws on registration into an undeclared
 * slot). This controller waits on both declarations through
 * `slots.inject`, and the moment either target proves impossible — a
 * registration throws, `slots.inject` is unavailable, or a declaration
 * never lands before the deadline — it degrades the WHOLE mount to one
 * body portal (one content tree, two containers). No error escapes; every
 * degradation is a logged warning.
 */

/** The slice of the slots service the gate uses (structural: fakeable, and tolerant of older shapes). */
export interface GateSlots {
  /** Observe a slot declaration lifetime (absent on older runtimes). */
  inject?(key: string, callback: () => () => void): () => void
}

/** One slot target: the key to wait for and the registration to place there. */
export interface GateTarget {
  key: string
  /** Perform the real registration; throws = this DSH cannot host it. */
  register(): () => void
}

/** Gate wiring. */
export interface GateOptions {
  targets: readonly GateTarget[]
  /** Mount the body-portal fallback; must not throw (guard internally). */
  mountFallback(): () => void
  warn(message: string): void
  /** How long declarations may take before the gate gives up (default 2500ms). */
  deadlineMs?: number
  /** Injectable timer (tests). Returns the cancel. */
  schedule?(callback: () => void, ms: number): () => void
}

/** Terminal gate modes (deciding is the pre-deadline state). */
export type GateMode = 'deciding' | 'slots' | 'fallback'

/** Live gate controller. */
export interface GateController {
  /** Current mode (observable for diagnostics and tests). */
  mode(): GateMode
  /** Tear everything down: watchers, slot registrations, fallback, timer. */
  dispose(): void
}

const DEFAULT_DEADLINE_MS = 2500

/**
 * Start the gate.
 * @param slots - the slots service face.
 * @param options - targets + fallback + wiring.
 * @returns the controller.
 */
export function mountVersionGated(slots: GateSlots, options: GateOptions): GateController {
  const schedule = options.schedule
    ?? ((callback: () => void, ms: number) => {
      const timer = setTimeout(callback, ms)
      return () => {
        clearTimeout(timer)
      }
    })

  let mode: GateMode = 'deciding'
  let disposed = false
  const registered = new Map<string, () => void>()
  const watchers: Array<() => void> = []
  let disposeFallback: (() => void) | undefined
  let cancelDeadline: (() => void) | undefined

  const disposeSlotSide = (): void => {
    for (const dispose of watchers.splice(0)) {
      try {
        dispose()
      } catch {
        // A watcher disposer crashing must not stop the rest of teardown;
        // nothing downstream depends on it once the gate leaves slot mode.
      }
    }
    for (const [, dispose] of registered) {
      try {
        dispose()
      } catch {
        // Same containment as watcher disposers above.
      }
    }
    registered.clear()
  }

  const engageFallback = (reason: string): void => {
    if (disposed || mode === 'fallback') return
    mode = 'fallback'
    cancelDeadline?.()
    cancelDeadline = undefined
    options.warn(`ccteam-team: ${reason} — degrading to the body-portal mount`)
    disposeSlotSide()
    try {
      disposeFallback = options.mountFallback()
    } catch (error) {
      options.warn(`ccteam-team: fallback mount failed: ${error instanceof Error ? error.message : String(error)}`)
      disposeFallback = undefined
    }
  }

  const maybeSettle = (): void => {
    if (disposed || mode !== 'deciding') return
    if (options.targets.every(target => registered.has(target.key))) {
      mode = 'slots'
      cancelDeadline?.()
      cancelDeadline = undefined
    }
  }

  const tryRegister = (target: GateTarget): (() => void) => {
    if (mode === 'fallback') return () => {}
    try {
      const dispose = target.register()
      const guarded = (): void => {
        if (!registered.has(target.key)) return
        registered.delete(target.key)
        dispose()
      }
      registered.set(target.key, guarded)
      maybeSettle()
      return guarded
    } catch (error) {
      engageFallback(
        `slot "${target.key}" rejected the registration (${error instanceof Error ? error.message : String(error)})`,
      )
      return () => {}
    }
  }

  for (const target of options.targets) {
    if (mode !== 'deciding') break
    if (typeof slots.inject === 'function') {
      try {
        watchers.push(slots.inject(target.key, () => tryRegister(target)))
        continue
      } catch (error) {
        engageFallback(
          `slots.inject("${target.key}") threw (${error instanceof Error ? error.message : String(error)})`,
        )
        break
      }
    }
    // No declaration observer on this runtime: probe directly, once — the
    // registration either lands (the slot is already declared) or throws,
    // and the throw engages the fallback.
    tryRegister(target)
  }

  if (mode === 'deciding') {
    cancelDeadline = schedule(() => {
      if (mode !== 'deciding') return
      const missing = options.targets.filter(target => !registered.has(target.key)).map(target => target.key)
      engageFallback(`slot(s) ${missing.map(key => `"${key}"`).join(', ')} never declared`)
    }, options.deadlineMs ?? DEFAULT_DEADLINE_MS)
  }

  return {
    mode: () => mode,
    dispose() {
      if (disposed) return
      disposed = true
      cancelDeadline?.()
      cancelDeadline = undefined
      disposeSlotSide()
      disposeFallback?.()
      disposeFallback = undefined
    },
  }
}
