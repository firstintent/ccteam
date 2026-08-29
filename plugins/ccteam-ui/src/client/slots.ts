/**
 * This plugin's slot-side contract, spelled the way every DSH client package
 * spells its own (ui-goal, ui-settings-plugins, ui-settings-general): type-only
 * imports pull the SlotMap / Context merges of the packages whose seats it
 * registers into, one LocaleNamespaceMap merge declares its own copy, and each
 * component's props are the framework's composed shares — never re-typed by
 * hand. A type-only import is erased at build, so none of these become a
 * runtime edge (the client bundle purity gate only ever sees value imports).
 */
import type { InjectFace, PropsLocale, PropsRuntime, TranslateNS } from '@deepseek-ai/dsh-client-ui-slots'
import type { ClientContext } from '@deepseek-ai/dsh-client-runtime/client'
// ctx.locale (register / bind).
import type {} from '@deepseek-ai/dsh-client-locale/client'
// 'shell.overlay' — the frame-wide floating layer (declared by ui-layout's root entry).
import type {} from '@deepseek-ai/dsh-client-ui-layout/client'
// 'sidebar.footer.action' — the seat beside Settings at the sidebar foot (declared by ui-sidebar).
import type {} from '@deepseek-ai/dsh-client-ui-sidebar/client'
// ctx.settingsScope — the per-namespace settings scope binder.
import type {} from '@deepseek-ai/dsh-client-ui-settings/client'
// 'settings.plugin.item' — one card per settings namespace (declared by ui-settings-plugins' configurable tab).
import type {} from '@deepseek-ai/dsh-client-ui-settings-plugins/client'
import type { CcteamLocaleKey } from './locales.js'
import type { ApiClient } from './api.js'
import type { Action, ConsoleStore } from './store.js'
import type { SettingsCardController, SettingsCardFace } from './settings/form.js'

declare module '@deepseek-ai/dsh-client-ui-slots' {
  interface LocaleNamespaceMap {
    /** This plugin's copy. */
    ccteam: CcteamLocaleKey
  }
}

/** Dictionary namespace owned by this plugin. */
export const NS = 'ccteam'

/** The typed translate of this plugin's namespace (the framework's `t` seat). */
export type T = TranslateNS<'ccteam'>

/**
 * The workbench's injected business face, shared by its two seats: the one
 * store under the reserved `hooks` compartment (the framework binds it as the
 * `useConsole` selector hook), the store's write path, and the BFF client.
 */
export interface ConsoleFace {
  hooks: { console: ConsoleStore }
  dispatch(action: Action): void
  api: ApiClient
}

/**
 * The settings card's injected face: the staged form (its controller under
 * `hooks.card`) plus the workbench store (under `hooks.console`, for the
 * engine slice the 「引擎」 section renders), the store's write path, and the
 * BFF client the engine actions call.
 */
export interface SettingsSeatFace extends SettingsCardFace {
  hooks: { card: SettingsCardController; console: ConsoleStore }
  dispatch(action: Action): void
  api: ApiClient
}

/** Composed props of the `sidebar.footer.action` entry: owner column state + face + `t`. */
export type EntryButtonProps = PropsRuntime<'sidebar.footer.action'> & InjectFace<ConsoleFace> & PropsLocale<'ccteam'>

/** Composed props of the `shell.overlay` entry: face + `t` (the layer passes no owner props). */
export type WorkbenchProps = PropsRuntime<'shell.overlay'> & InjectFace<ConsoleFace> & PropsLocale<'ccteam'>

/** Composed props of one `settings.plugin.item` card: the card face + `t`. */
export type SettingsCardProps = PropsRuntime<'settings.plugin.item'> & InjectFace<SettingsSeatFace> & PropsLocale<'ccteam'>

/** DSH's own workspace list, as the framework's global seat hands it to every slot component. */
export type UseWorkspaces = WorkbenchProps['useWorkspaces']

/** The client context as this plugin sees it (locale + settingsScope merges pulled above). */
export type CcteamClientContext = ClientContext
