import Schema from '@deepseek-ai/schemastery'

export const DEFAULT_DAEMON_URL = 'http://127.0.0.1:7331'
export const SETTINGS_NAMESPACE = 'ccteam-ui'

export const UNCHECKED_STATUS = 'Not checked. If the daemon is unreachable, run: ccteam start'

/**
 * ONE flat namespace for all three faces of this plugin. The base URL is
 * entered once and every face reads it; the two credentials sit on the same
 * card because they are the same person's, one per face:
 *
 *   - `enrollment` identifies THIS DSH PROCESS to ccteam's MCP endpoint, so the
 *     agent's ccteam tools work (`ccteam-enroll:<id>:<secret>`);
 *   - `restToken` identifies the HUMAN's ccteam account, so the workbench can
 *     read their team (`ccteam:<hex>`).
 *
 * They are not interchangeable, which is why both exist and why each hint says
 * where to copy it from.
 *
 * Keys are FLAT on purpose: this schema is also the shape of the row `config`
 * that ccteam's materializer writes into a profile's `cordis.patch.yml`, and
 * Cordis supplies live references to `apply(ctx, config)`. Nesting a key under a
 * namespace makes the value silently undefined (shipped once, v0.10.0).
 */
export interface CcteamSettings {
  daemonUrl: string
  enrollment: string
  restToken: string
  defaultProject: string
  connectionStatus: string
  /**
   * Install the engine and start its daemon when this plugin loads. ON by
   * default: `dsh plugin --profile <name> add @ccteam/ccteam-ui` is meant to be the whole
   * install. Turning it off never STOPS anything — the daemon outlives this
   * plugin either way (PRD D1); it only means the plugin waits to be asked.
   */
  autoStart: boolean
  /**
   * Advanced: an explicit `ccteam` binary. Empty = find it the way the shell
   * does (PATH, then the canonical install path).
   */
  enginePath: string
}

/** Native Config is the single authority; DSH projects volatile fields into its forms. */
export const CCTEAM_SETTINGS_FIELDS = {
  daemonUrl: Schema.string().default(DEFAULT_DAEMON_URL).description('ccteam daemon URL.').volatile(),
  enrollment: Schema.string().default('').role('secret').description('ccteam enrollment credential.').volatile(),
  restToken: Schema.string().default('').role('secret').description('Personal ccteam REST API token.').volatile(),
  defaultProject: Schema.string().default('').description('Default ccteam project slug.').volatile(),
  connectionStatus: Schema.string().default(UNCHECKED_STATUS).volatile(),
  autoStart: Schema.boolean().default(true).description('Install and start the local ccteam engine.').volatile(),
  enginePath: Schema.string().default('').description('Path to the ccteam binary; empty uses PATH.').volatile(),
}
export const CcteamSettingsSchema = Schema.object(CCTEAM_SETTINGS_FIELDS)

export type LiveSettings = ReturnType<typeof CcteamSettingsSchema>

export interface SettingsScope<T> {
  get(): T
  update?(patch: object): Promise<void>
}

/** Read the framework-owned references on every operation; no second settings registry. */
export function bindCcteamSettings(
  ctx: { get?(name: string): unknown; fiber?: { entry?: { id: string } } },
  config: LiveSettings,
): SettingsScope<CcteamSettings> {
  return {
    get: () => ({
      daemonUrl: config.daemonUrl.get(),
      enrollment: config.enrollment.get(),
      restToken: config.restToken.get(),
      defaultProject: config.defaultProject.get(),
      connectionStatus: config.connectionStatus.get(),
      autoStart: config.autoStart.get(),
      enginePath: config.enginePath.get(),
    }),
    async update(patch) {
      const service = ctx.get?.('settings') as import('@deepseek-ai/dsh-settings').SettingsForms | undefined
      const entryId = ctx.fiber?.entry?.id
      if (service === undefined || entryId === undefined) throw new Error('DSH profile settings are unavailable')
      await service.update(entryId, patch)
    },
  }
}
