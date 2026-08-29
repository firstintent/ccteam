/**
 * The 「引擎」 section at the top of the ccteam settings card: state dot +
 * label, the facts line, the explicit actions (stop / restart confirm in a
 * Modal), the live auto-start switch, the advanced engine-path override, and
 * the daemon log tail. Facts come from the host's `engine.status`; every
 * verdict (colors, enablement, the inert sentence) is derived in engine.ts.
 * Presentation only — the store holds the slice, the runners do the calls.
 */
import { useEffect, useState } from 'react'
import {
  Button,
  DisclosureRow,
  IconCodeOutline16,
  IconRightUpOutline14,
  IconSettingsOutline14,
  Modal,
} from '@deepseek-ai/dsh-client-ui-primitives'
import type { EngineLogResponse } from '../../shared/contract.js'
import type { ApiClient } from '../api.js'
import { EngineDot } from '../EnginePanel.js'
import {
  confirmEngineAction,
  engineDot,
  engineEnablement,
  engineInertKey,
  engineStateKey,
  hostDetailShown,
  requestEngineAction,
  stripAnsi,
  truncateMiddle,
} from '../engine.js'
import type { CcteamLocaleKey } from '../locales.js'
import type { T } from '../slots.js'
import type { Action, EngineAction, EngineSlice } from '../store.js'
import type { FieldState } from './form.js'
import css from './settings.module.css'

/** Lines of the daemon log the tail shows. */
export const ENGINE_LOG_LINES = 50

export interface EngineSectionProps {
  t: T
  engine: EngineSlice
  api: ApiClient
  dispatch(action: Action): void
  /** The live `autoStart` toggle. */
  autoStart: boolean
  onAutoStart(next: boolean): void
  /** The staged `enginePath` text field (written by the card's Save). */
  enginePath: {
    id: string
    value: FieldState
    writable: boolean
    placeholder: string
    onEdit(text: string): void
    onReset(): void
  }
}

interface LogState {
  lines: string[]
  error: string | null
  loading: boolean
}

/**
 * Render the engine section.
 * @param props - the engine slice, the BFF client, and the two settings controls.
 * @returns the section.
 */
export function EngineSection({ t, engine, api, dispatch, autoStart, onAutoStart, enginePath }: EngineSectionProps) {
  const [advancedOpen, setAdvancedOpen] = useState(false)
  const [logOpen, setLogOpen] = useState(false)
  const [log, setLog] = useState<LogState>({ lines: [], error: null, loading: false })
  const status = engine.status
  const pending = engine.pending
  const enablement = engineEnablement(status, pending)
  const inertKey = engineInertKey(status)
  const supervised = status !== null && status.supervised

  const loadLog = (): void => {
    setLog(previous => ({ ...previous, loading: true }))
    api
      .call('engine.log', { lines: ENGINE_LOG_LINES })
      .then((response: EngineLogResponse) => {
        setLog({ lines: response.lines.map(stripAnsi), error: response.ok ? null : response.error ?? 'unknown', loading: false })
      })
      .catch((error: unknown) => {
        setLog({ lines: [], error: error instanceof Error ? error.message : String(error), loading: false })
      })
  }
  useEffect(() => {
    if (logOpen) loadLog()
    // The tail is (re)read when the disclosure opens; 刷新 re-reads on demand.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [logOpen])

  const label = (action: EngineAction, idle: CcteamLocaleKey, busyKey: CcteamLocaleKey): string =>
    t(pending === action ? busyKey : idle)
  const act = (action: EngineAction): void => {
    void requestEngineAction(dispatch, api, action)
  }

  const facts: Array<{ text: string; title: string }> = []
  if (status?.binaryVersion !== undefined) {
    facts.push({ text: t('engine.fact.engine', { version: status.binaryVersion }), title: status.binary ?? '' })
  }
  if (status?.runningVersion !== undefined && status.runningVersion !== status.binaryVersion) {
    facts.push({ text: t('engine.fact.daemon', { version: status.runningVersion }), title: t('engine.fact.daemon.hint') })
  }
  if (status?.pid !== undefined) facts.push({ text: `pid ${status.pid}`, title: 'pid' })
  if (status !== null && status.home !== '') facts.push({ text: truncateMiddle(status.home, 30), title: status.home })
  if (status?.webBind !== undefined) facts.push({ text: status.webBind, title: t('engine.fact.webBind') })

  return (
    <section className={css.engine} aria-label={t('engine.title')} data-engine-state={status?.state ?? 'unknown'}>
      <div className={css.engineHead}>
        <span className={css.engineTitle}>{t('engine.title')}</span>
        <span className={css.engineStatus}>
          <EngineDot dot={engineDot(status)} size={8} />
          <span className={css.engineState}>{t(engineStateKey(status))}</span>
        </span>
      </div>
      {status?.state === 'attached' && <p className={css.engineHint}>{t('engine.attached.hint')}</p>}
      {status !== null && hostDetailShown(status, facts.length > 0) && <p className={css.engineDetail}>{status.detail}</p>}
      {facts.length > 0 && (
        <p className={css.engineFacts}>
          {facts.map(fact => (
            <span key={fact.text} className={css.engineFact} title={fact.title}>{fact.text}</span>
          ))}
          {status?.reachable === true && (
            <a className={css.engineLink} href={status.daemonUrl} target="_blank" rel="noreferrer">
              {t('engine.openWeb')}
              <IconRightUpOutline14 size={12} />
            </a>
          )}
        </p>
      )}
      {inertKey !== null
        ? <p className={css.engineInert}>{t(inertKey)}</p>
        : status !== null && (
          <div className={css.engineActions}>
            <Button
              variant="outline"
              size="sm"
              disabled={!enablement.start}
              onClick={() => {
                act('start')
              }}
            >
              {label('start', 'engine.action.start', 'engine.action.starting')}
            </Button>
            <Button
              variant="outline"
              size="sm"
              disabled={!enablement.stop}
              onClick={() => {
                act('stop')
              }}
            >
              {label('stop', 'engine.action.stop', 'engine.action.stopping')}
            </Button>
            <Button
              variant="outline"
              size="sm"
              disabled={!enablement.restart}
              onClick={() => {
                act('restart')
              }}
            >
              {label('restart', 'engine.action.restart', 'engine.action.restarting')}
            </Button>
            {(enablement.update || pending === 'update') && (
              <Button
                variant="primary"
                size="sm"
                disabled={!enablement.update}
                onClick={() => {
                  act('update')
                }}
              >
                {label('update', 'engine.action.update', 'engine.action.updating')}
              </Button>
            )}
          </div>
        )}
      {engine.error !== null && <p className={css.engineError} role="alert">{t('engine.error', { message: engine.error })}</p>}
      {engine.pollError !== null && <p className={css.engineError} role="status">{t('engine.pollError', { message: engine.pollError })}</p>}
      {supervised && (
        <>
          <label className={css.engineToggle}>
            <span className={css.engineToggleText}>
              {t('engine.autoStart')}
              <span className={css.hint}>{t('engine.autoStart.hint')}</span>
            </span>
            <input
              className={css.switch}
              type="checkbox"
              role="switch"
              checked={autoStart}
              disabled={!enginePath.writable}
              onChange={(event) => {
                onAutoStart(event.currentTarget.checked)
              }}
            />
          </label>
          <DisclosureRow
            icon={<IconSettingsOutline14 size={14} />}
            title={t('engine.advanced')}
            open={advancedOpen}
            expandable
            expandOnRowClick
            onToggle={() => {
              setAdvancedOpen(previous => !previous)
            }}
          >
            <div className={css.field}>
              <div className={css.head}>
                <label className={css.label} htmlFor={enginePath.id}>{t('engine.enginePath')}</label>
                <span className={css.badges}>
                  {enginePath.value.overridden && (
                    <>
                      <span className={css.badge}>{t('settings.overridden')}</span>
                      <button
                        type="button"
                        className={css.reset}
                        disabled={!enginePath.writable}
                        onClick={enginePath.onReset}
                      >
                        {t('settings.reset')}
                      </button>
                    </>
                  )}
                </span>
              </div>
              <input
                id={enginePath.id}
                className={css.input}
                type="text"
                autoComplete="off"
                spellCheck={false}
                value={enginePath.value.text}
                placeholder={enginePath.placeholder}
                disabled={!enginePath.writable}
                onChange={(event) => {
                  enginePath.onEdit(event.currentTarget.value)
                }}
              />
              <p className={css.hint}>{t('engine.enginePath.hint')}</p>
              <p className={css.engineResolved}>
                <span>{t('engine.resolvedBinary')}</span>
                <code title={status?.binary ?? ''}>{status?.binary ?? '—'}</code>
                {status?.binarySource !== undefined && <span className={css.badgeMuted}>{status.binarySource}</span>}
              </p>
            </div>
          </DisclosureRow>
          <DisclosureRow
            icon={<IconCodeOutline16 size={14} />}
            title={t('engine.log')}
            open={logOpen}
            expandable
            expandOnRowClick
            onToggle={() => {
              setLogOpen(previous => !previous)
            }}
          >
            <div className={css.engineLog}>
              <div className={css.engineLogBar}>
                <span className={css.hint} title={status?.logPath ?? ''}>{truncateMiddle(status?.logPath ?? '', 44)}</span>
                <Button variant="outline" size="sm" disabled={log.loading} onClick={loadLog}>
                  {t('engine.log.refresh')}
                </Button>
              </div>
              {log.error !== null
                ? <p className={css.engineError} role="status">{t('engine.log.error', { kind: log.error })}</p>
                : log.lines.length === 0
                  ? <p className={css.hint}>{log.loading ? t('engine.log.loading') : t('engine.log.empty')}</p>
                  : <pre className={css.engineLogPre}>{log.lines.join('\n')}</pre>}
            </div>
          </DisclosureRow>
        </>
      )}
      <Modal
        open={engine.confirm !== null}
        title={t(engine.confirm === 'restart' ? 'engine.restart.title' : 'engine.stop.title')}
        description={t(engine.confirm === 'restart' ? 'engine.restart.body' : 'engine.stop.body')}
        closeLabel={t('engine.stop.cancel')}
        onClose={() => {
          dispatch({ type: 'engine_confirm_cancel' })
        }}
        footer={(
          <div className={css.modalActions}>
            <Button
              variant="outline"
              size="md"
              onClick={() => {
                dispatch({ type: 'engine_confirm_cancel' })
              }}
            >
              {t('engine.stop.cancel')}
            </Button>
            <Button
              variant="primary"
              size="md"
              onClick={() => {
                void confirmEngineAction(dispatch, api, engine)
              }}
            >
              {t(engine.confirm === 'restart' ? 'engine.restart.confirm' : 'engine.stop.confirm')}
            </Button>
          </div>
        )}
      />
    </section>
  )
}
