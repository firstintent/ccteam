import Schema from '@deepseek-ai/schemastery'
import { SessionCredentialStore } from './credentials.js'
import { CcteamCompletionNotifier, CcteamMcpClientPool, registerCcteamTools } from './tools.js'
import { DEFAULT_DAEMON_URL, registerCcteamSettings, type CcteamSettings } from './settings.js'
import { startDshSocketTransport } from './transport.js'

export const name = 'ccteam-client'
export const inject = ['agents', 'tools', 'settings', 'agentDefaultModel']

export interface Config extends Partial<CcteamSettings> {
  completionPollIntervalMs?: number
  completionMaxPolls?: number
  transportSocket?: string
}

export const Config: Schema<Config> = Schema.object({
  daemonUrl: Schema.string().default(DEFAULT_DAEMON_URL).description('ccteam daemon URL.'),
  enrollment: Schema.string().default('').role('secret').description('Enrollment credential from ccteam config.'),
  connectionStatus: Schema.string().default('Not checked. If the daemon is unreachable, run: ccteam start'),
  boundProject: Schema.string().default(''),
  completionPollIntervalMs: Schema.number().default(5000),
  completionMaxPolls: Schema.number().default(720),
  transportSocket: Schema.string().description('Unix socket path this plugin serves ACP on for ccteam. Empty = tool surface only.'),
})

const PACKAGE_VERSION = '0.10.3-alpha.0'

export interface ApplyContext {
  tools: {
    register(definition: unknown): () => void
  }
  agents: unknown
  settings?: {
    register<T>(ns: string, schema: Schema<T>, options?: { applies?: 'live' | 'restart'; base?: Partial<T> }): { get(): T }
  }
  agentDefaultModel?: {
    currentSelection(): { provider?: string; model?: string } | undefined
  }
  workspaceRegistry?: unknown
  on?(event: string, handler: (...args: never[]) => unknown): () => void
  effect?<T extends (() => void | Promise<void>) | void>(setup: () => T, label?: string): () => void
  logger?: {
    warn(message: string): void
  }
}

export function apply(ctx: ApplyContext, config: Config = {}): void {
  const settings = registerCcteamSettings(ctx, {
    daemonUrl: config.daemonUrl,
    enrollment: config.enrollment,
    connectionStatus: config.connectionStatus,
    boundProject: config.boundProject,
  })
  const daemonUrl = () => config.daemonUrl ?? settings.get().daemonUrl ?? DEFAULT_DAEMON_URL
  const enrollment = () => {
    const enrolled = config.enrollment ?? settings.get().enrollment
    return enrolled === undefined || enrolled.trim() === '' ? undefined : enrolled
  }

  // One identity per ccteam session, never one per process: this runtime serves
  // many hires plus the human at the DSH UI.
  const credentials = new SessionCredentialStore()
  const clients = new CcteamMcpClientPool({
    daemonUrl,
    enrollment,
    credentials,
    clientName: 'ccteam-dsh-client',
    clientVersion: PACKAGE_VERSION,
  })
  const notifier = new CcteamCompletionNotifier({
    pollIntervalMs: config.completionPollIntervalMs,
    maxPolls: config.completionMaxPolls,
  })
  ctx.effect?.(() => () => {
    notifier.close()
    clients.close()
  }, 'ccteam.mcp.client')
  registerCcteamTools(
    ctx as Parameters<typeof registerCcteamTools>[0],
    exec => clients.clientFor(exec),
    notifier,
  )

  const socketPath = (config.transportSocket ?? '').trim()
  if (socketPath !== '') {
    startDshSocketTransport(ctx as Parameters<typeof startDshSocketTransport>[0], {
      version: PACKAGE_VERSION,
      socketPath,
      credentials,
    })
  }
}
