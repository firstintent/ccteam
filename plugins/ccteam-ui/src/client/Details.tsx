/**
 * The details column (DSH's DetailsPanel geometry): the selected session's
 * identity, state, usage (with the context-window meter from the statusline),
 * delegation links, and actions (rename, interrupt, stop, copy sid). When a
 * step row is selected in the chat it shows that step. Presentation +
 * the small action calls; state lives in the store.
 */
import { useEffect, useState } from 'react'
import {
  Button,
  IconCheckOutline14,
  IconCloseOutline16,
  IconCopyOutline16,
  Input,
  StateDot,
  writeClipboard,
} from '@deepseek-ai/dsh-client-ui-primitives'
import type { Step, TeamGraph, TeamNode } from '../shared/contract.js'
import type { ApiClient } from './api.js'
import { formatCost, formatTokens } from './format.js'
import { dotState, findNode } from './store.js'
import type { Action, ChatState } from './store.js'
import { whenText } from './TeamColumn.js'
import type { T } from './slots.js'
import css from './workbench.module.css'

export interface DetailsProps {
  node: TeamNode | undefined
  chat: ChatState | undefined
  graph: TeamGraph | null
  step: Step | undefined
  now: number
  api: ApiClient
  dispatch(action: Action): void
  t: T
  onClose(): void
  onSelectSession(sid: string): void
  onRefreshGraph(): void
}

function Kv({ k, v, mono }: { k: string; v: string | null | undefined; mono?: boolean }) {
  if (v === undefined || v === null || v === '') return null
  return (
    <>
      <span className={css.k}>{k}</span>
      <span className={mono === true ? `${css.v} ${css.mono}` : css.v}>{v}</span>
    </>
  )
}

/**
 * Render the details column.
 * @param props - the selected node/chat/step and actions.
 * @returns the column.
 */
export function Details(props: DetailsProps) {
  const { node, chat, graph, step, now, api, dispatch, t } = props
  const [renaming, setRenaming] = useState(false)
  const [title, setTitle] = useState('')
  const [confirmStop, setConfirmStop] = useState(false)
  const [copied, setCopied] = useState(false)
  const [actionError, setActionError] = useState<string | null>(null)

  useEffect(() => {
    setRenaming(false)
    setConfirmStop(false)
    setActionError(null)
    setTitle(node?.title ?? '')
  }, [node?.sid, node?.title])

  useEffect(() => {
    if (!confirmStop) return
    const timer = setTimeout(() => {
      setConfirmStop(false)
    }, 4000)
    return () => {
      clearTimeout(timer)
    }
  }, [confirmStop])

  const parent = node?.parentSid === undefined ? undefined : findNode(graph, node.parentSid)
  const activity = chat?.activity ?? node?.activity
  const working = activity === 'working'
  const status = chat?.status ?? null
  const pct = status?.context?.pct

  const rename = (): void => {
    if (node === undefined) return
    const next = title.trim()
    if (next === '' || next === node.title) {
      setRenaming(false)
      return
    }
    api
      .call('session.rename', { sid: node.sid, title: next })
      .then((receipt) => {
        if (!receipt.ok) {
          setActionError(t('details.rename.failed', { kind: receipt.error ?? receipt.errorKind ?? 'unknown' }))
          return
        }
        setRenaming(false)
        props.onRefreshGraph()
      })
      .catch((error: unknown) => {
        setActionError(t('details.rename.failed', { kind: error instanceof Error ? error.message : String(error) }))
      })
  }

  const interrupt = (): void => {
    if (node === undefined) return
    api
      .call('session.interrupt', { sid: node.sid })
      .then((receipt) => {
        if (!receipt.ok) setActionError(t('details.interrupt.failed', { kind: receipt.error ?? receipt.errorKind ?? 'unknown' }))
      })
      .catch((error: unknown) => {
        setActionError(t('details.interrupt.failed', { kind: error instanceof Error ? error.message : String(error) }))
      })
  }

  const stop = (): void => {
    if (node === undefined) return
    if (!confirmStop) {
      setConfirmStop(true)
      return
    }
    setConfirmStop(false)
    api
      .call('session.stop', { sid: node.sid })
      .then((receipt) => {
        if (!receipt.ok) {
          setActionError(t('details.stop.failed', { kind: receipt.error ?? receipt.errorKind ?? 'unknown' }))
          return
        }
        dispatch({ type: 'graph_stale' })
        props.onRefreshGraph()
      })
      .catch((error: unknown) => {
        setActionError(t('details.stop.failed', { kind: error instanceof Error ? error.message : String(error) }))
      })
  }

  return (
    <aside className={css.details} aria-label={t('details.title')}>
      <div className={css.detailsHead}>
        <span>{step !== undefined ? t('details.step') : t('details.title')}</span>
        <button type="button" className={css.iconBtn} aria-label={t('panel.close')} onClick={props.onClose}>
          <IconCloseOutline16 size={16} />
        </button>
      </div>
      <div className={css.detailsBody}>
        {step !== undefined && (
          <div className={css.section}>
            <div className={css.kv}>
              <Kv k={t('details.step.kind')} v={step.kind} mono />
              <Kv k={t('details.step.name')} v={step.name} mono />
              <Kv k={t('details.step.status')} v={step.status === 'completed' ? t('chat.step.done') : t('chat.step.running')} />
            </div>
            <div className={css.sectionTitle}>{t('details.step.summary')}</div>
            <div className={css.stepDetailSummary}>{step.summary}</div>
            <div className={css.detailsHint}>{t('details.step.hint')}</div>
          </div>
        )}
        {node === undefined
          ? <div className={css.centerNote}>{t('details.none')}</div>
          : (
              <>
                <div className={css.section}>
                  <div className={css.sectionTitle}>{t('details.identity')}</div>
                  <div className={css.kv}>
                    <Kv k={t('details.sid')} v={node.sid} mono />
                    <Kv k={t('details.project')} v={node.project} mono />
                    <Kv k={t('details.vendor')} v={node.vendor} />
                    <Kv k={t('details.model')} v={status?.model ?? node.model} mono />
                    <Kv k={t('details.effort')} v={status?.effort ?? node.effort} />
                    <Kv k={t('details.role')} v={node.role} />
                    <Kv k={t('details.host')} v={node.host} />
                  </div>
                </div>
                <div className={css.section}>
                  <div className={css.sectionTitle}>{t('details.state')}</div>
                  <div className={css.kv}>
                    <span className={css.k}>{t('details.state')}</span>
                    <span className={css.v}>
                      <StateDot state={dotState(activity)} size={8} />
                      {' '}
                      {working ? t('chat.working') : (activity ?? 'idle')}
                    </span>
                    <Kv k={t('details.lastActive')} v={whenText(t, node.lastActive, now)} />
                    <Kv k={t('details.turns')} v={node.turnCount === undefined ? undefined : String(node.turnCount)} />
                  </div>
                </div>
                <div className={css.section}>
                  <div className={css.sectionTitle}>{t('details.usage')}</div>
                  <div className={css.kv}>
                    <Kv k={t('details.cost')} v={formatCost(node.costUsd)} />
                    <Kv k={t('details.tokens')} v={formatTokens(node.tokensTotal)} />
                    <Kv
                      k={t('details.context')}
                      v={status?.context?.usedTokens === undefined
                        ? undefined
                        : `${formatTokens(status.context.usedTokens) ?? ''}${status.context.windowTokens === undefined ? '' : ` / ${formatTokens(status.context.windowTokens) ?? ''}`}${pct === undefined ? '' : ` (${Math.round(pct)}%)`}`}
                    />
                  </div>
                  {pct !== undefined && (
                    <div className={css.meter} aria-hidden="true">
                      <div className={css.meterFill} style={{ width: `${Math.min(100, Math.max(0, pct))}%` }} />
                    </div>
                  )}
                </div>
                {(parent !== undefined || node.children.length > 0) && (
                  <div className={css.section}>
                    <div className={css.sectionTitle}>{t('details.delegation')}</div>
                    <div className={css.kv}>
                      {parent !== undefined && (
                        <>
                          <span className={css.k}>{t('details.parent')}</span>
                          <button
                            type="button"
                            className={css.linkBtn}
                            onClick={() => {
                              props.onSelectSession(parent.sid)
                            }}
                          >
                            {parent.title ?? parent.sid}
                          </button>
                        </>
                      )}
                      {node.children.length > 0 && (
                        <>
                          <span className={css.k}>{t('details.children')}</span>
                          <span className={css.v}>
                            {node.children.map(child => (
                              <button
                                key={child.sid}
                                type="button"
                                className={css.linkBtn}
                                style={{ display: 'block' }}
                                onClick={() => {
                                  props.onSelectSession(child.sid)
                                }}
                              >
                                {child.title ?? child.sid}
                              </button>
                            ))}
                          </span>
                        </>
                      )}
                    </div>
                  </div>
                )}
                <div className={css.section}>
                  <div className={css.sectionTitle}>{t('details.actions')}</div>
                  {renaming
                    ? (
                        <div className={css.renameRow}>
                          <Input
                            className={css.renameInput}
                            value={title}
                            placeholder={t('details.rename.placeholder')}
                            autoFocus
                            onChange={event => setTitle(event.currentTarget.value)}
                            onKeyDown={(event) => {
                              if (event.key === 'Enter') rename()
                              if (event.key === 'Escape') {
                                event.stopPropagation()
                                setRenaming(false)
                              }
                            }}
                          />
                          <Button variant="primary" size="sm" onClick={rename}>{t('details.rename.save')}</Button>
                        </div>
                      )
                    : (
                        <div className={css.actions}>
                          <Button
                            variant="outline"
                            size="sm"
                            onClick={() => {
                              setRenaming(true)
                            }}
                          >
                            {t('details.rename')}
                          </Button>
                          {working && (
                            <Button variant="outline" size="sm" onClick={interrupt}>{t('details.interrupt')}</Button>
                          )}
                          <Button variant="outline" size="sm" onClick={stop}>
                            {confirmStop ? t('details.stop.confirm') : t('details.stop')}
                          </Button>
                          <Button
                            variant="ghost"
                            size="sm"
                            icon={copied ? <IconCheckOutline14 size={14} /> : <IconCopyOutline16 size={14} />}
                            onClick={() => {
                              writeClipboard(node.sid)
                              setCopied(true)
                              setTimeout(() => {
                                setCopied(false)
                              }, 1500)
                            }}
                          >
                            {copied ? t('states.copied') : t('details.copySid')}
                          </Button>
                        </div>
                      )}
                  {actionError !== null && <div className={`${css.notice} ${css.noticeError}`} role="alert">{actionError}</div>}
                </div>
              </>
            )}
      </div>
    </aside>
  )
}
