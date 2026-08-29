import Schema from '@deepseek-ai/schemastery'

export const DEFAULT_DAEMON_URL = 'http://127.0.0.1:7331'
export const SETTINGS_NAMESPACE = 'ccteam-ui'

export const UNCHECKED_STATUS = 'Not checked. If the daemon is unreachable, run: ccteam start'
const NO_SERVICE_STATUS = 'Settings service unavailable. If the daemon is unreachable, run: ccteam start'

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
 * that config reaches `apply(ctx, config)` verbatim. Nesting a key under a
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

export const CcteamSettingsSchema: Schema<CcteamSettings> = Schema.object({
  daemonUrl: Schema.string().default(DEFAULT_DAEMON_URL).description('ccteam daemon URL.'),
  enrollment: Schema.string()
    .default('')
    .role('secret')
    .description('Enrollment credential from ccteam config (tool surface).'),
  restToken: Schema.string()
    .default('')
    .role('secret')
    .description('Personal ccteam REST API token (ccteam web console → Account).'),
  defaultProject: Schema.string()
    .default('')
    .description('Project slug new sessions land in when the panel does not name one.'),
  connectionStatus: Schema.string().default(UNCHECKED_STATUS),
  autoStart: Schema.boolean()
    .default(true)
    .description('Install the ccteam engine and start its daemon when this plugin loads.'),
  enginePath: Schema.string()
    .default('')
    .description('Advanced: path to the ccteam binary. Empty = PATH, then the canonical install path.'),
})

export interface SettingsScope<T> {
  get(): T
  watch?(callback: (next: T, prev: T) => void | Promise<void>): () => void
  /**
   * Merge a patch into the user layer. Present on the real service
   * (`@deepseek-ai/dsh-settings`); optional here because the no-service
   * fallback below has nowhere to write.
   */
  update?(patch: object): Promise<void>
}

export interface SettingsContext {
  settings?: {
    register<T>(
      ns: string,
      schema: Schema<T>,
      options?: { applies?: 'live' | 'restart'; base?: Partial<T> },
    ): SettingsScope<T>
  }
}

/**
 * Register the settings card. Credentials live here and in closure scope only:
 * they are never written to `process.env`, never logged, and never leave the
 * host half (see bff.ts and tools.ts).
 */
export function registerCcteamSettings(
  ctx: SettingsContext,
  base?: Partial<CcteamSettings>,
): SettingsScope<CcteamSettings> {
  if (ctx.settings === undefined) {
    return {
      get: () => ({
        daemonUrl: base?.daemonUrl ?? DEFAULT_DAEMON_URL,
        enrollment: base?.enrollment ?? '',
        restToken: base?.restToken ?? '',
        defaultProject: base?.defaultProject ?? '',
        connectionStatus: base?.connectionStatus ?? NO_SERVICE_STATUS,
        autoStart: base?.autoStart ?? true,
        enginePath: base?.enginePath ?? '',
      }),
    }
  }
  return ctx.settings.register(SETTINGS_NAMESPACE, CcteamSettingsSchema, {
    // `live` for the panel's own reads (the BFF resolves through closures on
    // every request); the tool and transport faces read their credential per
    // call, so an edit reaches them without a restart too.
    applies: 'live',
    base,
  })
}
