/**
 * The console column: the `shell.overlay` entry — a right-side column that
 * floats over the 3-column layout (never squeezes it), sliding in on the
 * deepsuite sider curve, width-draggable with AppFrame's handle chrome, one
 * header, three views (tree / chat / spawn), Esc walking back and finally
 * out. Every not-happy state names its next action: unreachable → copyable
 * `ccteam start` + retry; unconfigured → the two-step Settings pointer;
 * empty → the spawn hero. Business state arrives through the bound
 * `useConsole` hook and leaves through `dispatch`; nothing here owns state.
 */
import { useEffect, useRef, useState } from 'react'
import {
  Button,
  IconBranchOutline16,
  IconCheckOutline14,
  IconChevronLeftOutline14,
  IconCloseOutline16,
  IconCopyOutline16,
  IconPlusOutline16,
  IconWarningOutline16,
  Pill,
  StateDot,
  writeClipboard,
} from '@deepseek-ai/dsh-client-ui-primitives'
import type { ApiClient } from './api.js'
import { dotState, findNode, planSpawnOutcome, projectSlugs } from './store.js'
import type { Action, ConsoleState, View } from './store.js'
import type { PanelProps, T } from './slots.js'
import { SessionTree } from './SessionTree.js'
import { SessionChat } from './SessionChat.js'
import { SpawnForm } from './SpawnForm.js'
import css from './panel.module.css'

/** The store's write path, as the views see it. */
export type Dispatch = (action: Action) => void

/** Exit-animation settle; keep in step with --ds-transition-duration-slow. */
const PANEL_EXIT_MS = 320

/**
 * Refresh connectivity into the store (boot, retry, reconnect).
 * @param dispatch - the store's write path.
 * @param api - the BFF client.
 * @returns settled promise (results land as dispatches).
 */
export async function refreshStatus(dispatch: Dispatch, api: ApiClient): Promise<void> {
  // The 'checking' phase exists only before the FIRST result: re-probes
  // (retry button, stream reconnects) must not flicker the state screen.
  try {
    const status = await api.call('status', {})
    dispatch({ type: 'status_loaded', status })
  } catch {
    // The failure IS the result: the store's phase turns unreachable.
    dispatch({ type: 'status_failed' })
  }
}

/**
 * Refresh the team graph into the store.
 * @param dispatch - the store's write path.
 * @param api - the BFF client.
 * @returns settled promise (results land as dispatches).
 */
export async function refreshGraph(dispatch: Dispatch, api: ApiClient): Promise<void> {
  dispatch({ type: 'graph_loading' })
  try {
    const graph = await api.call('team.graph', {})
    dispatch({ type: 'graph_loaded', graph })
  } catch (error) {
    dispatch({ type: 'graph_failed', message: error instanceof Error ? error.message : String(error) })
  }
}

function CopyChip({ text, t }: { text: string; t: T }) {
  const [copied, setCopied] = useState(false)
  return (
    <span className={css.codeChip}>
      {text}
      <button
        type="button"
        className={css.iconBtn}
        aria-label={t('states.copy')}
        title={copied ? t('states.copied') : t('states.copy')}
        onClick={() => {
          writeClipboard(text)
          setCopied(true)
        }}
      >
        {copied ? <IconCheckOutline14 size={14} /> : <IconCopyOutline16 size={14} />}
      </button>
    </span>
  )
}

function StateScreen({ state, dispatch, api, t }: { state: ConsoleState; dispatch: Dispatch; api: ApiClient; t: T }) {
  const phase = state.connection.phase
  if (phase === 'checking') {
    return (
      <div className={css.state}>
        <StateDot state="ongoing" size={14} />
        <div className={css.stateBody}>{t('states.checking')}</div>
      </div>
    )
  }
  if (phase === 'unconfigured') {
    return (
      <div className={css.state}>
        <IconWarningOutline16 className={css.stateIcon} size={24} />
        <div className={css.stateTitle}>{t('states.unconfigured.title')}</div>
        <ol className={css.stateSteps}>
          <li>{t('states.unconfigured.step1')}</li>
          <li>{t('states.unconfigured.step2')}</li>
        </ol>
        <div className={css.stateActions}>
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              void refreshStatus(dispatch, api)
            }}
          >
            {t('states.retry')}
          </Button>
        </div>
      </div>
    )
  }
  return (
    <div className={css.state}>
      <IconWarningOutline16 className={css.stateIcon} size={24} />
      <div className={css.stateTitle}>{t('states.unreachable.title')}</div>
      <div className={css.stateBody}>{t('states.unreachable.body')}</div>
      <CopyChip text="ccteam start" t={t} />
      <div className={css.stateActions}>
        <Button
          variant="outline"
          size="sm"
          onClick={() => {
            void refreshStatus(dispatch, api)
          }}
        >
          {t('states.retry')}
        </Button>
      </div>
    </div>
  )
}

function EmptyHero({ t, onSpawn }: { t: T; onSpawn(): void }) {
  return (
    <div className={css.state}>
      <IconBranchOutline16 className={css.stateIcon} size={32} />
      <div className={css.stateTitle}>{t('tree.empty.title')}</div>
      <div className={css.stateBody}>{t('tree.empty.body')}</div>
      <div className={css.stateActions}>
        <Button variant="primary" size="md" icon={<IconPlusOutline16 size={16} />} onClick={onSpawn}>
          {t('tree.spawn')}
        </Button>
      </div>
    </div>
  )
}

function graphIsEmpty(state: ConsoleState): boolean {
  return state.graph !== null && state.graph.projects.every(project => project.nodes.length === 0)
}

function PanelBody({ state, view, dispatch, api, t }: {
  state: ConsoleState
  view: View
  dispatch: Dispatch
  api: ApiClient
  t: T
}) {
  if (view.kind === 'chat') {
    return (
      <SessionChat
        key={view.sid}
        sid={view.sid}
        chat={state.chats[view.sid] ?? { rows: [], activity: undefined, loading: false, error: null, notices: [] }}
        node={findNode(state.graph, view.sid)}
        dispatch={dispatch}
        api={api}
        t={t}
      />
    )
  }
  if (view.kind === 'spawn') {
    return (
      <SpawnForm
        vendors={state.connection.vendors}
        projects={projectSlugs(state.graph)}
        lastProject={state.spawnProject}
        busy={state.spawn.busy}
        error={state.spawn.error}
        t={t}
        onCancel={() => {
          dispatch({ type: 'back' })
        }}
        onCreate={(request) => {
          if (request.project !== undefined) {
            dispatch({ type: 'set_spawn_project', project: request.project })
          }
          dispatch({ type: 'spawn_started' })
          api
            .call('session.spawn', request)
            .then((response) => {
              const outcome = planSpawnOutcome(response)
              if (outcome.kind === 'form_error') {
                dispatch({ type: 'spawn_failed', message: outcome.message })
                return
              }
              // A sid exists upstream even when the first task failed: enter
              // the session and state the failure inside its chat.
              dispatch({ type: 'spawn_done' })
              dispatch({ type: 'open_chat', sid: outcome.sid })
              if (outcome.errorMessage !== undefined) {
                dispatch({ type: 'send_failed', sid: outcome.sid, message: outcome.errorMessage })
              }
              void refreshGraph(dispatch, api)
            })
            .catch((error: unknown) => {
              dispatch({
                type: 'spawn_failed',
                message: error instanceof Error ? error.message : String(error),
              })
            })
        }}
      />
    )
  }
  if (state.connection.phase !== 'ok') return <StateScreen state={state} dispatch={dispatch} api={api} t={t} />
  if (state.graphError !== null && state.graph === null) {
    return (
      <div className={css.state}>
        <IconWarningOutline16 className={css.stateIcon} size={24} />
        <div className={css.stateTitle}>{t('tree.error')}</div>
        <div className={css.stateActions}>
          <Button
            variant="outline"
            size="sm"
            onClick={() => {
              void refreshGraph(dispatch, api)
            }}
          >
            {t('states.retry')}
          </Button>
        </div>
      </div>
    )
  }
  if (state.graph === null) {
    return (
      <div className={css.state}>
        <StateDot state="ongoing" size={14} />
      </div>
    )
  }
  if (graphIsEmpty(state)) {
    return (
      <EmptyHero
        t={t}
        onSpawn={() => {
          dispatch({ type: 'open_spawn' })
        }}
      />
    )
  }
  return (
    <SessionTree
      graph={state.graph}
      recents={state.recents}
      collapsed={state.collapsed}
      t={t}
      onOpenChat={(sid) => {
        dispatch({ type: 'open_chat', sid })
      }}
      onToggleProject={(slug) => {
        dispatch({ type: 'toggle_project', slug })
      }}
    />
  )
}

/**
 * Render the console column (or nothing while closed).
 * @param props - the injected face (bound `useConsole`, dispatch, api) + the locale seat.
 * @returns the panel element tree.
 */
export function Panel({ useConsole, dispatch, api, t }: PanelProps) {
  const state = useConsole(snapshot => snapshot)
  const [rendered, setRendered] = useState(state.open)
  const [dragging, setDragging] = useState(false)
  const rootRef = useRef<HTMLDivElement | null>(null)

  // Exit animation: stay mounted for one slide-out after close.
  useEffect(() => {
    if (state.open) {
      setRendered(true)
      return
    }
    if (!rendered) return
    const timer = setTimeout(() => {
      setRendered(false)
    }, PANEL_EXIT_MS)
    return () => {
      clearTimeout(timer)
    }
  }, [state.open, rendered])

  // Opening refreshes what the tree renders from.
  useEffect(() => {
    if (!state.open) return
    void refreshStatus(dispatch, api)
  }, [state.open, dispatch, api])
  useEffect(() => {
    if (!state.open || state.connection.phase !== 'ok') return
    if (!state.graphStale || state.graphLoading) return
    void refreshGraph(dispatch, api)
  }, [state.open, state.connection.phase, state.graphStale, state.graphLoading, dispatch, api])

  // Focus rides the view so Esc walks back without a click first. Keyed on
  // `rendered` too: on the opening pass the column is not in the DOM yet
  // (the mount effect above flips `rendered` in the same commit), so an
  // effect keyed on `open` alone focuses nothing and the first Esc goes to
  // DSH instead of closing the panel.
  const view: View = state.stack[state.stack.length - 1] ?? { kind: 'tree' }
  useEffect(() => {
    if (state.open && rendered && view.kind !== 'chat') rootRef.current?.focus()
  }, [state.open, rendered, view.kind])

  if (!rendered) return null

  const node = view.kind === 'chat' ? findNode(state.graph, view.sid) : undefined
  const chatActivity = view.kind === 'chat' ? (state.chats[view.sid]?.activity ?? node?.activity) : undefined
  const title = view.kind === 'chat'
    ? (node?.title ?? view.sid)
    : view.kind === 'spawn' ? t('spawn.title') : t('panel.title')

  return (
    <div
      ref={rootRef}
      data-ccteam-console=""
      data-closing={state.open ? undefined : ''}
      className={css.panel}
      style={{ width: state.width }}
      role="complementary"
      aria-label={t('panel.title')}
      tabIndex={-1}
      onKeyDown={(event) => {
        if (event.key !== 'Escape') return
        event.stopPropagation()
        dispatch({ type: 'back' })
      }}
    >
      <div
        className={css.dragHandle}
        data-dragging={dragging ? '' : undefined}
        role="separator"
        aria-orientation="vertical"
        aria-label={t('panel.resize')}
        onPointerDown={(event) => {
          event.preventDefault()
          event.currentTarget.setPointerCapture(event.pointerId)
          setDragging(true)
        }}
        onPointerMove={(event) => {
          if (!dragging) return
          dispatch({ type: 'set_width', width: window.innerWidth - event.clientX })
        }}
        onPointerUp={(event) => {
          event.currentTarget.releasePointerCapture(event.pointerId)
          setDragging(false)
        }}
        onPointerCancel={() => {
          setDragging(false)
        }}
      />
      <div className={css.head}>
        {view.kind === 'tree'
          ? (
              <span className={css.iconBtn} aria-hidden="true">
                <IconBranchOutline16 size={16} />
              </span>
            )
          : (
              <button
                type="button"
                className={css.iconBtn}
                aria-label={t('panel.back')}
                onClick={() => {
                  dispatch({ type: 'back' })
                }}
              >
                <IconChevronLeftOutline14 size={14} />
              </button>
            )}
        <div className={css.headTitle}>
          <span className={css.headTitleText}>{title}</span>
          {view.kind === 'chat' && node !== undefined && (
            <Pill>{node.model !== undefined ? `${node.vendor} · ${node.model}` : node.vendor}</Pill>
          )}
          {view.kind === 'chat' && (
            <StateDot className={css.headDot} state={dotState(chatActivity)} />
          )}
        </div>
        {view.kind === 'tree' && state.connection.phase === 'ok' && (
          <button
            type="button"
            className={css.iconBtn}
            aria-label={t('tree.spawn')}
            onClick={() => {
              dispatch({ type: 'open_spawn' })
            }}
          >
            <IconPlusOutline16 size={16} />
          </button>
        )}
        <button
          type="button"
          className={css.iconBtn}
          aria-label={t('panel.close')}
          onClick={() => {
            dispatch({ type: 'close_panel' })
          }}
        >
          <IconCloseOutline16 size={16} />
        </button>
      </div>
      <div className={css.body}>
        <PanelBody state={state} view={view} dispatch={dispatch} api={api} t={t} />
      </div>
    </div>
  )
}
