/**
 * The two entry affordances: the sidebar-foot button (slot mode — copies the
 * native Settings trigger's geometry in both column states) and the floating
 * right-edge handle (body-portal fallback on older DSH). Both carry the
 * completed-turns badge, show the muted not-connected dot, and toggle the
 * panel through the one store.
 */
import { useSyncExternalStore } from 'react'
import { IconBranchOutline16, Tooltip } from '@deepseek-ai/dsh-client-ui-primitives'
import type { ConsoleInjected, SidebarFooterActionOwnerProps, T } from './slots.js'
import type { ConsoleStore } from './store.js'
import css from './panel.module.css'

/** The store slice both entries render from. */
interface EntrySlice {
  open: boolean
  badge: number
  disconnected: boolean
}

function useEntrySlice(store: ConsoleStore): EntrySlice {
  const state = useSyncExternalStore(store.subscribe, store.getState, store.getState)
  return {
    open: state.open,
    badge: state.badge,
    disconnected: state.connection.phase === 'unreachable' || state.connection.phase === 'unconfigured',
  }
}

function EntrySignals({ badge, disconnected, t }: { badge: number; disconnected: boolean; t: T }) {
  if (badge > 0) {
    return (
      <span className={css.badge} title={t('entry.badge', { count: badge })}>
        {badge > 99 ? '99+' : badge}
      </span>
    )
  }
  if (disconnected) return <span className={css.entryDot} aria-hidden="true" />
  return null
}

/** Composed slot props of the `sidebar.footer.action` entry. */
export type EntryButtonProps = SidebarFooterActionOwnerProps & ConsoleInjected & { t: T }

/**
 * Render the sidebar-foot entry button.
 * @param props - owner column state + injected store/api + the locale seat.
 * @returns the entry button.
 */
export function EntryButton({ wide, store, t }: EntryButtonProps) {
  const { open, badge, disconnected } = useEntrySlice(store)
  return (
    <Tooltip label={t('entry.title')} delayMs={500} disabled={wide}>
      <button
        type="button"
        data-ccteam-console=""
        className={wide ? css.entry : `${css.entry} ${css.rail}`}
        aria-label={t('entry.label')}
        aria-expanded={open}
        onClick={() => {
          store.dispatch({ type: 'toggle_panel' })
        }}
      >
        <IconBranchOutline16 size={wide ? 16 : 18} />
        {wide && <span className={css.entryLabel}>{t('entry.title')}</span>}
        <EntrySignals badge={badge} disconnected={disconnected} t={t} />
      </button>
    </Tooltip>
  )
}

/**
 * Render the fallback entry: a floating handle hugging the right viewport
 * edge (body-portal mode only).
 * @param props - injected store + the bound translate.
 * @returns the handle button.
 */
export function FallbackHandle({ store, t }: { store: ConsoleStore; t: T }) {
  const { open, badge, disconnected } = useEntrySlice(store)
  if (open) return null
  return (
    <button
      type="button"
      className={css.fallbackHandle}
      aria-label={t('entry.label')}
      aria-expanded={false}
      onClick={() => {
        store.dispatch({ type: 'toggle_panel' })
      }}
    >
      <IconBranchOutline16 size={16} />
      <EntrySignals badge={badge} disconnected={disconnected} t={t} />
    </button>
  )
}
