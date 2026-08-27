/**
 * One ccteam settings card inside DSH's Plugin configuration tab — the same
 * chrome DSH's own cards draw (ui-settings-plugins `PluginCard` + `fields`):
 * a header naming the plugin and what its settings govern, disclosing the
 * controls in place, a save that writes every staged edit at once. Text
 * fields show the effective value with an "Overridden" badge and reset when
 * the user layer carries them; secret fields are write-only and report only
 * whether a value is configured. Presentation only — the form lives in
 * settings/form.ts and arrives through the injected face.
 */
import { useId, useState } from 'react'
import clsx from 'clsx'
import { Button, IconChevronDownOutline14 } from '@deepseek-ai/dsh-client-ui-primitives'
import type { SettingsCardProps } from '../slots.js'
import css from './settings.module.css'

/**
 * Render one plugin card.
 * @param props - the card face (spec, snapshot hook, actions) and the locale seat.
 * @returns the card, or nothing while the Host does not serve its namespace.
 */
export function SettingsCard({ spec, useCard, edit, reset, save, discard, t }: SettingsCardProps) {
  const state = useCard(snapshot => snapshot)
  const [open, setOpen] = useState(false)
  const baseId = useId()
  if (!state.available) return null
  const title = t(spec.titleKey)
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
          {spec.fields.map((field) => {
            const value = state.fields[field.field] ?? { text: '', overridden: false, configured: false }
            const id = `${baseId}-${field.field}`
            return (
              <div key={field.field} className={css.field}>
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
                            disabled={!state.writable}
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
                  disabled={!state.writable}
                  onChange={(event) => {
                    edit(field.field, event.currentTarget.value)
                  }}
                />
                <p className={css.hint}>{t(field.hintKey)}</p>
              </div>
            )
          })}
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
