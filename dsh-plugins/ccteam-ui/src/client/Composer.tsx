/**
 * The composer, drawn to DSH's input-box geometry: a 16px-radius box with the
 * growing textarea on top and a bottom bar (attachments on the left, the
 * vendor·model label and the round send/stop button on the right). Enter
 * sends, Shift+Enter breaks a line, IME composition never sends. Typing `/`
 * at the start opens the pass-through command menu. Shared by the hero (new
 * session) and the chat (follow-up turns); the owner supplies the submit.
 */
import { useEffect, useRef, useState } from 'react'
import type { ChangeEvent, KeyboardEvent, ReactNode } from 'react'
import clsx from 'clsx'
import {
  IconCloseFill14,
  IconLoadingOutline16,
  IconPaperclipOutline16,
  IconSendOutline16,
  IconStopFill16,
  Menu,
} from '@deepseek-ai/dsh-client-ui-primitives'
import type { MenuEntry } from '@deepseek-ai/dsh-client-ui-primitives'
import type { AttachmentRef } from '../shared/contract.js'
import type { T } from './slots.js'
import css from './workbench.module.css'

/** Composer max height (px); keep in step with .composerText max-height. */
const MAX_HEIGHT = 240

/** Pass-through commands the vendor understands (ccteam forwards verbatim). */
export const COMMANDS: ReadonlyArray<{ id: string; insert: string; key: 'chat.command.compact' | 'chat.command.new' | 'chat.command.clear' | 'chat.command.role' | 'chat.command.model' }> = [
  { id: 'compact', insert: '/compact', key: 'chat.command.compact' },
  { id: 'new', insert: '/new', key: 'chat.command.new' },
  { id: 'clear', insert: '/clear', key: 'chat.command.clear' },
  { id: 'role', insert: '/role ', key: 'chat.command.role' },
  { id: 'model', insert: '/model ', key: 'chat.command.model' },
]

/** One pending upload / uploaded attachment in the composer. */
export interface ComposerAttachment {
  key: string
  name: string
  kind: 'image' | 'file'
  /** Stored path once uploaded. */
  path?: string
  uploading?: boolean
  error?: string
}

export interface ComposerProps {
  t: T
  draft: string
  onDraftChange(next: string): void
  onSubmit(): void
  /** Show the stop button instead of send. */
  working?: boolean
  onStop?(): void
  busy?: boolean
  disabled?: boolean
  placeholder: string
  attachments?: ComposerAttachment[]
  onAttach?(files: File[]): void
  onRemoveAttachment?(key: string): void
  /** Right-hand label (vendor · model) or any node. */
  trailing?: ReactNode
  /** Left-hand controls before the attachment button. */
  leading?: ReactNode
  error?: string | null
  hint?: string
  autoFocus?: boolean
  /** Enable the `/` command menu (chat only). */
  commands?: boolean
  inputRef?: { current: HTMLTextAreaElement | null }
}

/**
 * Render the composer.
 * @param props - draft state, actions and chrome.
 * @returns the composer box.
 */
export function Composer(props: ComposerProps) {
  const { t, draft, onDraftChange, onSubmit, working, onStop, busy, disabled, placeholder } = props
  const localRef = useRef<HTMLTextAreaElement | null>(null)
  const inputRef = props.inputRef ?? localRef
  const fileRef = useRef<HTMLInputElement | null>(null)
  const [menuOpen, setMenuOpen] = useState(false)

  useEffect(() => {
    if (props.autoFocus === true) inputRef.current?.focus()
  }, [props.autoFocus, inputRef])

  useEffect(() => {
    const el = inputRef.current
    if (el === null) return
    el.style.height = 'auto'
    el.style.height = `${Math.min(el.scrollHeight, MAX_HEIGHT)}px`
  }, [draft, inputRef])

  const commandsEnabled = props.commands === true
  useEffect(() => {
    if (!commandsEnabled) return
    setMenuOpen(draft === '/' || (draft.startsWith('/') && !draft.includes(' ') && draft.length < 12 && !draft.includes('\n')))
  }, [draft, commandsEnabled])

  const uploading = (props.attachments ?? []).some(a => a.uploading === true)
  const canSend = draft.trim().length > 0 && busy !== true && disabled !== true && !uploading

  const submit = (): void => {
    if (!canSend) return
    setMenuOpen(false)
    onSubmit()
  }

  const onKeyDown = (event: KeyboardEvent<HTMLTextAreaElement>): void => {
    if (menuOpen && (event.key === 'ArrowDown' || event.key === 'ArrowUp' || event.key === 'Tab')) return
    if (event.key === 'Escape') {
      if (menuOpen) {
        event.preventDefault()
        event.stopPropagation()
        setMenuOpen(false)
      }
      return
    }
    if (event.key === 'Enter' && !event.shiftKey && !event.nativeEvent.isComposing) {
      event.preventDefault()
      submit()
    }
  }

  const items: MenuEntry[] = [
    { type: 'label', id: 'label', text: t('chat.commands') },
    ...COMMANDS.filter(c => draft === '/' || c.insert.startsWith(draft)).map(command => ({
      id: command.id,
      label: (
        <>
          <span className={css.mono}>{command.insert.trim()}</span>
          <span className={css.menuMeta}>{t(command.key)}</span>
        </>
      ),
    })),
  ]

  const textarea = (
    <textarea
      ref={inputRef}
      className={css.composerText}
      rows={2}
      placeholder={placeholder}
      value={draft}
      disabled={disabled === true}
      onChange={(event: ChangeEvent<HTMLTextAreaElement>) => {
        onDraftChange(event.currentTarget.value)
      }}
      onKeyDown={onKeyDown}
    />
  )

  return (
    <div className={css.composerWrap}>
      <div className={css.composerBox} data-error={props.error ? '' : undefined}>
        {commandsEnabled
          ? (
              <Menu
                open={menuOpen}
                align="start"
                side="top"
                portal
                dense
                items={items}
                onSelect={(id) => {
                  const command = COMMANDS.find(c => c.id === id)
                  setMenuOpen(false)
                  if (command !== undefined) {
                    onDraftChange(command.insert)
                    inputRef.current?.focus()
                  }
                }}
                onClose={() => {
                  setMenuOpen(false)
                }}
                anchor={textarea}
              />
            )
          : textarea}
        {props.attachments !== undefined && props.attachments.length > 0 && (
          <div className={css.attachChips}>
            {props.attachments.map(attachment => (
              <span key={attachment.key} className={css.chip} title={attachment.error ?? attachment.name}>
                {attachment.uploading === true
                  ? <IconLoadingOutline16 className={css.spin} size={12} />
                  : <IconPaperclipOutline16 size={12} />}
                <span className={css.chipText}>
                  {attachment.error !== undefined
                    ? t('chat.attach.failed', { kind: attachment.error })
                    : attachment.uploading === true ? t('chat.attach.uploading') : attachment.name}
                </span>
                {props.onRemoveAttachment !== undefined && (
                  <button
                    type="button"
                    className={css.chipBtn}
                    aria-label={t('chat.attach.remove')}
                    onClick={() => {
                      props.onRemoveAttachment?.(attachment.key)
                    }}
                  >
                    <IconCloseFill14 size={12} />
                  </button>
                )}
              </span>
            ))}
          </div>
        )}
        <div className={css.composerBar}>
          <div className={css.composerLeft}>
            {props.leading}
            {props.onAttach !== undefined && (
              <>
                <input
                  ref={fileRef}
                  type="file"
                  multiple
                  hidden
                  onChange={(event) => {
                    const files = Array.from(event.currentTarget.files ?? [])
                    event.currentTarget.value = ''
                    if (files.length > 0) props.onAttach?.(files)
                  }}
                />
                <button
                  type="button"
                  className={css.iconBtn}
                  aria-label={t('chat.attach')}
                  title={t('chat.attach')}
                  disabled={disabled === true}
                  onClick={() => {
                    fileRef.current?.click()
                  }}
                >
                  <IconPaperclipOutline16 size={16} />
                </button>
              </>
            )}
          </div>
          <div className={css.composerRight}>
            {props.trailing}
            {working === true && onStop !== undefined
              ? (
                  <button
                    type="button"
                    className={clsx(css.sendBtn)}
                    data-stop=""
                    aria-label={t('chat.stop')}
                    title={t('chat.stop')}
                    onClick={onStop}
                  >
                    <IconStopFill16 size={14} />
                  </button>
                )
              : (
                  <button
                    type="button"
                    className={css.sendBtn}
                    aria-label={t('chat.send')}
                    title={t('chat.send')}
                    disabled={!canSend}
                    onClick={submit}
                  >
                    {busy === true ? <IconLoadingOutline16 className={css.spin} size={16} /> : <IconSendOutline16 size={16} />}
                  </button>
                )}
          </div>
        </div>
      </div>
      {props.error ? <div className={css.composerError} role="alert">{props.error}</div> : null}
      {props.hint !== undefined && <div className={css.hint}>{props.hint}</div>}
    </div>
  )
}
