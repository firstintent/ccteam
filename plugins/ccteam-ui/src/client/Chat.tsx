/**
 * One session's conversation: the canonical transcript (paged, oldest on
 * top), the in-flight live turn (structured steps + streaming narrative),
 * choice prompts, lifecycle/delegation notes, and the composer. Assistant
 * text renders through DSH's own `MarkdownText`; steps are compact rows that
 * open in the details column. Receipts are honest: queued renders a chip
 * naming what it waits behind, failures render an error row — never
 * swallowed. Auto-scroll stays pinned to the bottom until the user scrolls up.
 */
import { useEffect, useRef, useState } from 'react'
import clsx from 'clsx'
import {
  Button,
  IconApiOutline14,
  IconCodeOutline16,
  IconEditOutline16,
  IconGlobeOutline14,
  IconLoadingOutline16,
  IconPaperclipOutline16,
  IconSparkle16,
  IconThinkOutline16,
  MarkdownText,
  StateDot,
} from '@deepseek-ai/dsh-client-ui-primitives'
import type { AttachmentRef, ChoiceOption, Step, TeamNode } from '../shared/contract.js'
import type { ApiClient } from './api.js'
import { Composer } from './Composer.js'
import type { ComposerAttachment } from './Composer.js'
import { formatElapsed } from './format.js'
import type { Action, ChatNotice, ChatRow, ChatState } from './store.js'
import type { T } from './slots.js'
import css from './workbench.module.css'

/** Distance from the bottom (px) under which auto-scroll stays pinned. */
const PIN_THRESHOLD = 64

export type Dispatch = (action: Action) => void

export interface ChatProps {
  sid: string
  project: string | undefined
  chat: ChatState
  node: TeamNode | undefined
  selectedStep: string | null
  api: ApiClient
  dispatch: Dispatch
  t: T
  onSelectStep(itemId: string): void
}

function StepIcon({ kind }: { kind: string }) {
  switch (kind) {
    case 'command_exec':
      return <IconCodeOutline16 className={css.stepIcon} size={14} />
    case 'file_change':
      return <IconEditOutline16 className={css.stepIcon} size={14} />
    case 'web_search':
      return <IconGlobeOutline14 className={css.stepIcon} size={14} />
    case 'thinking':
      return <IconThinkOutline16 className={css.stepIcon} size={14} />
    case 'tool_call':
    case 'tool_result':
      return <IconApiOutline14 className={css.stepIcon} size={14} />
    default:
      return <IconSparkle16 className={css.stepIcon} size={14} />
  }
}

function Steps({ steps, selected, t, onSelect }: { steps: Step[]; selected: string | null; t: T; onSelect(itemId: string): void }) {
  if (steps.length === 0) return null
  return (
    <div className={css.steps} aria-label={t('chat.steps', { count: steps.length })}>
      {steps.map(step => (
        <button
          key={step.itemId}
          type="button"
          className={css.stepRow}
          aria-pressed={selected === step.itemId}
          title={step.summary}
          onClick={() => {
            onSelect(step.itemId)
          }}
        >
          <StepIcon kind={step.kind} />
          <span className={css.stepName}>{step.name === '' ? step.kind : step.name}</span>
          <span className={css.stepSummary}>{step.summary}</span>
          {step.status === 'completed'
            ? <StateDot className={css.stepDot} state="done" size={8} />
            : <IconLoadingOutline16 className={clsx(css.stepDot, css.spin)} size={12} />}
        </button>
      ))}
    </div>
  )
}

function Attachments({ attachments }: { attachments: AttachmentRef[] | undefined }) {
  if (attachments === undefined || attachments.length === 0) return null
  return (
    <div className={css.attachments}>
      {attachments.map((attachment, index) => (
        attachment.kind === 'image' && attachment.url !== undefined
          ? <img key={`${attachment.name}-${index}`} className={css.attachmentImg} src={attachment.url} alt={attachment.name} />
          : (
              <a
                key={`${attachment.name}-${index}`}
                className={css.attachmentLink}
                href={attachment.url ?? '#'}
                target={attachment.url === undefined ? undefined : '_blank'}
                rel="noreferrer"
                onClick={attachment.url === undefined ? event => event.preventDefault() : undefined}
              >
                <IconPaperclipOutline16 size={12} />
                {attachment.name}
              </a>
            )
      ))}
    </div>
  )
}

function ChoiceCard({ row, t, onResolve }: {
  row: Extract<ChatRow, { kind: 'choice' }>
  t: T
  onResolve(option: ChoiceOption): void
}) {
  const chosen = row.resolved === undefined ? undefined : row.options.find(o => o.id === row.resolved)
  return (
    <div className={css.choiceCard}>
      {row.content !== '' && <div className={css.markdown}><MarkdownText text={row.content} /></div>}
      {chosen !== undefined
        ? <div className={css.choiceResolved}>{t('chat.choice.resolved', { label: chosen.label })}</div>
        : (
            <div className={css.choiceOptions}>
              {row.options.map(option => (
                <Button
                  key={option.id}
                  variant="outline"
                  size="sm"
                  disabled={row.resolving === true}
                  onClick={() => {
                    onResolve(option)
                  }}
                >
                  {option.label}
                </Button>
              ))}
            </div>
          )}
      {row.error !== undefined && <div className={clsx(css.notice, css.noticeError)} role="alert">{t('chat.choice.failed', { kind: row.error })}</div>}
    </div>
  )
}

function NoticeRow({ notice, t }: { notice: ChatNotice; t: T }) {
  if (notice.kind === 'queued') {
    return (
      <div className={clsx(css.notice, css.noticeQueued)}>
        {notice.queuedBehind !== undefined
          ? t('chat.queued', { behind: notice.queuedBehind })
          : t('chat.queued.plain')}
      </div>
    )
  }
  if (notice.kind === 'info') {
    const message = notice.message ?? ''
    const [head, ...rest] = message.split(':')
    const title = rest[0] ?? ''
    const text = head === 'spawned'
      ? t('chat.delegation.spawned', { title })
      : head === 'failed'
        ? t('chat.delegation.failed', { title, reason: rest.slice(1).join(':') })
        : head === 'done' ? t('chat.delegation.done', { title }) : message
    return <div className={clsx(css.notice, css.noticeInfo)}>{text}</div>
  }
  return (
    <div className={clsx(css.notice, css.noticeError)} role="alert">
      {t('chat.failed', { kind: notice.errorKind ?? notice.message ?? 'unknown' })}
      {notice.errorKind !== undefined && notice.message !== undefined ? ` — ${notice.message}` : ''}
    </div>
  )
}

function Row({ row, selectedStep, t, onSelectStep, onResolve }: {
  row: ChatRow
  selectedStep: string | null
  t: T
  onSelectStep(itemId: string): void
  onResolve(row: Extract<ChatRow, { kind: 'choice' }>, option: ChoiceOption): void
}) {
  switch (row.kind) {
    case 'user':
      return (
        <div className={css.turnUser}>
          <div className={css.bubble} data-local={row.local === true ? '' : undefined}>{row.content}</div>
          <Attachments attachments={row.attachments} />
        </div>
      )
    case 'assistant':
      return (
        <div className={css.turnAssistant}>
          <Steps steps={row.steps} selected={selectedStep} t={t} onSelect={onSelectStep} />
          {row.content !== '' && <div className={css.markdown}><MarkdownText text={row.content} /></div>}
          <Attachments attachments={row.attachments} />
        </div>
      )
    case 'choice':
      return (
        <ChoiceCard
          row={row}
          t={t}
          onResolve={(option) => {
            onResolve(row, option)
          }}
        />
      )
    case 'system':
      return <div className={css.systemRow} data-tone={row.tone}>{t('chat.lifecycle', { state: row.text })}</div>
  }
}

/**
 * Render one session's conversation.
 * @param props - sid + transcript state + transports.
 * @returns the transcript + composer.
 */
export function Chat({ sid, project, chat, node, selectedStep, api, dispatch, t, onSelectStep }: ChatProps) {
  const [draft, setDraft] = useState('')
  const [attachments, setAttachments] = useState<ComposerAttachment[]>([])
  const [tick, setTick] = useState(0)
  const scrollRef = useRef<HTMLDivElement | null>(null)
  const inputRef = useRef<HTMLTextAreaElement | null>(null)
  const pinnedRef = useRef(true)

  // History + the sid-filtered live stream + the statusline, all scoped to
  // this sid's mount.
  useEffect(() => {
    let alive = true
    dispatch({ type: 'history_loading', sid })
    api
      .call('session.history', { sid })
      .then((history) => {
        if (!alive) return
        dispatch({ type: 'history_loaded', sid, rows: history.rows, hasMore: history.hasMore, ...(history.nextBefore === undefined ? {} : { nextBefore: history.nextBefore }) })
      })
      .catch(() => {
        if (!alive) return
        dispatch({ type: 'history_failed', sid, message: t('chat.history.error') })
      })
    const loadStatus = (): void => {
      api
        .call('session.status', { sid })
        .then((status) => {
          if (alive) dispatch({ type: 'session_status', sid, status })
        })
        .catch(() => {
          // The statusline is decoration; a miss leaves the last one up.
        })
    }
    loadStatus()
    const stream = api.events(
      {
        onEvent(event) {
          if (event.kind === 'session' && event.sid === sid) {
            dispatch({ type: 'session_event', sid, event: event.event, now: Date.now() })
            if (event.event.kind === 'answer' && event.event.options === undefined) {
              // The canonical turn is on disk now: reconcile the settled
              // live turn against it, and refresh the statusline.
              api
                .call('session.history', { sid })
                .then((history) => {
                  if (alive) dispatch({ type: 'history_loaded', sid, rows: history.rows, hasMore: history.hasMore, ...(history.nextBefore === undefined ? {} : { nextBefore: history.nextBefore }) })
                })
                .catch(() => {
                  // The ephemeral row stays until the next successful load.
                })
              loadStatus()
            }
            return
          }
          if (event.kind === 'turn_done' && event.sid === sid) dispatch({ type: 'turn_done', sid })
          // Other kinds (graph, delegation, other sids) are the team stream's business.
        },
      },
      sid,
    )
    return () => {
      alive = false
      stream.close()
    }
  }, [sid, api, dispatch, t])

  const working = (chat.activity ?? node?.activity) === 'working' && !chat.waiting

  // Elapsed timer for the working indicator.
  useEffect(() => {
    if (!working || chat.live === null) return
    const timer = setInterval(() => {
      setTick(value => value + 1)
    }, 1000)
    return () => {
      clearInterval(timer)
    }
  }, [working, chat.live])
  void tick

  // Pinned auto-scroll: follow new content unless the user scrolled away.
  const contentKey = `${chat.rows.length}:${chat.notices.length}:${chat.live?.content.length ?? 0}:${chat.live?.steps.length ?? 0}:${working}`
  useEffect(() => {
    const el = scrollRef.current
    if (el !== null && pinnedRef.current) el.scrollTop = el.scrollHeight
  }, [contentKey, sid])

  useEffect(() => {
    inputRef.current?.focus()
    pinnedRef.current = true
  }, [sid])

  const send = (): void => {
    const text = draft.trim()
    if (text.length === 0) return
    const uploaded = attachments.filter(a => a.path !== undefined && a.error === undefined)
    setDraft('')
    setAttachments([])
    pinnedRef.current = true
    dispatch({
      type: 'send_started',
      sid,
      text,
      ...(uploaded.length === 0 ? {} : { attachments: uploaded.map(a => ({ kind: a.kind, name: a.name })) }),
    })
    api
      .call('session.send', {
        sid,
        text,
        ...(uploaded.length === 0 ? {} : { attachments: uploaded.map(a => ({ kind: a.kind, path: a.path! })) }),
      })
      .then((receipt) => {
        dispatch({ type: 'send_settled', sid, receipt })
      })
      .catch((error: unknown) => {
        dispatch({ type: 'send_failed', sid, message: error instanceof Error ? error.message : String(error) })
      })
  }

  const interrupt = (): void => {
    api
      .call('session.interrupt', { sid })
      .then((receipt) => {
        if (!receipt.ok) dispatch({ type: 'notice', sid, kind: 'error', message: receipt.error ?? receipt.errorKind ?? 'interrupt failed' })
      })
      .catch((error: unknown) => {
        dispatch({ type: 'notice', sid, kind: 'error', message: error instanceof Error ? error.message : String(error) })
      })
  }

  const attach = (files: File[]): void => {
    if (project === undefined) return
    for (const file of files) {
      const key = `${Date.now()}-${file.name}-${Math.random().toString(36).slice(2, 8)}`
      const kind: 'image' | 'file' = file.type.startsWith('image/') ? 'image' : 'file'
      setAttachments(previous => [...previous, { key, name: file.name, kind, uploading: true }])
      api
        .upload(project, { name: file.name, type: file.type, body: file })
        .then((response) => {
          setAttachments(previous => previous.map(a => a.key !== key
            ? a
            : response.ok && response.attachment !== undefined
              ? { key, name: response.attachment.name, kind: response.attachment.kind === 'image' ? 'image' : 'file', path: response.attachment.path }
              : { ...a, uploading: false, error: response.error ?? 'upload failed' }))
        })
        .catch((error: unknown) => {
          setAttachments(previous => previous.map(a => a.key !== key ? a : { ...a, uploading: false, error: error instanceof Error ? error.message : String(error) }))
        })
    }
  }

  const resolve = (row: Extract<ChatRow, { kind: 'choice' }>, option: ChoiceOption): void => {
    dispatch({ type: 'choice_resolving', sid, id: row.id })
    api
      .call('session.resolve', { sid, token: row.token, selection: option.id })
      .then((receipt) => {
        if (receipt.ok) dispatch({ type: 'choice_resolved', sid, id: row.id, selection: option.id })
        else dispatch({ type: 'choice_failed', sid, id: row.id, message: receipt.error ?? receipt.errorKind ?? 'unknown' })
      })
      .catch((error: unknown) => {
        dispatch({ type: 'choice_failed', sid, id: row.id, message: error instanceof Error ? error.message : String(error) })
      })
  }

  const loadOlder = (): void => {
    if (chat.loadingOlder || chat.nextBefore === undefined) return
    dispatch({ type: 'history_loading', sid, older: true })
    api
      .call('session.history', { sid, before: chat.nextBefore })
      .then((history) => {
        dispatch({ type: 'history_loaded', sid, rows: history.rows, hasMore: history.hasMore, older: true, ...(history.nextBefore === undefined ? {} : { nextBefore: history.nextBefore }) })
      })
      .catch(() => {
        dispatch({ type: 'history_failed', sid, message: t('chat.history.error') })
      })
  }

  const trailing = node === undefined
    ? undefined
    : (
        <span className={css.composerModel} title={`${node.vendor}${node.model === undefined ? '' : ` · ${node.model}`}`}>
          <b>{node.vendor}</b>
          {node.model !== undefined && <span>· {chat.status?.model ?? node.model}</span>}
          {(chat.status?.effort ?? node.effort) !== undefined && <span>· {chat.status?.effort ?? node.effort}</span>}
        </span>
      )

  return (
    <>
      <div
        ref={scrollRef}
        className={css.chatScroll}
        onScroll={(event) => {
          const el = event.currentTarget
          pinnedRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < PIN_THRESHOLD
        }}
      >
        <div className={css.chatColumn}>
          {chat.hasMore && (
            <Button variant="ghost" size="sm" className={css.olderBtn} disabled={chat.loadingOlder} onClick={loadOlder}>
              {chat.loadingOlder ? t('chat.older.loading') : t('chat.older')}
            </Button>
          )}
          {chat.error !== null && <div className={clsx(css.notice, css.noticeError)}>{chat.error}</div>}
          {chat.rows.length === 0 && chat.live === null && chat.error === null && !chat.loading && (
            <div className={css.centerNote}>{t('chat.empty')}</div>
          )}
          {chat.rows.map(row => (
            <Row key={row.id} row={row} selectedStep={selectedStep} t={t} onSelectStep={onSelectStep} onResolve={resolve} />
          ))}
          {chat.live !== null && chat.live.steps.length > 0 && (
            <div className={css.turnAssistant}>
              <Steps steps={chat.live.steps} selected={selectedStep} t={t} onSelect={onSelectStep} />
            </div>
          )}
          {chat.notices.map(notice => (
            <NoticeRow key={notice.id} notice={notice} t={t} />
          ))}
          {chat.waiting && (
            <div className={css.working}>
              <StateDot state="warning" />
              {t('chat.waiting')}
            </div>
          )}
          {working && (
            <div className={css.working}>
              <StateDot state="ongoing" />
              {t('chat.working')}
              {chat.live !== null && <span className={css.workingTime}>· {formatElapsed(Date.now() - chat.live.startedAt)}</span>}
              {chat.live !== null && chat.live.steps.length === 0 && chat.live.content !== '' && (
                // The vendor's folded status line (no structured steps to show instead).
                <span className={css.workingNote}>{chat.live.content}</span>
              )}
            </div>
          )}
        </div>
      </div>
      <Composer
        t={t}
        draft={draft}
        onDraftChange={setDraft}
        onSubmit={send}
        working={working}
        onStop={interrupt}
        placeholder={t('chat.placeholder')}
        attachments={attachments}
        onAttach={project === undefined ? undefined : attach}
        onRemoveAttachment={(key) => {
          setAttachments(previous => previous.filter(a => a.key !== key))
        }}
        trailing={trailing}
        hint={t('chat.hint')}
        commands
        inputRef={inputRef}
      />
    </>
  )
}
