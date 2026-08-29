/**
 * One ccteam settings card inside DSH's Plugin configuration tab — the same
 * chrome DSH's own cards draw (ui-settings-plugins `PluginCard` + `fields`):
 * a header naming the plugin and what its settings govern, disclosing the
 * controls in place, a save that writes every staged edit at once. The
 * 「引擎」 section sits at the top of the body (engine state, actions, the
 * live auto-start switch, the engine-path override, the log tail); the
 * credential fields follow. Text fields show the effective value with an
 * "Overridden" badge and reset when the user layer carries them; secret
 * fields are write-only and report only whether a value is configured.
 * Presentation only — the form lives in settings/form.ts and arrives through
 * the injected face; the engine slice arrives from the workbench store.
 */
import { useEffect, useId, useState } from 'react'
import clsx from 'clsx'
import { Button, IconChevronDownOutline14 } from '@deepseek-ai/dsh-client-ui-primitives'
import type { SettingsCardProps } from '../slots.js'
import type { T } from '../slots.js'
import { EngineSection } from './EngineSection.js'
import type { CardFieldSpec, CardState, FieldState } from './form.js'
import css from './settings.module.css'

const EMPTY_FIELD: FieldState = { text: '', overridden: false, configured: false }

function TextField({ field, id, value, writable, edit, reset, t }: {
  field: CardFieldSpec
  id: string
  value: FieldState
  writable: boolean
  edit(field: string, text: string): void
  reset(field: string): void
  t: T
}) {
  return (
    <div className={css.field}>
      <div className={css.head}>
        <label className={css.label} htmlFor={id}>{t(field.labelKey)}</label>
        <span className={css.badges}>
          {field.kind === 'secret'
            ? (
                <span className={value.configured ? css.badge : css.badgeMuted}>
                  {t(value.configured ? 'settings.secret.set' : 'settings.secret.unset')}
                </span>
              )
            : value.overridden && (
              <>
                <span className={css.badge}>{t('settings.overridden')}</span>
                <button
                  type="button"
                  className={css.reset}
                  disabled={!writable}
                  onClick={() => {
                    reset(field.field)
                  }}
                >
                  {t('settings.reset')}
                </button>
              </>
            )}
        </span>
      </div>
      <input
        id={id}
        className={css.input}
        type={field.kind === 'secret' ? 'password' : 'text'}
        autoComplete="off"
        value={value.text}
        placeholder={field.placeholder ?? ''}
        disabled={!writable}
        onChange={(event) => {
          edit(field.field, event.currentTarget.value)
        }}
      />
      <p className={css.hint}>{t(field.hintKey)}</p>
    </div>
  )
}

/**
 * Render one plugin card.
 * @param props - the card face (spec, snapshot hooks, actions), the workbench face, and the locale seat.
 * @returns the card, or nothing while the Host does not serve its namespace.
 */
export function SettingsCard({ spec, useCard, useConsole, dispatch, api, edit, reset, save, discard, setToggle, t }: SettingsCardProps) {
  const state: CardState = useCard(snapshot => snapshot)
  const engine = useConsole(snapshot => snapshot.engine)
  const [open, setOpen] = useState(false)
  const baseId = useId()
  // The engine is polled only while somebody looks at it: this card, expanded.
  useEffect(() => {
    if (!open) return
    dispatch({ type: 'engine_watch' })
    return () => {
      dispatch({ type: 'engine_unwatch' })
    }
  }, [open, dispatch])
  if (!state.available) return null
  const title = t(spec.titleKey)
  const credentialFields = spec.fields.filter(field => field.section !== 'engine' && field.kind !== 'toggle')
  const enginePathSpec = spec.fields.find(field => field.field === 'enginePath')
  return (
    <li className={clsx(css.card, open && css.cardOpen)} data-ccteam-console="">
      <button
        type="button"
        className={css.header}
        aria-expanded={open}
        aria-label={`${t(open ? 'settings.collapse' : 'settings.expand')}: ${title}`}
        onClick={() => {
          setOpen(!open)
        }}
      >
        <span className={css.headText}>
          <span className={css.name}>{title}</span>
          <span className={css.description}>{t(spec.descriptionKey)}</span>
        </span>
        {state.dirty && <span className={css.pending}>{t('settings.unsaved')}</span>}
        <IconChevronDownOutline14 className={clsx(css.chevron, open && css.chevronOpen)} />
      </button>
      {open && (
        <div className={css.body}>
          {!state.writable && <p className={css.readOnly} role="status">{t('settings.readOnly')}</p>}
          <EngineSection
            t={t}
            engine={engine}
            api={api}
            dispatch={dispatch}
            autoStart={state.toggles.autoStart ?? true}
            onAutoStart={(next) => {
              setToggle('autoStart', next)
            }}
            enginePath={{
              id: `${baseId}-enginePath`,
              value: state.fields.enginePath ?? EMPTY_FIELD,
              writable: state.writable,
              placeholder: enginePathSpec?.placeholder ?? '',
              onEdit: (text) => {
                edit('enginePath', text)
              },
              onReset: () => {
                reset('enginePath')
              },
            }}
          />
          {credentialFields.map(field => (
            <TextField
              key={field.field}
              field={field}
              id={`${baseId}-${field.field}`}
              value={state.fields[field.field] ?? EMPTY_FIELD}
              writable={state.writable}
              edit={edit}
              reset={reset}
              t={t}
            />
          ))}
          <div className={css.footer}>
            {state.failed && <p className={css.failed} role="status">{t('settings.saveFailed')}</p>}
            <Button variant="outline" size="sm" disabled={!state.dirty || state.saving} onClick={discard}>
              {t('settings.discard')}
            </Button>
            <Button variant="primary" size="sm" disabled={!state.dirty || state.saving || !state.writable} onClick={save}>
              {t(state.saving ? 'settings.saving' : 'settings.save')}
            </Button>
          </div>
        </div>
      )}
    </li>
  )
}
