/**
 * The DSH-side contract this plugin registers against, mirrored as
 * declaration merges (the DSH pattern: SlotMap/LocaleNamespaceMap keys merge
 * lexically into `@deepseek-ai/dsh-client-ui-slots`).
 *
 * The two target slots are declared upstream — `sidebar.footer.action` by
 * ui-sidebar (`src/client/contract/slots.ts`: list/root, owner
 * `{ wide: boolean }`, false = the 56px rail) and `shell.overlay` by
 * ui-layout (`src/client/index.ts`: list/root, no owner props, rendered in
 * AppFrame's click-through overlay layer). Their type packages are not
 * published, so the merges live here, byte-mirroring the upstream
 * declarations; drift shows up as a runtime registration failure, which the
 * version gate (mount.ts) degrades to the body portal.
 */
import type { TranslateNS } from '@deepseek-ai/dsh-client-ui-slots'
import type { ClientContext } from '@deepseek-ai/dsh-client-runtime/client'
import type { CcteamLocaleKey } from './locales.js'
import type { ApiClient } from './api.js'
import type { ConsoleStore } from './store.js'

declare module '@deepseek-ai/dsh-client-ui-slots' {
  interface SlotMap {
    /** Sidebar-foot action seat (declared by ui-sidebar; owner passes the column state). */
    'sidebar.footer.action': { kind: 'list'; scope: 'root'; owner: SidebarFooterActionOwnerProps }
    /** App-wide overlay layer (declared by ui-layout; additive, click-through until an entry opts in). */
    'shell.overlay': { kind: 'list'; scope: 'root' }
  }
  interface LocaleNamespaceMap {
    /** This plugin's copy. */
    ccteam: CcteamLocaleKey
  }
}

/** Owner share of `sidebar.footer.action` (mirror of ui-sidebar's contract). */
export interface SidebarFooterActionOwnerProps {
  /** Whether the sidebar renders wide content (false = 56px rail). */
  wide: boolean
}

/** Dictionary namespace owned by this plugin. */
export const NS = 'ccteam'

/** The typed translate of this plugin's namespace. */
export type T = TranslateNS<'ccteam'>

/**
 * The registrant business face injected into both slot components (and passed
 * explicitly in the body-portal fallback): the one store and the BFF client.
 */
export interface ConsoleInjected {
  store: ConsoleStore
  api: ApiClient
}

/**
 * The locale service face this plugin consumes (`ctx.locale`). The locale
 * package's Context merge is not importable here (client bundle purity: no
 * cross-plugin value imports, and the type package is unpublished), so the
 * service is typed structurally with exactly the two members used.
 */
export interface LocaleServiceLike {
  /** Register one namespace dictionary for one locale; returns the disposer. */
  register(ns: string, locale: string, dict: Record<string, string>): () => void
  /** Bind a namespace to its stable translate function. */
  bind(ns: string): (key: string, params?: Record<string, unknown>) => string
}

/** The client context as this plugin sees it: the runtime context + the locale service. */
export type CcteamClientContext = ClientContext & { locale: LocaleServiceLike }
