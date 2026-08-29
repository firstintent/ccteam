/**
 * The staged form behind the ccteam settings card, the way DSH's own
 * configurable-plugin cards stage theirs (ui-settings-plugins `card-form.ts`):
 * a card stages what the user types and writes it only on save, each field
 * shows its effective value and whether the user layer carries it, and the
 * Host stays the only authority on what was accepted — the outcome is read
 * back from the scope after the writes, never predicted.
 *
 * Framework-neutral: the controller is a bare observable (getSnapshot +
 * subscribe), which is exactly what the slot framework binds into the card's
 * `useCard` selector hook. Nothing here touches React or the wire directly;
 * the bound `SettingsScope` is the transport.
 */
import type { SettingsScope } from '@deepseek-ai/dsh-client-runtime/client'
import type { CcteamLocaleKey } from '../locales.js'

/** One control of a card. */
export interface CardFieldSpec {
  /** Field name inside the namespace section. */
  field: string
  /**
   * `text` renders the effective value; `secret` is write-only — it renders
   * blank, reports only whether a value is configured, and a blank draft
   * writes nothing (keeps the stored value); `toggle` is a live boolean —
   * flipping it writes at once, there is no save step.
   */
  kind: 'text' | 'secret' | 'toggle'
  labelKey: CcteamLocaleKey
  hintKey: CcteamLocaleKey
  placeholder?: string
  /** Where the card draws it: the credential list (default) or the engine section. */
  section?: 'engine'
  /** What a toggle shows while no layer supplies a value. */
  defaultValue?: boolean
}

/** One card: the namespace it edits and the controls it shows. */
export interface CardSpec {
  namespace: string
  titleKey: CcteamLocaleKey
  descriptionKey: CcteamLocaleKey
  fields: readonly CardFieldSpec[]
}

/** One field as the card renders it. */
export interface FieldState {
  /** Draft text the control renders (blank for a secret without a staged draft). */
  text: string
  /** Whether saving would leave a user-layer entry for this field (text fields). */
  overridden: boolean
  /** Whether any layer supplies a non-empty value (secret fields). */
  configured: boolean
}

/** What one card renders. */
export interface CardState {
  /** False while the Host does not serve the namespace: the card renders nothing. */
  available: boolean
  /** Whether the Host document accepts writes. */
  writable: boolean
  /** Whether the form holds edits a save would write. */
  dirty: boolean
  /** Whether a save is crossing the wire. */
  saving: boolean
  /** Whether the last save did not land as staged; cleared by the next edit, discard, or successful save. */
  failed: boolean
  fields: Record<string, FieldState>
  /** Live toggles by field (effective value, or the spec default). */
  toggles: Record<string, boolean>
}

/** The write actions a card's slot entry injects. */
export interface CardActions {
  /** Stage draft text for one field. */
  edit(field: string, text: string): void
  /** Stage a clear, so saving lets the field re-inherit the composition layer. */
  reset(field: string): void
  /** Write every staged edit, then re-read what the Host accepted. */
  save(): void
  /** Drop every staged edit. */
  discard(): void
  /** Write one live toggle at once (no staging). */
  setToggle(field: string, value: boolean): void
}

/** The registration-side face one card's slot entry injects (`hooks.card` → `useCard`). */
export interface SettingsCardFace extends CardActions {
  hooks: { card: SettingsCardController }
  spec: CardSpec
}

/** The one settings namespace this plugin serves (src/settings.ts). */
export const CCTEAM_NS = 'ccteam-ui'

/**
 * The single ccteam card: base URL once, then one credential per face — the
 * personal REST token the workbench reads the team with, and the enrollment
 * credential a DSH agent calls the ccteam tools with. They are different
 * credentials for different callers, so both live here rather than one
 * standing in for the other.
 */
export const CCTEAM_CARD: CardSpec = {
  namespace: CCTEAM_NS,
  titleKey: 'settings.card.title',
  descriptionKey: 'settings.card.description',
  fields: [
    { field: 'daemonUrl', kind: 'text', labelKey: 'settings.field.daemonUrl', hintKey: 'settings.field.daemonUrl.hint', placeholder: 'http://127.0.0.1:7331' },
    { field: 'restToken', kind: 'secret', labelKey: 'settings.field.restToken', hintKey: 'settings.field.restToken.hint' },
    { field: 'enrollment', kind: 'secret', labelKey: 'settings.field.enrollment', hintKey: 'settings.field.enrollment.hint' },
    { field: 'defaultProject', kind: 'text', labelKey: 'settings.field.defaultProject', hintKey: 'settings.field.defaultProject.hint' },
    // The engine section's two controls (drawn there, not in the list above).
    { field: 'autoStart', kind: 'toggle', labelKey: 'engine.autoStart', hintKey: 'engine.autoStart.hint', section: 'engine', defaultValue: true },
    { field: 'enginePath', kind: 'text', labelKey: 'engine.enginePath', hintKey: 'engine.enginePath.hint', section: 'engine', placeholder: '/usr/local/bin/ccteam' },
  ],
}

/** The section shape every ccteam namespace shares: flat string fields. */
export type Section = Record<string, unknown>

function stringAt(section: unknown, field: string): string {
  if (section === null || typeof section !== 'object') return ''
  const value = (section as Section)[field]
  return typeof value === 'string' ? value : ''
}

function booleanAt(section: unknown, field: string): boolean | undefined {
  if (section === null || typeof section !== 'object') return undefined
  const value = (section as Section)[field]
  return typeof value === 'boolean' ? value : undefined
}

function hasOwn(section: unknown, field: string): boolean {
  return section !== null && typeof section === 'object' && Object.prototype.hasOwnProperty.call(section, field)
}

/** Stages one card's edits over one settings namespace and writes them on save. */
export class SettingsCardController {
  private readonly staged = new Map<string, string>()
  private readonly listeners = new Set<() => void>()
  private saving = false
  private failed = false
  private snapshot: CardState

  /**
   * @param scope - the bound settings scope for this card's namespace.
   * @param spec - the card's namespace and controls.
   */
  constructor(
    private readonly scope: SettingsScope<Section>,
    readonly spec: CardSpec,
  ) {
    this.snapshot = this.project()
    scope.subscribe(() => {
      this.publish()
    })
  }

  /** @returns the current card state (stable reference until the next change). */
  getSnapshot(): CardState {
    return this.snapshot
  }

  /**
   * Observe state replacements.
   * @param listener - invoked after each change.
   * @returns the disposer.
   */
  subscribe(listener: () => void): () => void {
    this.listeners.add(listener)
    return () => {
      this.listeners.delete(listener)
    }
  }

  /**
   * Build the face the card's slot registration injects.
   * @returns the card's snapshot source, its spec, and its form actions.
   */
  inject(): SettingsCardFace {
    return {
      hooks: { card: this },
      spec: this.spec,
      edit: (field, text) => {
        this.staged.set(field, text)
        this.failed = false
        this.publish()
      },
      reset: (field) => {
        this.staged.set(field, '')
        this.failed = false
        this.publish()
      },
      save: () => {
        void this.save()
      },
      discard: () => {
        if (this.staged.size === 0 && !this.failed) return
        this.staged.clear()
        this.failed = false
        this.publish()
      },
      setToggle: (field, value) => {
        void this.setToggle(field, value)
      },
    }
  }

  /**
   * Write one live toggle at once: the Host either takes the value — read
   * back, never assumed — or the switch stays where it was and the card
   * reports the failure.
   * @param field - the toggle's field.
   * @param value - the new value.
   * @returns settlement after the write and the read-back.
   */
  async setToggle(field: string, value: boolean): Promise<void> {
    if (this.saving) return
    this.saving = true
    this.failed = false
    this.publish()
    let threw = false
    try {
      await this.scope.set(field, value)
    } catch {
      threw = true
    }
    this.failed = threw || booleanAt(this.scope.getSnapshot().value, field) !== value
    this.saving = false
    this.publish()
  }

  /**
   * Write every staged edit, then re-read what the Host accepted. A save that
   * did not land keeps its drafts so the user can correct them.
   * @returns settlement after the writes and the read-back.
   */
  async save(): Promise<void> {
    if (this.saving || this.staged.size === 0) return
    const plan = [...this.staged.entries()].map(([field, text]) => ({ field, text: text.trim(), kind: this.kindOf(field) }))
    this.saving = true
    this.publish()
    let threw = false
    for (const item of plan) {
      if (item.kind === 'secret' && item.text === '') continue
      try {
        if (item.text === '') await this.scope.unset(item.field)
        else await this.scope.set(item.field, item.text)
      } catch {
        // The scope's own contract is to reload Host state on a failed
        // write; the read-back below is what decides whether this save landed.
        threw = true
      }
    }
    const after = this.scope.getSnapshot()
    const landed = plan.every((item) => {
      if (item.kind === 'secret') return item.text === '' || stringAt(after.value, item.field) !== ''
      if (item.text === '') return !hasOwn(after.user, item.field)
      return stringAt(after.value, item.field) === item.text
    })
    this.failed = threw || !landed
    if (!this.failed) this.staged.clear()
    this.saving = false
    this.publish()
  }

  private kindOf(field: string): CardFieldSpec['kind'] {
    return this.spec.fields.find(candidate => candidate.field === field)?.kind ?? 'text'
  }

  private publish(): void {
    this.snapshot = this.project()
    for (const listener of [...this.listeners]) listener()
  }

  private project(): CardState {
    const snapshot = this.scope.getSnapshot()
    const fields: Record<string, FieldState> = {}
    const toggles: Record<string, boolean> = {}
    for (const spec of this.spec.fields) {
      if (spec.kind === 'toggle') {
        toggles[spec.field] = booleanAt(snapshot.value, spec.field) ?? spec.defaultValue ?? false
        continue
      }
      const staged = this.staged.get(spec.field)
      const current = stringAt(snapshot.value, spec.field)
      if (spec.kind === 'secret') {
        fields[spec.field] = { text: staged ?? '', overridden: false, configured: current !== '' }
        continue
      }
      fields[spec.field] = {
        text: staged ?? current,
        overridden: staged === undefined ? hasOwn(snapshot.user, spec.field) : staged.trim() !== '',
        configured: (staged ?? current) !== '',
      }
    }
    return {
      available: snapshot.status === 'ready',
      writable: snapshot.writable,
      dirty: this.staged.size > 0,
      saving: this.saving,
      failed: this.failed,
      fields,
      toggles,
    }
  }
}
