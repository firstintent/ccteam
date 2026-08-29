/**
 * The workbench: the `shell.overlay` entry, drawn as a pane that lives
 * BESIDE DSH rather than over it. It portals to `document.body` as a fixed
 * right-hand column and reserves its width as a body margin — DSH's frame is
 * a normal-flow block that measures itself with a ResizeObserver, so to DSH
 * a docked ccteam is exactly a narrower window: both stay live, and the
 * native sidebar / conversation / details keep working while a ccteam session
 * runs next to them. The pane is resizable (a handle on its left edge) and
 * expands to the full page on demand (⤢); one width, animated, is the whole
 * transition between the two.
 *
 * Inside, the layout follows the pane's own width: three columns (team /
 * main / details) from 1240px, two columns with a details sheet from 880px,
 * and a single column below that (the team fills the pane until a session is
 * chosen, a back control returns to it, and the details sheet slides over
 * the conversation). Esc walks layers back: text field → sheet → full →
 * docked → closed.
 * Business state arrives through the bound `useConsole` hook and leaves
 * through `dispatch`; nothing here owns state beyond transient chrome.
 */
import { useEffect, useRef, useState } from 'react'
import { createPortal } from 'react-dom'
import {
  Button,
  IconBranchOutline16,
  IconCheckOutline14,
  IconChevronLeftOutline14,
  IconCloseOutline16,
  IconCopyOutline16,
  IconFullscreenOutline16,
  IconInspectOutline12,
  IconPanelLeftOutline16,
  IconPlusOutline16,
  IconWarningOutline16,
  StateDot,
  Tooltip,
  writeClipboard,
} from '@deepseek-ai/dsh-client-ui-primitives'
import type { ApiClient } from './api.js'
import { Chat } from './Chat.js'
import type { ComposerAttachment } from './Composer.js'
import { Details } from './Details.js'
import { Hero } from './Hero.js'
import { chatOf, dotState, findNode, planSpawnOutcome } from './store.js'
import type { Action, ConsoleState } from './store.js'
import type { T, WorkbenchProps } from './slots.js'
import { TeamColumn } from './TeamColumn.js'
import css from './workbench.module.css'

/** The store's write path, as the views see it. */
export type Dispatch = (action: Action) => void

/** Width/margin transition; keep in step with .dock's transition. */
const TRANSITION_MS = 300
/** Pane widths (px) at which the inner layout gains its second and third column. */
const TIER_TWO = 880
const TIER_THREE = 1240

type Tier = 'narrow' | 'two' | 'three'

function tierOf(width: number): Tier {
  if (width >= TIER_THREE) return 'three'
  if (width >= TIER_TWO) return 'two'
  return 'narrow'
}

/**
 * Refresh connectivity into the store (boot, retry, reconnect).
 * @param dispatch - the store's write path.
 * @param api - the BFF client.
 * @returns settled promise (results land as dispatches).
 */
export async function refreshStatus(dispatch: Dispatch, api: ApiClient): Promise<void> {
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

/**
 * Load the spawn catalogs (projects + models) into the store.
 * @param dispatch - the store's write path.
 * @param api - the BFF client.
 * @returns settled promise.
 */
export async function loadCatalogs(dispatch: Dispatch, api: ApiClient): Promise<void> {
  await Promise.all([
    api.call('catalog.projects', {}).then((response) => {
      dispatch({ type: 'projects_loaded', projects: response.projects })
    }).catch(() => {
      dispatch({ type: 'projects_loaded', projects: [] })
    }),
    api.call('catalog.models', {}).then((models) => {
      dispatch({ type: 'models_loaded', models })
    }).catch(() => {
      // The model picker degrades to "vendor default" without a catalog.
    }),
  ])
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

function isTextTarget(target: EventTarget | null): boolean {
  if (!(target instanceof HTMLElement)) return false
  const tag = target.tagName
  return tag === 'TEXTAREA' || tag === 'INPUT' || target.isContentEditable
}

/**
 * Render the workbench pane (or nothing while closed).
 * @param props - the injected face (bound `useConsole`, dispatch, api) + the locale seat.
 * @returns a portal into document.body.
 */
export function Workbench({ useConsole, dispatch, api, t }: WorkbenchProps) {
  const state = useConsole(snapshot => snapshot)
  const rootRef = useRef<HTMLDivElement | null>(null)
  const [host] = useState(() => (typeof document === 'undefined' ? null : document.createElement('div')))
  const [rendered, setRendered] = useState(state.open)
  const [paneWidth, setPaneWidth] = useState(0)
  const [dragging, setDragging] = useState(false)
  const [now, setNow] = useState(() => Date.now())
  const [heroAttachments, setHeroAttachments] = useState<ComposerAttachment[]>([])

  // The pane lives at body level, beside DSH's frame rather than inside it.
  useEffect(() => {
    if (host === null) return
    host.setAttribute('data-ccteam-dock', '')
    document.body.appendChild(host)
    return () => {
      host.remove()
    }
  }, [host])

  // Exit animation: stay mounted for one width transition after close.
  useEffect(() => {
    if (state.open) {
      setRendered(true)
      return
    }
    if (!rendered) return
    const timer = setTimeout(() => {
      setRendered(false)
    }, TRANSITION_MS)
    return () => {
      clearTimeout(timer)
    }
  }, [state.open, rendered])

  // Reserve the pane's width as a body margin: DSH's frame (a normal-flow
  // block measuring itself) sees exactly a narrower window and re-lays out —
  // no DSH DOM is touched. Full mode leaves the frame untouched underneath.
  useEffect(() => {
    if (host === null || !rendered) return
    const body = document.body
    const previous = { margin: body.style.marginRight, transition: body.style.transition }
    body.style.transition = `margin-right ${TRANSITION_MS}ms cubic-bezier(.4,0,.2,1)`
    return () => {
      body.style.marginRight = previous.margin
      body.style.transition = previous.transition
    }
  }, [host, rendered])
  const mode = state.layout.mode
  const dockWidth = state.layout.dockWidth
  useEffect(() => {
    if (host === null || !rendered) return
    document.body.style.marginRight = !state.open || mode === 'full' ? '0px' : `${dockWidth}px`
  }, [host, rendered, state.open, mode, dockWidth])

  // The inner layout follows the pane's own width.
  useEffect(() => {
    const el = rootRef.current
    if (el === null || !rendered) return
    const observer = new ResizeObserver(() => {
      const width = el.getBoundingClientRect().width
      if (width > 0) setPaneWidth(width)
    })
    observer.observe(el)
    return () => {
      observer.disconnect()
    }
  }, [rendered])

  // Opening refreshes what the columns render from.
  useEffect(() => {
    if (!state.open) return
    void refreshStatus(dispatch, api)
  }, [state.open, dispatch, api])
  useEffect(() => {
    if (!state.open || state.connection.phase !== 'ok') return
    if (!state.graphStale || state.graphLoading) return
    void refreshGraph(dispatch, api)
  }, [state.open, state.connection.phase, state.graphStale, state.graphLoading, dispatch, api])
  useEffect(() => {
    if (!state.open || state.connection.phase !== 'ok') return
    if (state.catalogs.projects !== null) return
    void loadCatalogs(dispatch, api)
  }, [state.open, state.connection.phase, state.catalogs.projects, dispatch, api])
  const draftProject = state.spawn.draft.project
  useEffect(() => {
    if (!state.open || state.connection.phase !== 'ok' || draftProject === null) return
    if (state.catalogs.roles[draftProject] !== undefined) return
    api
      .call('catalog.roles', { project: draftProject })
      .then((response) => {
        dispatch({ type: 'roles_loaded', project: draftProject, roles: response.roles })
      })
      .catch(() => {
        dispatch({ type: 'roles_loaded', project: draftProject, roles: [] })
      })
  }, [state.open, state.connection.phase, draftProject, state.catalogs.roles, dispatch, api])

  // Default the draft's project/vendor once the catalogs are known.
  const projects = state.catalogs.projects
  useEffect(() => {
    if (projects === null || projects.length === 0) return
    if (draftProject !== null && projects.some(p => p.slug === draftProject)) return
    dispatch({ type: 'set_draft', draft: { project: projects[0]!.slug } })
  }, [projects, draftProject, dispatch])
  const vendors = state.connection.vendors
  const draftVendor = state.spawn.draft.vendor
  useEffect(() => {
    if (draftVendor !== null && (vendors.length === 0 || vendors.some(v => v.vendor === draftVendor && v.installed))) return
    const first = vendors.find(v => v.installed)?.vendor
    if (first !== undefined) dispatch({ type: 'set_draft', draft: { vendor: first } })
  }, [vendors, draftVendor, dispatch])

  // The clock behind relative times and the working timer.
  useEffect(() => {
    if (!state.open) return
    const timer = setInterval(() => {
      setNow(Date.now())
    }, 15_000)
    setNow(Date.now())
    return () => {
      clearInterval(timer)
    }
  }, [state.open])

  useEffect(() => {
    if (state.open && rendered && state.selection.kind !== 'session') rootRef.current?.focus()
  }, [state.open, rendered, state.selection.kind])

  if (host === null || !rendered) return null

  const full = mode === 'full'
  const viewport = typeof window === 'undefined' ? 1440 : window.innerWidth
  const tier = tierOf(paneWidth > 0 ? paneWidth : full ? viewport : dockWidth)
  const selectedSid = state.selection.kind === 'session' ? state.selection.sid : null
  const node = selectedSid === null ? undefined : findNode(state.graph, selectedSid)
  const chat = selectedSid === null ? undefined : chatOf(state, selectedSid)
  const chatActivity = selectedSid === null ? undefined : (state.chats[selectedSid]?.activity ?? node?.activity)
  const liveActivity: Record<string, ConsoleState['chats'][string]['activity']> = {}
  for (const [sid, entry] of Object.entries(state.chats)) if (entry.activity !== undefined) liveActivity[sid] = entry.activity
  const selectedStep = state.details.step !== null && state.details.step.sid === selectedSid ? state.details.step.itemId : null
  const step = selectedStep === null || chat === undefined
    ? undefined
    : chat.live?.steps.find(s => s.itemId === selectedStep)
      ?? chat.rows.flatMap(r => (r.kind === 'assistant' ? r.steps : [])).find(s => s.itemId === selectedStep)
  const connected = state.connection.phase === 'ok'
  const narrow = tier === 'narrow'
  const teamFillsPane = narrow && state.selection.kind === 'none'
  const teamColumnShown = !narrow && state.teamOpen
  const detailsAsColumn = tier === 'three'

  const attach = (files: File[]): void => {
    const project = state.spawn.draft.project
    if (project === null) return
    for (const file of files) {
      const key = `${Date.now()}-${file.name}-${Math.random().toString(36).slice(2, 8)}`
      const kind: 'image' | 'file' = file.type.startsWith('image/') ? 'image' : 'file'
      setHeroAttachments(previous => [...previous, { key, name: file.name, kind, uploading: true }])
      api
        .upload(project, { name: file.name, type: file.type, body: file })
        .then((response) => {
          setHeroAttachments(previous => previous.map(a => a.key !== key
            ? a
            : response.ok && response.attachment !== undefined
              ? { key, name: response.attachment.name, kind: response.attachment.kind === 'image' ? 'image' : 'file', path: response.attachment.path }
              : { ...a, uploading: false, error: response.error ?? 'upload failed' }))
        })
        .catch((error: unknown) => {
          setHeroAttachments(previous => previous.map(a => a.key !== key ? a : { ...a, uploading: false, error: error instanceof Error ? error.message : String(error) }))
        })
    }
  }

  const team = (fill: boolean) => (
    <TeamColumn
      graph={state.graph}
      graphError={state.graphError}
      filter={state.filter}
      collapsed={state.collapsed}
      selectedSid={selectedSid}
      liveActivity={liveActivity}
      canSpawn={connected}
      fill={fill}
      now={now}
      t={t}
      onSelect={(sid) => {
        dispatch({ type: 'select_session', sid })
      }}
      onNew={() => {
        dispatch({ type: 'select_new' })
      }}
      onToggleProject={(slug) => {
        dispatch({ type: 'toggle_project', slug })
      }}
      onProjectAction={(action, slug) => {
        switch (action) {
          case 'new':
            dispatch({ type: 'set_draft', draft: { project: slug } })
            dispatch({ type: 'select_new' })
            return
          case 'copy':
            writeClipboard(slug)
            return
          case 'solo':
            dispatch({ type: 'expand_only', slug })
            return
          case 'collapseAll':
            dispatch({ type: 'collapse_all' })
        }
      }}
      onFilter={(filter) => {
        dispatch({ type: 'set_filter', filter })
      }}
      onRetry={() => {
        void refreshGraph(dispatch, api)
      }}
      onRename={(sid, title) => {
        api
          .call('session.rename', { sid, title })
          .then((receipt) => {
            if (!receipt.ok) dispatch({ type: 'notice', sid, kind: 'error', message: receipt.error ?? receipt.errorKind ?? 'rename failed' })
            void refreshGraph(dispatch, api)
          })
          .catch((error: unknown) => {
            dispatch({ type: 'notice', sid, kind: 'error', message: error instanceof Error ? error.message : String(error) })
          })
      }}
      onCopySid={(sid) => {
        writeClipboard(sid)
      }}
      onInterrupt={(sid) => {
        api
          .call('session.interrupt', { sid })
          .then((receipt) => {
            if (!receipt.ok) dispatch({ type: 'notice', sid, kind: 'error', message: receipt.error ?? receipt.errorKind ?? 'interrupt failed' })
          })
          .catch((error: unknown) => {
            dispatch({ type: 'notice', sid, kind: 'error', message: error instanceof Error ? error.message : String(error) })
          })
      }}
      onStop={(sid) => {
        api
          .call('session.stop', { sid })
          .then((receipt) => {
            if (!receipt.ok) dispatch({ type: 'notice', sid, kind: 'error', message: receipt.error ?? receipt.errorKind ?? 'stop failed' })
            void refreshGraph(dispatch, api)
          })
          .catch((error: unknown) => {
            dispatch({ type: 'notice', sid, kind: 'error', message: error instanceof Error ? error.message : String(error) })
          })
      }}
      onDetails={(sid) => {
        dispatch({ type: 'select_session', sid })
        dispatch({ type: 'open_details' })
      }}
    />
  )

  const details = (
    <Details
      node={node}
      chat={selectedSid === null ? undefined : state.chats[selectedSid]}
      graph={state.graph}
      step={step}
      now={now}
      api={api}
      dispatch={dispatch}
      t={t}
      onClose={() => {
        dispatch({ type: 'close_details' })
      }}
      onSelectSession={(sid) => {
        dispatch({ type: 'select_session', sid })
      }}
      onRefreshGraph={() => {
        void refreshGraph(dispatch, api)
      }}
    />
  )

  const mainContent = !connected
    ? <StateScreen state={state} dispatch={dispatch} api={api} t={t} />
    : selectedSid !== null && chat !== undefined
      ? (
          <Chat
            key={selectedSid}
            sid={selectedSid}
            project={node?.project}
            chat={chat}
            node={node}
            models={state.catalogs.models}
            selectedStep={selectedStep}
            api={api}
            dispatch={dispatch}
            t={t}
            onSelectStep={(itemId) => {
              dispatch({ type: 'select_step', sid: selectedSid, itemId })
            }}
          />
        )
      : (
          <Hero
            t={t}
            draft={state.spawn.draft}
            projects={state.catalogs.projects}
            vendors={state.connection.vendors}
            models={state.catalogs.models}
            roles={draftProject === null ? [] : state.catalogs.roles[draftProject] ?? []}
            busy={state.spawn.busy}
            error={state.spawn.error}
            attachments={heroAttachments}
            onDraft={(draft) => {
              dispatch({ type: 'set_draft', draft })
            }}
            onAttach={attach}
            onRemoveAttachment={(key) => {
              setHeroAttachments(previous => previous.filter(a => a.key !== key))
            }}
            onCreate={(request) => {
              dispatch({ type: 'spawn_started' })
              setHeroAttachments([])
              api
                .call('session.spawn', request)
                .then((response) => {
                  const outcome = planSpawnOutcome(response)
                  if (outcome.kind === 'form_error') {
                    dispatch({ type: 'spawn_failed', message: outcome.message })
                    return
                  }
                  // A sid exists upstream even when the first task failed:
                  // enter the session and state the failure inside its chat.
                  dispatch({ type: 'spawn_done' })
                  dispatch({ type: 'select_session', sid: outcome.sid })
                  if (outcome.errorMessage !== undefined) {
                    dispatch({ type: 'send_failed', sid: outcome.sid, message: outcome.errorMessage })
                  }
                  void refreshGraph(dispatch, api)
                })
                .catch((error: unknown) => {
                  dispatch({ type: 'spawn_failed', message: error instanceof Error ? error.message : String(error) })
                })
            }}
          />
        )

  const pane = (
    <div
      ref={rootRef}
      data-ccteam-console=""
      data-mode={mode}
      data-tier={tier}
      data-closing={state.open ? undefined : ''}
      data-dragging={dragging ? '' : undefined}
      className={css.dock}
      style={{ width: full ? '100vw' : dockWidth }}
      role="region"
      aria-label={t('panel.title')}
      tabIndex={-1}
      onKeyDown={(event) => {
        if (event.key !== 'Escape' || event.defaultPrevented) return
        if (isTextTarget(event.target)) {
          // Esc in a text field only leaves the field.
          ;(event.target as HTMLElement).blur()
          event.stopPropagation()
          return
        }
        // An open menu (row / workspace / model picker) owns this Esc: DSH's
        // Menu closes itself from a document-level listener without
        // preventDefault, so the pane must step aside — and must NOT stop
        // propagation, or that listener never sees the key.
        if (event.currentTarget.querySelector('[aria-haspopup="menu"][aria-expanded="true"]') !== null) return
        event.stopPropagation()
        if (state.details.open) {
          dispatch({ type: 'close_details' })
          return
        }
        if (full) {
          dispatch({ type: 'set_mode', mode: 'docked' })
          return
        }
        dispatch({ type: 'close_panel' })
      }}
    >
      {!full && (
        <div
          className={css.dockHandle}
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
            dispatch({ type: 'set_dock_width', width: window.innerWidth - event.clientX, viewport: window.innerWidth })
          }}
          onPointerUp={(event) => {
            event.currentTarget.releasePointerCapture(event.pointerId)
            setDragging(false)
          }}
          onPointerCancel={() => {
            setDragging(false)
          }}
        />
      )}
      <div className={css.header}>
        {narrow && state.selection.kind !== 'none'
          ? (
              <Tooltip label={t('panel.backToTeam')} delayMs={400}>
                <button
                  type="button"
                  className={css.iconBtn}
                  aria-label={t('panel.backToTeam')}
                  onClick={() => {
                    dispatch({ type: 'clear_selection' })
                  }}
                >
                  <IconChevronLeftOutline14 size={14} />
                </button>
              </Tooltip>
            )
          : (
              <Tooltip label={t('panel.team')} delayMs={400}>
                <button
                  type="button"
                  className={css.iconBtn}
                  aria-label={t('panel.team')}
                  aria-pressed={state.teamOpen}
                  disabled={narrow}
                  onClick={() => {
                    dispatch({ type: 'toggle_team' })
                  }}
                >
                  <IconPanelLeftOutline16 size={16} />
                </button>
              </Tooltip>
            )}
        <span className={css.brand}>
          <IconBranchOutline16 size={16} />
          {t('panel.title')}
          <Tooltip label={connected ? t('panel.connected') : t('panel.disconnected')} delayMs={300}>
            <span className={css.brandDot}>
              <StateDot state={connected ? 'done' : state.connection.phase === 'checking' ? 'ongoing' : 'error'} size={8} />
            </span>
          </Tooltip>
        </span>
        <div className={css.crumbs}>
          {node !== undefined && (
            <>
              <span className={css.crumbSep}>/</span>
              {!narrow && (
                <>
                  <span className={css.crumb}>{node.project}</span>
                  <span className={css.crumbSep}>/</span>
                </>
              )}
              <span className={`${css.crumb} ${css.crumbTitle}`}>{node.title ?? node.sid}</span>
              <StateDot state={dotState(chatActivity)} size={8} />
            </>
          )}
          {state.selection.kind === 'new' && (
            <>
              <span className={css.crumbSep}>/</span>
              <span className={`${css.crumb} ${css.crumbTitle}`}>{t('hero.title')}</span>
            </>
          )}
        </div>
        <div className={css.headerActions}>
          <Tooltip label={t('panel.new')} delayMs={400}>
            <button
              type="button"
              className={css.iconBtn}
              aria-label={t('panel.new')}
              disabled={!connected}
              onClick={() => {
                dispatch({ type: 'select_new' })
              }}
            >
              <IconPlusOutline16 size={16} />
            </button>
          </Tooltip>
          <Tooltip label={t('panel.details')} delayMs={400}>
            <button
              type="button"
              className={css.iconBtn}
              aria-label={t('panel.details')}
              aria-pressed={state.details.open}
              onClick={() => {
                dispatch({ type: 'toggle_details' })
              }}
            >
              <IconInspectOutline12 size={14} />
            </button>
          </Tooltip>
          <Tooltip label={full ? t('panel.dock') : t('panel.expand')} delayMs={400}>
            <button
              type="button"
              className={css.iconBtn}
              aria-label={full ? t('panel.dock') : t('panel.expand')}
              aria-pressed={full}
              onClick={() => {
                dispatch({ type: 'toggle_mode' })
              }}
            >
              <IconFullscreenOutline16 size={16} />
            </button>
          </Tooltip>
          <Tooltip label={`${t('panel.close')} · Esc`} delayMs={400}>
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
          </Tooltip>
        </div>
      </div>
      <div className={css.columns}>
        {teamColumnShown && team(false)}
        {teamFillsPane
          ? team(true)
          : (
              <main className={css.main}>
                {mainContent}
                {!detailsAsColumn && state.details.open && (
                  <div className={css.sheet}>{details}</div>
                )}
              </main>
            )}
        {detailsAsColumn && state.details.open && !teamFillsPane && details}
      </div>
    </div>
  )

  return createPortal(pane, host)
}
