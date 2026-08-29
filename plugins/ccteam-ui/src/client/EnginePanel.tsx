/**
 * The engine's faces inside the workbench: the state dot the header and the
 * settings card share, the first-run panel the workbench gates on (engine not
 * ready → one-click start; no project → add a workspace), and the version
 * banner. Presentation only — the store holds the engine slice, engine.ts
 * derives every verdict, and the workbench wires the actions.
 */
import { useState } from 'react'
import type { KeyboardEvent } from 'react'
import {
  Button,
  IconCheckOutline14,
  IconCloseOutline16,
  IconCopyOutline16,
  IconFolderOpen16,
  IconLoadingOutline16,
  IconPlayOutline16,
  IconWarningOutline16,
  StateDot,
  writeClipboard,
} from '@deepseek-ai/dsh-client-ui-primitives'
import type { EngineStatus } from '../shared/contract.js'
import { engineDot, engineEnablement, engineInertKey, engineStateKey, truncateMiddle } from './engine.js'
import type { EngineDotState, VersionRelation } from './engine.js'
import { isAbsolutePath } from './projects.js'
import type { T, UseWorkspaces } from './slots.js'
import type { EngineAction } from './store.js'
import css from './workbench.module.css'

/** A code chip with a copy button (`ccteam start`, `dsh plugin update …`). */
export function CopyChip({ text, t }: { text: string; t: T }) {
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

/**
 * The engine dot: StateDot for the four colored states, a grey disc for
 * neutral (installed, not running / not yet known).
 * @param props - dot state and diameter.
 * @returns the dot (aria-hidden; pair it with text).
 */
export function EngineDot({ dot, size = 8 }: { dot: EngineDotState; size?: number }) {
  if (dot === 'neutral') {
    return <span className={css.dotNeutral} style={{ width: size, height: size }} aria-hidden="true" />
  }
  return <StateDot state={dot} size={size} />
}

export interface EnginePanelProps {
  t: T
  status: EngineStatus | null
  pending: EngineAction | null
  error: string | null
  /** First run: the workbench is gated on the engine. Manual: opened from the header dot. */
  mode: 'first-run' | 'manual'
  onStart(): void
  /** Present when the DSH runtime offers a way to the plugin's settings card. */
  onOpenSettings?: (() => void) | undefined
}

/**
 * The engine panel: what is true (state, reason, the host's own sentence),
 * the one action that helps (start), and where the rest lives.
 * @param props - status, pending action, error, and the start action.
 * @returns the centered panel.
 */
export function EnginePanel({ t, status, pending, error, mode, onStart, onOpenSettings }: EnginePanelProps) {
  const dot = engineDot(status)
  const inertKey = engineInertKey(status)
  const enablement = engineEnablement(status, pending)
  const state = status?.state
  const busy = state === 'starting' || state === 'installing'
  const homeMismatch = status !== null && status.state === 'mismatch' && status.mismatch === 'home'
  const title = status === null
    ? t('engine.state.unknown')
    : state === 'starting'
      ? t('firstrun.engine.starting')
      : state === 'installing'
        ? t('firstrun.engine.installing')
        : state === 'unsupported'
          ? t('firstrun.engine.unsupported')
          : homeMismatch
            ? t('firstrun.engine.mismatchHome')
            : state === 'stopped' || state === 'missing'
              ? t('firstrun.engine.title')
              : t(engineStateKey(status))
  const reason = status === null
    ? null
    : state === 'stopped'
      ? t('firstrun.engine.reason.stopped')
      : state === 'missing'
        ? t('firstrun.engine.reason.missing')
        : state === 'unsupported'
          ? t('firstrun.engine.reason.unsupported')
          : homeMismatch
            ? t('firstrun.engine.mismatchHome.hint')
            : busy
              ? t('firstrun.engine.reason.busy')
              : t('firstrun.engine.reason.ready')
  const showStart = status !== null && inertKey === null && !busy && (enablement.start || pending === 'start')
  return (
    <div className={css.firstRun} data-first-run={mode} data-engine-state={state ?? 'unknown'}>
      <div className={css.firstRunCard} role="group" aria-label={title}>
        <div className={css.firstRunTitle}>
          {busy || status === null ? <IconLoadingOutline16 className={css.spin} size={16} /> : <EngineDot dot={dot} size={10} />}
          <span>{title}</span>
        </div>
        {reason !== null && <p className={css.firstRunBody}>{reason}</p>}
        {inertKey !== null && state !== 'unsupported' && <p className={css.firstRunBody}>{t(inertKey)}</p>}
        {homeMismatch && (
          <dl className={css.firstRunFacts}>
            <dt>{t('firstrun.engine.mismatchHome.plugin')}</dt>
            <dd title={status.home}>{status.home}</dd>
            <dt>{t('firstrun.engine.mismatchHome.daemon')}</dt>
            <dd title={status.daemonHome ?? ''}>{status.daemonHome ?? '—'}</dd>
          </dl>
        )}
        {status !== null && status.detail !== '' && <p className={css.firstRunDetail}>{status.detail}</p>}
        {error !== null && <p className={css.firstRunError} role="alert">{t('engine.error', { message: error })}</p>}
        {(showStart || onOpenSettings !== undefined) && (
          <div className={css.firstRunActions}>
            {showStart && (
              <Button
                variant="primary"
                size="md"
                icon={pending === 'start' ? <IconLoadingOutline16 className={css.spin} size={16} /> : <IconPlayOutline16 size={16} />}
                disabled={pending !== null}
                onClick={onStart}
              >
                {pending === 'start' ? t('engine.action.starting') : t('firstrun.start')}
              </Button>
            )}
            {onOpenSettings !== undefined && (
              <Button variant="outline" size="md" onClick={onOpenSettings}>
                {t('firstrun.openSettings')}
              </Button>
            )}
          </div>
        )}
        {onOpenSettings === undefined && <p className={css.firstRunHint}>{t('firstrun.settings.hint')}</p>}
      </div>
    </div>
  )
}

export interface VersionBannerProps {
  t: T
  relation: VersionRelation
  /** The update action is available (supervised, version mismatch, engine older). */
  canUpdate: boolean
  pending: EngineAction | null
  onUpdate(): void
  onDismiss(): void
}

/**
 * The version banner: engine older than the plugin's pinned version → update
 * the engine (one-way repair, PRD D5); plugin older than the daemon → the
 * `dsh plugin update` command to copy.
 * @param props - relation and actions.
 * @returns the banner, or null when the versions agree.
 */
export function VersionBanner({ t, relation, canUpdate, pending, onUpdate, onDismiss }: VersionBannerProps) {
  if (relation.kind !== 'engine-older' && relation.kind !== 'plugin-older') return null
  return (
    <div className={css.banner} role="status" data-relation={relation.kind}>
      <IconWarningOutline16 className={css.bannerIcon} size={16} />
      <span className={css.bannerText}>
        {relation.kind === 'engine-older'
          ? t('banner.engineOlder', { engine: relation.engine, pinned: relation.pinned })
          : t('banner.pluginOlder', { plugin: relation.plugin, engine: relation.engine })}
      </span>
      <span className={css.bannerActions}>
        {relation.kind === 'engine-older' && canUpdate && (
          <Button variant="primary" size="sm" disabled={pending !== null} onClick={onUpdate}>
            {pending === 'update' ? t('engine.action.updating') : t('banner.update')}
          </Button>
        )}
        {relation.kind === 'plugin-older' && <CopyChip text="dsh plugin update @ccteam/ccteam-ui" t={t} />}
        <button type="button" className={css.iconBtn} aria-label={t('banner.dismiss')} onClick={onDismiss}>
          <IconCloseOutline16 size={14} />
        </button>
      </span>
    </div>
  )
}

export interface ProjectPanelProps {
  t: T
  busy: boolean
  error: string | null
  /** DSH's own workspace list (the framework's global seat), offered as one-click rows. */
  useWorkspaces?: UseWorkspaces | undefined
  onCreate(path: string, slug: string): void
}

/**
 * The "add a workspace" panel: an absolute directory (validated here, so a
 * relative path never crosses the wire), an optional slug, and DSH's own
 * workspaces as one-click rows above the input.
 * @param props - busy/error state, DSH's workspace hook, the create action.
 * @returns the centered panel.
 */
export function ProjectPanel({ t, busy, error, useWorkspaces, onCreate }: ProjectPanelProps) {
  const [path, setPath] = useState('')
  const [slug, setSlug] = useState('')
  const [validation, setValidation] = useState<string | null>(null)
  const submit = (): void => {
    if (busy) return
    const trimmed = path.trim()
    if (!isAbsolutePath(trimmed)) {
      setValidation(t('firstrun.project.needAbsolute'))
      return
    }
    setValidation(null)
    onCreate(trimmed, slug.trim())
  }
  const onEnter = (event: KeyboardEvent<HTMLInputElement>): void => {
    if (event.key !== 'Enter') return
    event.preventDefault()
    submit()
  }
  const shown = validation ?? (error === null ? null : t('firstrun.project.error', { message: error }))
  return (
    <div className={css.firstRun} data-first-run="no-project">
      <div className={css.firstRunCard} role="group" aria-label={t('firstrun.project.title')}>
        <div className={css.firstRunTitle}>
          <IconFolderOpen16 size={18} />
          <span>{t('firstrun.project.title')}</span>
        </div>
        <p className={css.firstRunBody}>{t('firstrun.project.body')}</p>
        {useWorkspaces !== undefined && (
          <DshWorkspaces
            t={t}
            busy={busy}
            useWorkspaces={useWorkspaces}
            onPick={(picked) => {
              setValidation(null)
              onCreate(picked, '')
            }}
          />
        )}
        <label className={css.projectField}>
          <span className={css.projectLabel}>{t('firstrun.project.path')}</span>
          <input
            className={css.projectInput}
            type="text"
            autoFocus
            spellCheck={false}
            placeholder="/home/you/project"
            value={path}
            disabled={busy}
            onChange={(event) => {
              setPath(event.currentTarget.value)
            }}
            onKeyDown={onEnter}
          />
        </label>
        <label className={css.projectField}>
          <span className={css.projectLabel}>{t('firstrun.project.slug')}</span>
          <input
            className={css.projectInput}
            type="text"
            spellCheck={false}
            placeholder={t('firstrun.project.slug.placeholder')}
            value={slug}
            disabled={busy}
            onChange={(event) => {
              setSlug(event.currentTarget.value)
            }}
            onKeyDown={onEnter}
          />
        </label>
        {shown !== null && <p className={css.firstRunError} role="alert">{shown}</p>}
        <div className={css.firstRunActions}>
          <Button
            variant="primary"
            size="md"
            type="button"
            disabled={busy}
            icon={busy ? <IconLoadingOutline16 className={css.spin} size={16} /> : undefined}
            onClick={submit}
          >
            {busy ? t('firstrun.project.adding') : t('firstrun.project.add')}
          </Button>
        </div>
      </div>
    </div>
  )
}

function DshWorkspaces({ t, busy, useWorkspaces, onPick }: {
  t: T
  busy: boolean
  useWorkspaces: UseWorkspaces
  onPick(path: string): void
}) {
  const items = useWorkspaces(list => list.items)
  if (items.length === 0) return null
  return (
    <div className={css.importList} role="group" aria-label={t('firstrun.project.import')}>
      <div className={css.importTitle}>{t('firstrun.project.import')}</div>
      {items.map(item => (
        <div key={item.workspaceId} className={css.importRow}>
          <span className={css.importText}>
            <span className={css.importName}>{item.title}</span>
            <span className={css.importPath} title={item.path}>{truncateMiddle(item.path, 44)}</span>
          </span>
          <Button
            variant="outline"
            size="sm"
            disabled={busy}
            onClick={() => {
              onPick(item.path)
            }}
          >
            {t('firstrun.project.add')}
          </Button>
        </div>
      ))}
    </div>
  )
}
