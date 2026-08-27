/**
 * One session's chat view: transcript from `session.history` + live rows and
 * activity from the sid-filtered event stream, a typing indicator while the
 * session works, and a composer (Enter sends, Shift+Enter newline, Esc
 * bubbles to the panel = back). Send receipts are honest: queued renders an
 * inline notice with what it is queued behind, failures render an error row —
 * never swallowed. Auto-scroll stays pinned to the bottom until the user
 * scrolls up.
 */
import { useEffect, useRef, useState } from 'react'
import clsx from 'clsx'
import { IconSendOutline16, StateDot } from '@deepseek-ai/dsh-client-ui-primitives'
import type { TeamNode } from '../shared/contract.js'
import type { ApiClient } from './api.js'
import type { ChatNotice, ChatState } from './store.js'
import type { Dispatch } from './Panel.js'
import type { T } from './slots.js'
import css from './panel.module.css'

/** Composer max height (px); keep in step with .composerInput max-height. */
const COMPOSER_MAX_HEIGHT = 140

/** Distance from the bottom (px) under which auto-scroll stays pinned. */
const PIN_THRESHOLD = 48

/** Chat view props (assembled by Panel from the one store snapshot). */
export interface SessionChatProps {
  sid: string
  chat: ChatState
  node: TeamNode | undefined
  dispatch: Dispatch
  api: ApiClient
  t: T
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
  return (
    <div className={clsx(css.notice, css.noticeError)} role="alert">
      {t('chat.failed', { kind: notice.errorKind ?? notice.message ?? 'unknown' })}
      {notice.errorKind !== undefined && notice.message !== undefined ? ` — ${notice.message}` : ''}
    </div>
  )
}

/**
 * Render the chat view body (the header lives with the panel chrome).
 * @param props - sid + transcript state + transports.
 * @returns the chat body: transcript + composer.
 */
export function SessionChat({ sid, chat, node, dispatch, api, t }: SessionChatProps) {
  const [draft, setDraft] = useState('')
  const scrollRef = useRef<HTMLDivElement | null>(null)
  const inputRef = useRef<HTMLTextAreaElement | null>(null)
  const pinnedRef = useRef(true)

  // History + the sid-filtered live stream, both scoped to this sid's mount.
  useEffect(() => {
    let alive = true
    dispatch({ type: 'history_loading', sid })
    api
      .call('session.history', { sid })
      .then((history) => {
        if (!alive) return
        dispatch({ type: 'history_loaded', sid, rows: history.rows })
      })
      .catch(() => {
        if (!alive) return
        dispatch({ type: 'history_failed', sid, message: t('chat.history.error') })
      })
    const stream = api.events(
      {
        onEvent(event) {
          if (event.kind === 'session') {
            if (event.row !== undefined) dispatch({ type: 'event_row', sid, row: event.row })
            if (event.activity !== undefined) dispatch({ type: 'activity', sid, activity: event.activity })
            return
          }
          if (event.kind === 'turn_done') dispatch({ type: 'turn_done', sid })
          // Unknown kinds: ignored (forward-compat contract).
        },
      },
      sid,
    )
    return () => {
      alive = false
      stream.close()
    }
  }, [sid, api, dispatch, t])

  const working = (chat.activity ?? node?.activity) === 'working'

  // Pinned auto-scroll: follow new rows unless the user scrolled away.
  const rowCount = chat.rows.length + chat.notices.length
  useEffect(() => {
    const el = scrollRef.current
    if (el !== null && pinnedRef.current) el.scrollTop = el.scrollHeight
  }, [rowCount, working, sid])

  useEffect(() => {
    inputRef.current?.focus()
  }, [sid])

  const send = (): void => {
    const text = draft.trim()
    if (text.length === 0) return
    setDraft('')
    const input = inputRef.current
    if (input !== null) input.style.height = 'auto'
    pinnedRef.current = true
    dispatch({ type: 'send_started', sid, text })
    api
      .call('session.send', { sid, text })
      .then((receipt) => {
        dispatch({ type: 'send_settled', sid, receipt })
      })
      .catch((error: unknown) => {
        dispatch({ type: 'send_failed', sid, message: error instanceof Error ? error.message : String(error) })
      })
  }

  return (
    <>
      <div
        ref={scrollRef}
        className={css.scroll}
        onScroll={(event) => {
          const el = event.currentTarget
          pinnedRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < PIN_THRESHOLD
        }}
      >
        <div className={css.transcript}>
          {chat.error !== null && <div className={clsx(css.notice, css.noticeError)}>{chat.error}</div>}
          {chat.rows.length === 0 && chat.error === null && !chat.loading && (
            <div className={css.centerNote}>{t('chat.empty')}</div>
          )}
          {chat.rows.map(row => (
            <div key={row.turnId} className={row.role === 'user' ? css.rowUser : css.rowAssistant}>
              {row.content}
            </div>
          ))}
          {chat.notices.map(notice => (
            <NoticeRow key={notice.id} notice={notice} t={t} />
          ))}
          {working && (
            <div className={css.typing}>
              <StateDot state="ongoing" />
              {t('chat.working')}
            </div>
          )}
        </div>
      </div>
      <div className={css.composer}>
        <div className={css.composerRow}>
          <textarea
            ref={inputRef}
            className={css.composerInput}
            rows={1}
            placeholder={t('chat.placeholder')}
            value={draft}
            onChange={(event) => {
              setDraft(event.currentTarget.value)
            }}
            onInput={(event) => {
              const el = event.currentTarget
              el.style.height = 'auto'
              el.style.height = `${Math.min(el.scrollHeight, COMPOSER_MAX_HEIGHT)}px`
            }}
            onKeyDown={(event) => {
              if (event.key === 'Enter' && !event.shiftKey && !event.nativeEvent.isComposing) {
                event.preventDefault()
                send()
              }
            }}
          />
          <button
            type="button"
            className={css.sendBtn}
            aria-label={t('chat.send')}
            disabled={draft.trim().length === 0}
            onClick={send}
          >
            <IconSendOutline16 size={16} />
          </button>
        </div>
        <div className={css.composerHint}>{t('chat.hint')}</div>
      </div>
    </>
  )
}
