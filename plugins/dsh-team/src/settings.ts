import Schema from '@deepseek-ai/schemastery'

export const DEFAULT_DAEMON_URL = 'http://127.0.0.1:7331'
export const SETTINGS_NAMESPACE = 'ccteam-team'

export const UNCHECKED_STATUS = 'Not checked. If the daemon is unreachable, run: ccteam start'
const NO_SERVICE_STATUS = 'Settings service unavailable. If the daemon is unreachable, run: ccteam start'

export interface CcteamTeamSettings {
  daemonUrl: string
  restToken: string
  defaultProject: string
  connectionStatus: string
}

export const CcteamTeamSettingsSchema: Schema<CcteamTeamSettings> = Schema.object({
  daemonUrl: Schema.string().default(DEFAULT_DAEMON_URL).description('ccteam daemon URL.'),
  restToken: Schema.string()
    .default('')
    .role('secret')
    .description('Personal ccteam REST API token (ccteam web console → Account).'),
  defaultProject: Schema.string()
    .default('')
    .description('Project slug new sessions land in when the panel does not name one.'),
  connectionStatus: Schema.string().default(UNCHECKED_STATUS),
})

export interface SettingsScope<T> {
  get(): T
  watch?(callback: (next: T, prev: T) => void | Promise<void>): () => void
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
 * Register the settings card. The token lives here and in closure scope only:
 * it is never written to `process.env`, never logged, and never leaves the host
 * half (see bff.ts).
 */
export function registerCcteamTeamSettings(
  ctx: SettingsContext,
  base?: Partial<CcteamTeamSettings>,
): SettingsScope<CcteamTeamSettings> {
  if (ctx.settings === undefined) {
    return {
      get: () => ({
        daemonUrl: base?.daemonUrl ?? DEFAULT_DAEMON_URL,
        restToken: base?.restToken ?? '',
        defaultProject: base?.defaultProject ?? '',
        connectionStatus: base?.connectionStatus ?? NO_SERVICE_STATUS,
      }),
    }
  }
  return ctx.settings.register(SETTINGS_NAMESPACE, CcteamTeamSettingsSchema, {
    applies: 'live',
    base,
  })
}
