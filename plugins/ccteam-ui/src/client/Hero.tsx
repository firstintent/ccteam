/**
 * The new-session hero — DSH's own "empty conversation" shape: a mark and a
 * title, one row of pickers (project / vendor / role), and the composer with
 * the model·effort picker in its bar. Typing the first task and pressing
 * Enter creates the session and sends it; validation is inline and the
 * daemon's own error is shown verbatim under the box. Presentation only —
 * the workbench owns the spawn call.
 */
import { useState } from 'react'
import {
  IconAgentPresetOutline16,
  IconBranchOutline16,
  IconFolderOpen16,
  IconSparkle16,
  IconUserOutline16,
} from '@deepseek-ai/dsh-client-ui-primitives'
import type { ModelsCatalog, ProjectInfo, SpawnRequest, VendorAvailability } from '../shared/contract.js'
import { titleFromTask } from './format.js'
import { Composer } from './Composer.js'
import type { ComposerAttachment } from './Composer.js'
import { Picker } from './Pickers.js'
import { VENDORS, effortsFor } from './store.js'
import type { SpawnDraft } from './store.js'
import type { T } from './slots.js'
import css from './workbench.module.css'

export interface HeroProps {
  t: T
  draft: SpawnDraft
  projects: ProjectInfo[] | null
  vendors: VendorAvailability[]
  models: ModelsCatalog | null
  roles: string[]
  busy: boolean
  error: string | null
  attachments: ComposerAttachment[]
  onDraft(draft: Partial<SpawnDraft>): void
  onAttach(files: File[]): void
  onRemoveAttachment(key: string): void
  onCreate(request: SpawnRequest): void
}

function installedSet(vendors: VendorAvailability[]): ReadonlySet<string> | undefined {
  if (vendors.length === 0) return undefined
  return new Set(vendors.filter(v => v.installed).map(v => v.vendor))
}

/**
 * Render the hero.
 * @param props - draft, catalogs, availability and the create action.
 * @returns the hero body.
 */
export function Hero(props: HeroProps) {
  const { t, draft, projects, models, roles, busy, error } = props
  const [task, setTask] = useState('')
  const [validation, setValidation] = useState<string | null>(null)
  const installed = installedSet(props.vendors)

  const projectOptions = (projects ?? []).map(project => ({
    id: project.slug,
    label: project.slug,
    ...(project.host !== undefined && project.host !== 'local' ? { meta: project.host } : {}),
  }))
  const vendorOptions = VENDORS.map(vendor => {
    const missing = installed !== undefined && !installed.has(vendor)
    return { id: vendor, label: vendor, disabled: missing, ...(missing ? { meta: t('spawn.vendor.missing') } : {}) }
  })
  const vendorRow = draft.vendor === null || models === null ? undefined : models.vendors.find(v => v.vendor === draft.vendor)
  const modelOptions = (vendorRow?.models ?? []).map(model => ({
    id: model.id,
    label: model.displayName ?? model.id,
    ...(model.displayName !== undefined && model.displayName !== model.id ? { meta: model.id } : {}),
  }))
  const efforts = effortsFor(models, draft.vendor, draft.model)
  const roleOptions = roles.map(role => ({ id: role, label: role }))

  const submit = (): void => {
    if (busy) return
    if (draft.project === null) {
      setValidation(t('spawn.needProject'))
      return
    }
    if (draft.vendor === null) {
      setValidation(t('spawn.needVendor'))
      return
    }
    const text = task.trim()
    if (text === '') {
      setValidation(t('spawn.needTask'))
      return
    }
    setValidation(null)
    const uploaded = props.attachments.filter(a => a.path !== undefined && a.error === undefined)
    const request: SpawnRequest = {
      project: draft.project,
      vendor: draft.vendor,
      task: text,
      ...(draft.model === null ? {} : { model: draft.model }),
      ...(draft.effort === null ? {} : { effort: draft.effort }),
      ...(draft.role === null ? {} : { role: draft.role }),
      ...(uploaded.length === 0 ? {} : { attachments: uploaded.map(a => ({ kind: a.kind, path: a.path! })) }),
    }
    const title = titleFromTask(text)
    if (title !== undefined) request.title = title
    props.onCreate(request)
    setTask('')
  }

  const noProjects = projects !== null && projects.length === 0

  return (
    <div className={css.hero}>
      <div className={css.heroInner}>
        <div className={css.heroMark}>
          <IconBranchOutline16 size={26} />
          <span>{t('hero.title')}</span>
        </div>
        <div className={css.heroSubtitle}>{noProjects ? t('hero.noProjects.body') : t('hero.subtitle')}</div>
        <div className={css.pickerRow}>
          <Picker
            icon={<IconFolderOpen16 size={14} />}
            label={t('spawn.project')}
            value={draft.project}
            placeholder={noProjects ? t('hero.noProjects.title') : '…'}
            options={projectOptions}
            disabled={busy}
            onChange={(id) => {
              props.onDraft({ project: id })
            }}
          />
          <Picker
            icon={<IconSparkle16 size={14} />}
            label={t('spawn.vendor')}
            value={draft.vendor}
            placeholder="…"
            options={vendorOptions}
            disabled={busy}
            onChange={(id) => {
              props.onDraft({ vendor: id })
            }}
          />
          <Picker
            icon={<IconUserOutline16 size={14} />}
            label={t('spawn.role')}
            value={draft.role}
            placeholder={t('spawn.role.none')}
            options={roleOptions}
            disabled={busy || draft.project === null}
            clearLabel={t('spawn.role.none')}
            onChange={(id) => {
              props.onDraft({ role: id })
            }}
          />
        </div>
        <Composer
          t={t}
          draft={task}
          onDraftChange={setTask}
          onSubmit={submit}
          busy={busy}
          placeholder={t('hero.placeholder')}
          attachments={props.attachments}
          onAttach={props.onAttach}
          onRemoveAttachment={props.onRemoveAttachment}
          error={validation ?? (error === null ? null : `${t('spawn.error')} — ${error}`)}
          hint={t('chat.hint')}
          autoFocus
          trailing={(
            <>
              <Picker
                icon={<IconAgentPresetOutline16 size={14} />}
                label={t('spawn.model')}
                value={draft.model}
                placeholder={t('spawn.model.default')}
                options={modelOptions}
                disabled={busy || draft.vendor === null}
                clearLabel={t('spawn.model.default')}
                align="end"
                onChange={(id) => {
                  props.onDraft({ model: id })
                }}
              />
              {efforts.length > 0 && (
                <Picker
                  label={t('spawn.effort')}
                  value={draft.effort}
                  placeholder={t('spawn.effort.default')}
                  options={efforts.map(effort => ({ id: effort, label: effort }))}
                  disabled={busy}
                  clearLabel={t('spawn.effort.default')}
                  align="end"
                  onChange={(id) => {
                    props.onDraft({ effort: id })
                  }}
                />
              )}
            </>
          )}
        />
      </div>
    </div>
  )
}
