/**
 * The spawn view: vendor pills (not-installed vendors greyed out when the
 * daemon reports availability), model/effort/mode collapsed under an
 * Advanced disclosure (empty = vendor defaults), an optional first task.
 * Enter creates, Esc bubbles to the panel = cancel. Presentation only — the
 * panel owns the spawn call and the jump into the new session's chat.
 */
import { useState } from 'react'
import { Button, DisclosureRow, IconSettingsOutline14, Input, Pill } from '@deepseek-ai/dsh-client-ui-primitives'
import type { SpawnRequest, VendorAvailability } from '../shared/contract.js'
import { VENDORS } from './store.js'
import type { T } from './slots.js'
import css from './panel.module.css'

/** Spawn view props. */
export interface SpawnFormProps {
  vendors: VendorAvailability[]
  /** Known project slugs (team.graph order). One = auto-picked and hidden. */
  projects: string[]
  /** Last project spawned into (persisted); preselects when still known. */
  lastProject: string | null
  busy: boolean
  error: string | null
  t: T
  onCreate(request: SpawnRequest): void
  onCancel(): void
}

function installedSet(vendors: VendorAvailability[]): ReadonlySet<string> | undefined {
  if (vendors.length === 0) return undefined
  return new Set(vendors.filter(v => v.installed).map(v => v.vendor))
}

/**
 * Render the spawn form.
 * @param props - availability + busy/error state + the create action.
 * @returns the spawn view body.
 */
export function SpawnForm({ vendors, projects, lastProject, busy, error, t, onCreate, onCancel }: SpawnFormProps) {
  const installed = installedSet(vendors)
  const known = VENDORS.filter(v => installed === undefined || installed.has(v))
  const [vendor, setVendor] = useState<string>(() => known[0] ?? VENDORS[0]!)
  // Exactly one known project: auto-picked, control hidden. Several: the
  // remembered project preselects when still known; none picked = the host
  // decides (its configured default, or an actionable error shown below).
  const [project, setProject] = useState<string | undefined>(() =>
    projects.length === 1 ? projects[0] : lastProject !== null && projects.includes(lastProject) ? lastProject : undefined)
  const [advancedOpen, setAdvancedOpen] = useState(false)
  const [model, setModel] = useState('')
  const [effort, setEffort] = useState('')
  const [mode, setMode] = useState('')
  const [task, setTask] = useState('')

  const submit = (): void => {
    if (busy) return
    const request: SpawnRequest = { vendor }
    const chosen = projects.length === 1 ? projects[0] : project
    if (chosen !== undefined) request.project = chosen
    if (model.trim() !== '') request.model = model.trim()
    if (effort.trim() !== '') request.effort = effort.trim()
    if (mode.trim() !== '') request.mode = mode.trim()
    if (task.trim() !== '') request.task = task.trim()
    onCreate(request)
  }

  const submitOnEnter = (event: { key: string; preventDefault(): void }): void => {
    if (event.key === 'Enter') {
      event.preventDefault()
      submit()
    }
  }

  return (
    <div className={css.scroll}>
      <div className={css.form}>
        {projects.length > 1 && (
          <div>
            <div className={css.fieldLabel}>{t('spawn.project')}</div>
            <div className={css.vendorPills}>
              {projects.map(slug => (
                <Pill
                  key={slug}
                  active={slug === project}
                  onClick={() => {
                    setProject(slug)
                  }}
                >
                  {slug}
                </Pill>
              ))}
            </div>
          </div>
        )}
        <div>
          <div className={css.fieldLabel}>{t('spawn.vendor')}</div>
          <div className={css.vendorPills}>
            {VENDORS.map((candidate) => {
              const missing = installed !== undefined && !installed.has(candidate)
              return (
                <Pill
                  key={candidate}
                  active={candidate === vendor}
                  className={missing ? css.vendorMissing : undefined}
                  disabled={missing}
                  title={missing ? `${candidate} — ${t('spawn.vendor.missing')}` : candidate}
                  onClick={() => {
                    if (!missing) setVendor(candidate)
                  }}
                >
                  {candidate}
                </Pill>
              )
            })}
          </div>
        </div>

        <DisclosureRow
          icon={<IconSettingsOutline14 size={14} />}
          title={t('spawn.advanced')}
          open={advancedOpen}
          expandable
          expandOnRowClick
          onToggle={() => {
            setAdvancedOpen(previous => !previous)
          }}
        >
          <div className={css.advancedBody}>
            <label className={css.advancedField}>
              <span className={css.advancedFieldLabel}>{t('spawn.model')}</span>
              <Input
                className={css.advancedInput}
                value={model}
                onChange={event => setModel(event.currentTarget.value)}
                onKeyDown={submitOnEnter}
              />
            </label>
            <label className={css.advancedField}>
              <span className={css.advancedFieldLabel}>{t('spawn.effort')}</span>
              <Input
                className={css.advancedInput}
                value={effort}
                onChange={event => setEffort(event.currentTarget.value)}
                onKeyDown={submitOnEnter}
              />
            </label>
            <label className={css.advancedField}>
              <span className={css.advancedFieldLabel}>{t('spawn.mode')}</span>
              <Input
                className={css.advancedInput}
                value={mode}
                onChange={event => setMode(event.currentTarget.value)}
                onKeyDown={submitOnEnter}
              />
            </label>
            <div className={css.fieldHint}>{t('spawn.defaults.hint')}</div>
          </div>
        </DisclosureRow>

        <div>
          <div className={css.fieldLabel}>{t('spawn.task')}</div>
          <textarea
            className={`${css.composerInput} ${css.taskInput}`}
            placeholder={t('spawn.task.placeholder')}
            value={task}
            onChange={event => setTask(event.currentTarget.value)}
            onKeyDown={(event) => {
              if (event.key === 'Enter' && !event.shiftKey && !event.nativeEvent.isComposing) {
                event.preventDefault()
                submit()
              }
            }}
          />
        </div>

        {error !== null && <div className={css.formError} role="alert">{`${t('spawn.error')} — ${error}`}</div>}

        <div className={css.formFoot}>
          <Button variant="outline" size="md" onClick={onCancel}>
            {t('spawn.cancel')}
          </Button>
          <Button variant="primary" size="md" disabled={busy} onClick={submit}>
            {t('spawn.create')}
          </Button>
        </div>
      </div>
    </div>
  )
}
