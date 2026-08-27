/**
 * Client half of `@ccteam/ccteam-ui`: the ccteam workbench inside the DSH web
 * console, composed exclusively from DSH-native material — components from
 * `@deepseek-ai/dsh-client-ui-primitives`, semantic `--dsw-alias-*` /
 * `--dsw-specific-*` tokens only, copy through the DSH locale service. Zero
 * imports from ccteam-web.
 *
 * Shaped like every DSH client plugin (ui-goal, ui-settings-plugins):
 *   - `inject` names the services `apply` needs;
 *   - dictionaries register through `ctx.locale`;
 *   - each seat is contributed with `ctx.slots.inject(<slot>, () =>
 *     ctx.slots.register(...))` — the framework waits for the declaring
 *     package's slot, runs the registration, and owns its lifetime (plugin
 *     unload removes it, a re-declaration re-runs it);
 *   - business state reaches components as bound selector hooks from the
 *     inject face's `hooks` compartment, never through React context or globals.
 *
 * Seats:
 *   1. `sidebar.footer.action` — the entry button beside DSH's Settings trigger
 *   2. `shell.overlay`         — the workbench (team tree / conversation / details)
 *   3. `settings.plugin.item`  — one card per ccteam settings namespace
 *
 * All network traffic goes through the host BFF (src/shared/contract.ts).
 */
import type { PanelEvent } from '../shared/contract.js'
import { createApi } from './api.js'
import { attachPersistence, createStore, initialState, loadPersisted } from './store.js'
import type { StorageLike } from './store.js'
import { NS } from './slots.js'
import type { CcteamClientContext, ConsoleFace } from './slots.js'
import { en, zh } from './locales.js'
import { EntryButton } from './EntryButton.js'
import { Workbench, refreshStatus } from './Workbench.js'
import { SettingsCard } from './settings/SettingsCard.js'
import { CLIENT_CARD, SettingsCardController, UI_CARD } from './settings/form.js'

export const name = 'ccteam-ui'

/** Services required by this plugin (cordis fiber inject). */
export const inject = ['slots', 'locale', 'settingsScope']

function browserStorage(): StorageLike | undefined {
  try {
    return typeof localStorage === 'undefined' ? undefined : localStorage
  } catch {
    // Storage access itself can throw (privacy modes); the workbench just
    // runs unpersisted then.
    return undefined
  }
}

/**
 * Plugin body: wire the store/api/stream and contribute the three seats.
 * @param ctx - client cordis context (slots + locale + settingsScope services injected).
 */
export function apply(ctx: CcteamClientContext): void {
  ctx.effect(() => ctx.locale.register(NS, { zh, en }), 'ccteam-ui: dictionaries')

  const store = createStore(initialState(loadPersisted(browserStorage())))
  const api = createApi()
  const face = (): ConsoleFace => ({ hooks: { console: store }, dispatch: store.dispatch, api })

  ctx.effect(() => {
    const storage = browserStorage()
    return storage === undefined ? () => {} : attachPersistence(store, storage)
  }, 'ccteam-ui: persistence')

  // Always-on team stream: feeds the entry badge (`turn_done` while the
  // workbench is closed), marks the tree stale on `graph` frames, narrates
  // delegation into open parent chats, and routes lifecycle frames to the
  // chats that are open. Per-sid subscriptions live with the chat view.
  ctx.effect(() => {
    const stream = api.events({
      onEvent(event: PanelEvent) {
        if (event.kind === 'turn_done') {
          store.dispatch({ type: 'turn_done', ...(event.sid !== undefined ? { sid: event.sid } : {}) })
        } else if (event.kind === 'graph') {
          store.dispatch({ type: 'graph_stale' })
        } else if (event.kind === 'delegation') {
          store.dispatch({
            type: 'delegation',
            relation: event.relation,
            ...(event.parentSid === undefined ? {} : { parentSid: event.parentSid }),
            ...(event.childSid === undefined ? {} : { childSid: event.childSid }),
            ...(event.title === undefined ? {} : { title: event.title }),
            ...(event.reason === undefined ? {} : { reason: event.reason }),
          })
        } else if (event.kind === 'session' && event.event.kind === 'lifecycle') {
          // Only chats that were opened get lifecycle rows (the tree shows
          // everyone's state through the graph).
          if (store.getSnapshot().chats[event.sid] !== undefined) {
            store.dispatch({ type: 'session_event', sid: event.sid, event: event.event, now: Date.now() })
          }
        }
        // Unknown kinds: ignored (forward-compat contract).
      },
      onOpen() {
        void refreshStatus(store.dispatch, api)
      },
      onError() {
        // Ask, don't assume: a dropped stream re-probes status — the daemon
        // may be fine (proxy hiccup) or truly down (the probe fails too).
        void refreshStatus(store.dispatch, api)
      },
    })
    return () => {
      stream.close()
    }
  }, 'ccteam-ui: team event stream')
  void refreshStatus(store.dispatch, api)

  ctx.slots.inject('sidebar.footer.action', () => ctx.slots.register({
    name: 'sidebar.footer.action',
    id: 'ccteam',
    order: 0,
    locale: NS,
    inject: face,
  }, EntryButton))

  ctx.slots.inject('shell.overlay', () => ctx.slots.register({
    name: 'shell.overlay',
    id: 'ccteam',
    order: 0,
    locale: NS,
    inject: face,
  }, Workbench))

  // One card per ccteam settings namespace. The configurable-plugins tab
  // dispatches by namespace and renders nothing for one the Host does not
  // serve, so the client card is inert wherever that plugin is absent.
  const cards = [UI_CARD, CLIENT_CARD].map(spec =>
    new SettingsCardController(ctx.settingsScope.bind({ namespace: spec.namespace }), spec))
  ctx.slots.inject('settings.plugin.item', function* () {
    for (const card of cards) {
      yield ctx.slots.register({
        name: 'settings.plugin.item',
        key: card.spec.namespace,
        locale: NS,
        inject: () => card.inject(),
      }, SettingsCard)
    }
  })
}
