import Schema from '@deepseek-ai/schemastery'

export const DEFAULT_DAEMON_URL = 'http://127.0.0.1:7331'
export const SETTINGS_NAMESPACE = 'ccteam-client'

export interface CcteamSettings {
  daemonUrl: string
  enrollment: string
  connectionStatus: string
  boundProject: string
}

export const CcteamSettingsSchema: Schema<CcteamSettings> = Schema.object({
  daemonUrl: Schema.string().default(DEFAULT_DAEMON_URL).description('ccteam daemon URL.'),
  enrollment: Schema.string().default('').role('secret').description('Enrollment credential from ccteam config.'),
  connectionStatus: Schema.string().default('Not checked. If the daemon is unreachable, run: ccteam start'),
  boundProject: Schema.string().default(''),
})

export interface SettingsScope<T> {
  get(): T
  watch?(callback: (next: T, prev: T) => void | Promise<void>): () => void
}

export interface SettingsContext {
  settings?: {
    register<T>(ns: string, schema: Schema<T>, options?: { applies?: 'live' | 'restart'; base?: Partial<T> }): SettingsScope<T>
  }
}

export function registerCcteamSettings(ctx: SettingsContext, base?: Partial<CcteamSettings>): SettingsScope<CcteamSettings> {
  if (ctx.settings === undefined) {
    return {
      get: () => ({
        daemonUrl: base?.daemonUrl ?? DEFAULT_DAEMON_URL,
        enrollment: base?.enrollment ?? '',
        connectionStatus: base?.connectionStatus ?? 'Settings service unavailable. If the daemon is unreachable, run: ccteam start',
        boundProject: base?.boundProject ?? '',
      }),
    }
  }
  return ctx.settings.register(SETTINGS_NAMESPACE, CcteamSettingsSchema, {
    applies: 'restart',
    base,
  })
}
