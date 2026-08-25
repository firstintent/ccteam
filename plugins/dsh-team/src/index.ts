import Schema from '@deepseek-ai/schemastery'

export const name = 'ccteam-team'
export const inject = ['webServer', 'settings']

export interface Config {
  daemonUrl?: string
  restToken?: string
  connectionStatus?: string
}

export const Config: Schema<Config> = Schema.object({
  daemonUrl: Schema.string()
    .default('http://127.0.0.1:7331')
    .description('ccteam daemon URL.'),
  restToken: Schema.string()
    .default('')
    .role('secret')
    .description('Personal ccteam REST API token (web console → Account).'),
  connectionStatus: Schema.string().default(
    'Not checked. If the daemon is unreachable, run: ccteam start',
  ),
})

export interface ApplyContext {
  webServer: {
    register(route: {
      kind: 'exact' | 'prefix'
      path: string
      handler: (req: unknown, res: unknown) => void | Promise<void>
    }): () => void
  }
  settings?: {
    register<T>(
      ns: string,
      schema: Schema<T>,
      options?: { applies?: 'live' | 'restart'; base?: Partial<T> },
    ): { get(): T }
  }
  effect?<T extends (() => void | Promise<void>) | void>(
    setup: () => T,
    label?: string,
  ): () => void
  logger?: { warn(message: string): void }
}

/**
 * Host half: one prefix route under API_PREFIX serving the BFF (method
 * dispatch + SSE fan-out; see src/shared/contract.ts) plus the settings card.
 * Implementation lands in bff.ts — this stub only fixes the wiring shape.
 */
export function apply(_ctx: ApplyContext, _config: Config = {}): void {
  // TODO(DSH2-HOST): registerCcteamSettings + registerBff(ctx, config)
}
