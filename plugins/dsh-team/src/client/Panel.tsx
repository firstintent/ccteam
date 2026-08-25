/**
 * The console column: a right-side overlay (floats over the 3-column layout,
 * never squeezes it) sliding in on the deepsuite sider curve, width-draggable
 * with AppFrame's handle chrome, one header, three views (tree / chat /
 * spawn), Esc walking back and finally out. Every not-happy state names its
 * next action: unreachable → copyable `ccteam start` + retry; unconfigured →
 * the two-step Settings pointer; empty → the spawn hero.
 */
import { useEffect, useRef, useState, useSyncExternalStore } from 'react'
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
import { dotState, findNode } from './store.js'
import type { ConsoleState, ConsoleStore, View } from './store.js'
import type { ConsoleInjected, T } from './slots.js'
import { SessionTree } from './SessionTree.js'
import { SessionChat } from './SessionChat.js'
import { SpawnForm } from './SpawnForm.js'
import css from './panel.module.css'

/** Exit-animation settle; keep in step with --ds-transition-duration-slow. */
const PANEL_EXIT_MS = 320

/**
 * Refresh connectivity into the store (boot, retry, reconnect).
 * @param store - the console store.
 * @param api - the BFF client.
 * @returns settled promise (results land as dispatches).
 */
export async function refreshStatus(store: ConsoleStore, api: ApiClient): Promise<void> {
  store.dispatch({ type: 'status_loading' })
  try {
    const status = await api.call('status', {})
    store.dispatch({ type: 'status_loaded', status })
  } catch {
    // The failure IS the result: the store's phase turns unreachable.
    store.dispatch({ type: 'status_failed' })
  }
}

/**
 * Refresh the team graph into the store.
 * @param store - the console store.
 * @param api - the BFF client.
 * @returns settled promise (results land as dispatches).
 */
export async function refreshGraph(store: ConsoleStore, api: ApiClient): Promise<void> {
  store.dispatch({ type: 'graph_loading' })
  try {
    const graph = await api.call('team.graph', {})
    store.dispatch({ type: 'graph_loaded', graph })
  } catch (error) {
    store.dispatch({ type: 'graph_failed', message: error instanceof Error ? error.message : String(error) })
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

function StateScreen({ state, store, api, t }: { state: ConsoleState; store: ConsoleStore; api: ApiClient; t: T }) {
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
              void refreshStatus(store, api)
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
            void refreshStatus(store, api)
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

function PanelBody({ state, view, store, api, t }: {
  state: ConsoleState
  view: View
  store: ConsoleStore
  api: ApiClient
  t: T
}) {
  if (view.kind === 'chat') {
    return (
      <SessionChat
        sid={view.sid}
        chat={state.chats[view.sid] ?? { rows: [], activity: undefined, loading: false, error: null, notices: [] }}
        node={findNode(state.graph, view.sid)}
        store={store}
        api={api}
        t={t}
      />
    )
  }
  if (view.kind === 'spawn') {
    return (
      <SpawnForm
        vendors={state.connection.vendors}
        busy={state.spawn.busy}
        error={state.spawn.error}
        t={t}
        onCreate={(request) => {
          store.dispatch({ type: 'spawn_started' })
          api
            .call('session.spawn', request)
            .then((response) => {
              if (response.ok && response.sid !== undefined) {
                store.dispatch({ type: 'spawn_done' })
                store.dispatch({ type: 'open_chat', sid: response.sid })
                void refreshGraph(store, api)
              } else {
                store.dispatch({ type: 'spawn_failed', message: response.error ?? 'unknown' })
              }
            })
            .catch((error: unknown) => {
              store.dispatch({
                type: 'spawn_failed',
                message: error instanceof Error ? error.message : String(error),
              })
            })
        }}
      />
    )
  }
  if (state.connection.phase !== 'ok') return <StateScreen state={state} store={store} api={api} t={t} />
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
              void refreshGraph(store, api)
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
          store.dispatch({ type: 'open_spawn' })
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
        store.dispatch({ type: 'open_chat', sid })
      }}
      onToggleProject={(slug) => {
        store.dispatch({ type: 'toggle_project', slug })
      }}
    />
  )
}

/** Composed slot props of the `shell.overlay` entry. */
export type PanelProps = ConsoleInjected & { t: T }

/**
 * Render the console column (or nothing while closed).
 * @param props - injected store/api + the locale seat.
 * @returns the panel element tree.
 */
export function Panel({ store, api, t }: PanelProps) {
  const state = useSyncExternalStore(store.subscribe, store.getState, store.getState)
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
    void refreshStatus(store, api)
  }, [state.open, store, api])
  useEffect(() => {
    if (!state.open || state.connection.phase !== 'ok') return
    if (!state.graphStale || state.graphLoading) return
    void refreshGraph(store, api)
  }, [state.open, state.connection.phase, state.graphStale, state.graphLoading, store, api])

  // Focus rides the view so Esc walks back without a click first.
  const view: View = state.stack[state.stack.length - 1] ?? { kind: 'tree' }
  useEffect(() => {
    if (state.open && view.kind !== 'chat') rootRef.current?.focus()
  }, [state.open, view.kind])

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
        store.dispatch({ type: 'back' })
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
          store.dispatch({ type: 'set_width', width: window.innerWidth - event.clientX })
        }}
        onPointerUp={(event) => {
          event.currentTarget.releasePointerCapture(event.pointerId)
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
                  store.dispatch({ type: 'back' })
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
              store.dispatch({ type: 'open_spawn' })
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
            store.dispatch({ type: 'close_panel' })
          }}
        >
          <IconCloseOutline16 size={16} />
        </button>
      </div>
      <div className={css.body}>
        <PanelBody state={state} view={view} store={store} api={api} t={t} />
      </div>
    </div>
  )
}
