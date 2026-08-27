/**
 * The workbench: the `shell.overlay` entry, drawn as a whole-viewport page
 * section of DSH (not a popup) — a 52px header, then three columns: the team
 * tree, the main column (a session's conversation, or the new-session hero),
 * and the details column. Esc walks layers back (details → workbench) and
 * finally returns to DSH. Every not-happy state names its next action:
 * unreachable → copyable `ccteam start` + retry; unconfigured → the
 * Settings pointer. Business state arrives through the bound `useConsole`
 * hook and leaves through `dispatch`; nothing here owns state beyond the
 * hero's pending uploads and the clock.
 */
import { useEffect, useRef, useState } from 'react'
import {
  Button,
  IconBranchOutline16,
  IconCheckOutline14,
  IconCloseOutline16,
  IconCopyOutline16,
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
 * Render the workbench (or nothing while closed).
 * @param props - the injected face (bound `useConsole`, dispatch, api) + the locale seat.
 * @returns the workbench element tree.
 */
export function Workbench({ useConsole, dispatch, api, t }: WorkbenchProps) {
  const state = useConsole(snapshot => snapshot)
  const rootRef = useRef<HTMLDivElement | null>(null)
  const [now, setNow] = useState(() => Date.now())
  const [heroAttachments, setHeroAttachments] = useState<ComposerAttachment[]>([])

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
    if (state.open && state.selection.kind !== 'session') rootRef.current?.focus()
  }, [state.open, state.selection.kind])

  if (!state.open) return null

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

  return (
    <div
      ref={rootRef}
      data-ccteam-console=""
      className={css.workbench}
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
        event.stopPropagation()
        if (state.details.open) {
          dispatch({ type: 'close_details' })
          return
        }
        dispatch({ type: 'close_panel' })
      }}
    >
      <div className={css.header}>
        <Tooltip label={state.teamOpen ? t('panel.team') : t('panel.team')} delayMs={400}>
          <button
            type="button"
            className={css.iconBtn}
            aria-label={t('panel.team')}
            aria-pressed={state.teamOpen}
            onClick={() => {
              dispatch({ type: 'toggle_team' })
            }}
          >
            <IconPanelLeftOutline16 size={16} />
          </button>
        </Tooltip>
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
              <span className={css.crumb}>{node.project}</span>
              <span className={css.crumbSep}>/</span>
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
        {state.teamOpen && (
          <TeamColumn
            graph={state.graph}
            graphError={state.graphError}
            filter={state.filter}
            collapsed={state.collapsed}
            selectedSid={selectedSid}
            liveActivity={liveActivity}
            canSpawn={connected}
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
            onFilter={(filter) => {
              dispatch({ type: 'set_filter', filter })
            }}
            onRetry={() => {
              void refreshGraph(dispatch, api)
            }}
          />
        )}
        <main className={css.main}>
          {!connected
            ? <StateScreen state={state} dispatch={dispatch} api={api} t={t} />
            : selectedSid !== null && chat !== undefined
              ? (
                  <Chat
                    key={selectedSid}
                    sid={selectedSid}
                    project={node?.project}
                    chat={chat}
                    node={node}
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
                          // A sid exists upstream even when the first task
                          // failed: enter the session and state the failure
                          // inside its chat.
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
                )}
        </main>
        {state.details.open && (
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
        )}
      </div>
    </div>
  )
}
