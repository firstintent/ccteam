/**
 * The sidebar-foot entry: the `sidebar.footer.action` seat beside DSH's own
 * Settings trigger, drawn with that trigger's geometry in both column states
 * (42px foot row wide, 36px circle on the 56px rail — ui-settings-general
 * SettingsRoot.module.css `.trigger` / `.trigger.rail`). Carries the
 * completed-turns badge while the workbench is closed and a muted dot while
 * ccteam is not reachable; toggles the workbench through the one store.
 */
import clsx from 'clsx'
import { IconBranchOutline16, Tooltip } from '@deepseek-ai/dsh-client-ui-primitives'
import type { EntryButtonProps } from './slots.js'
import css from './workbench.module.css'

/**
 * Render the sidebar-foot entry button.
 * @param props - owner column state + the injected face (bound `useConsole`, dispatch) + the locale seat.
 * @returns the entry button.
 */
export function EntryButton({ wide, useConsole, dispatch, t }: EntryButtonProps) {
  const open = useConsole(state => state.open)
  const badge = useConsole(state => state.badge)
  const disconnected = useConsole(
    state => state.connection.phase === 'unreachable' || state.connection.phase === 'unconfigured',
  )
  return (
    <Tooltip label={t('entry.title')} delayMs={500} disabled={wide}>
      <button
        type="button"
        data-ccteam-console=""
        className={clsx(css.entry, !wide && css.rail)}
        aria-label={t('entry.label')}
        aria-expanded={open}
        onClick={() => {
          dispatch({ type: 'toggle_panel' })
        }}
      >
        <IconBranchOutline16 size={wide ? 16 : 18} />
        {wide && <span className={css.entryLabel}>{t('entry.title')}</span>}
        {badge > 0
          ? (
              <span className={css.badge} title={t('entry.badge', { count: badge })}>
                {badge > 99 ? '99+' : badge}
              </span>
            )
          : disconnected && <span className={css.entryDot} aria-hidden="true" />}
      </button>
    </Tooltip>
  )
}
