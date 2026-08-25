import Schema from '@deepseek-ai/schemastery'
import { registerBff, type BffContext } from './bff.js'
import {
  DEFAULT_DAEMON_URL,
  UNCHECKED_STATUS,
  registerCcteamTeamSettings,
  type CcteamTeamSettings,
} from './settings.js'

export const name = 'ccteam-team'
export const inject = ['webServer', 'settings']

export interface Config extends Partial<CcteamTeamSettings> {}

export const Config: Schema<Config> = Schema.object({
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

export interface ApplyContext extends BffContext {
  settings?: {
    register<T>(
      ns: string,
      schema: Schema<T>,
      options?: { applies?: 'live' | 'restart'; base?: Partial<T> },
    ): { get(): T }
  }
}

/**
 * Host half: the settings card plus ONE prefix route under API_PREFIX serving
 * the BFF (method dispatch + SSE fan-out; see src/shared/contract.ts).
 *
 * Precedence is config-over-settings, matching @ccteam/dsh-client: a value
 * pinned in cordis.yml wins, otherwise the user's settings card decides. Both
 * are read through closures on every request, so editing the card takes effect
 * without a restart. The token never leaves this closure.
 */
export function apply(ctx: ApplyContext, config: Config = {}): void {
  const settings = registerCcteamTeamSettings(ctx, {
    daemonUrl: config.daemonUrl,
    restToken: config.restToken,
    defaultProject: config.defaultProject,
    connectionStatus: config.connectionStatus,
  })
  registerBff(ctx, {
    daemonUrl: () => config.daemonUrl ?? settings.get().daemonUrl ?? DEFAULT_DAEMON_URL,
    restToken: () => config.restToken ?? settings.get().restToken ?? '',
    defaultProject: () => config.defaultProject ?? settings.get().defaultProject ?? '',
    logger: ctx.logger,
  })
}
