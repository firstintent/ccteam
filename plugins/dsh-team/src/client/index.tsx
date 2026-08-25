/**
 * Client half: the ccteam team panel, composed exclusively from DSH-native
 * material (owner decree 2026-08-21): components from
 * `@deepseek-ai/dsh-client-ui-primitives`, semantic `--dsw-alias-*` /
 * `--dsw-specific-*` tokens only, copy through the DSH locale service.
 * Zero imports from ccteam-web.
 *
 * This module keeps only the plugin face and the wiring: dictionaries, the
 * one store + BFF client, the always-on team event stream (badge feed), and
 * the version-gated mount —
 *   1. entry button   → slot `sidebar.footer.action` (list, root)
 *   2. panel overlay  → slot `shell.overlay`         (list, root)
 *   3. fallback       → body portal (older DSH without those slots)
 *
 * All network traffic goes through the host BFF (src/shared/contract.ts).
 */
import type { PanelEvent } from '../shared/contract.js'
import { createApi } from './api.js'
import { attachPersistence, createStore, initialState, loadPersisted } from './store.js'
import type { StorageLike } from './store.js'
import { NS } from './slots.js'
import type { CcteamClientContext, ConsoleInjected, T } from './slots.js'
import { en, zh } from './locales.js'
import { mountVersionGated } from './mount.js'
import { EntryButton, FallbackHandle } from './EntryButton.js'
import { Panel, refreshStatus } from './Panel.js'
import css from './panel.module.css'

export const name = 'ccteam-team'
export const inject = ['slots', 'locale']

function browserStorage(): StorageLike | undefined {
  try {
    return typeof localStorage === 'undefined' ? undefined : localStorage
  } catch {
    // Storage access itself can throw (privacy modes); the panel just runs
    // unpersisted then.
    return undefined
  }
}

/**
 * Plugin body: wire the store/api/stream and mount through the version gate.
 * @param ctx - client cordis context (slots + locale services injected).
 */
export function apply(ctx: CcteamClientContext): void {
  const warn = (message: string): void => {
    const logger = (ctx as { logger?: { warn(text: string): void } }).logger
    if (logger !== undefined) logger.warn(message)
    else console.warn(message)
  }

  ctx.effect(() => {
    const disposeZh = ctx.locale.register(NS, 'zh', zh)
    const disposeEn = ctx.locale.register(NS, 'en', en)
    return () => {
      disposeEn()
      disposeZh()
    }
  }, 'ccteam-team: dictionaries')
  // The same translate the framework's `t` seat resolves to — handed
  // explicitly wherever no slot machinery runs (the body-portal fallback).
  const t = ctx.locale.bind(NS) as T

  const store = createStore(initialState(loadPersisted(browserStorage())))
  const api = createApi()
  const injected = (): ConsoleInjected => ({ store, api })

  ctx.effect(() => {
    const storage = browserStorage()
    return storage === undefined ? () => {} : attachPersistence(store, storage)
  }, 'ccteam-team: persistence')

  // Always-on team stream: feeds the entry badge (`turn_done` while the
  // panel is closed) and marks the tree stale on `graph` frames. Per-sid
  // subscriptions live with the chat view.
  ctx.effect(() => {
    const stream = api.events({
      onEvent(event: PanelEvent) {
        if (event.kind === 'turn_done') {
          store.dispatch({ type: 'turn_done', ...(event.sid !== undefined ? { sid: event.sid } : {}) })
        } else if (event.kind === 'graph') {
          store.dispatch({ type: 'graph_stale' })
        }
        // Unknown kinds: ignored (forward-compat contract).
      },
      onOpen() {
        void refreshStatus(store, api)
      },
      onError() {
        store.dispatch({ type: 'status_failed' })
      },
    })
    return () => {
      stream.close()
    }
  }, 'ccteam-team: team event stream')
  void refreshStatus(store, api)

  // Body-portal fallback (older DSH): one content tree, second container.
  // react-dom/client is a platform external; loading it lazily keeps the
  // slot path free of it and keeps non-DOM runs (tests, node boots) safe.
  const mountFallback = (): (() => void) => {
    if (typeof document === 'undefined') {
      warn('ccteam-team: no document — the body-portal fallback cannot mount here')
      return () => {}
    }
    let cancelled = false
    let cleanup: (() => void) | undefined
    void import('react-dom/client')
      .then(({ createRoot }) => {
        if (cancelled) return
        const host = document.createElement('div')
        host.setAttribute('data-ccteam-console', '')
        host.className = css.portalRoot ?? ''
        document.body.appendChild(host)
        const root = createRoot(host)
        root.render(
          <>
            <FallbackHandle store={store} t={t} />
            <Panel store={store} api={api} t={t} />
          </>,
        )
        cleanup = () => {
          root.unmount()
          host.remove()
        }
      })
      .catch((error: unknown) => {
        warn(`ccteam-team: react-dom unavailable for the fallback mount (${error instanceof Error ? error.message : String(error)})`)
      })
    return () => {
      cancelled = true
      cleanup?.()
      cleanup = undefined
    }
  }

  ctx.effect(() => {
    const gate = mountVersionGated(ctx.slots, {
      targets: [
        {
          key: 'sidebar.footer.action',
          register: () => ctx.slots.register(
            { name: 'sidebar.footer.action', id: 'ccteam', order: 0, locale: NS, inject: injected },
            EntryButton,
          ),
        },
        {
          key: 'shell.overlay',
          register: () => ctx.slots.register(
            { name: 'shell.overlay', id: 'ccteam', order: 0, locale: NS, inject: injected },
            Panel,
          ),
        },
      ],
      mountFallback,
      warn,
    })
    return () => {
      gate.dispose()
    }
  }, 'ccteam-team: mount')
}
