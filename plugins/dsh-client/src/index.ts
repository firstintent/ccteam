import Schema from '@deepseek-ai/schemastery'
import { CcteamCompletionNotifier, CcteamMcpClient, registerCcteamTools } from './tools.js'
import { DEFAULT_DAEMON_URL, registerCcteamSettings, type CcteamSettings } from './settings.js'
import { shouldStartTransport, startDshTransport } from './transport.js'

export const name = 'ccteam-client'
export const inject = ['agents', 'tools', 'settings', 'agentDefaultModel']

export interface Config extends Partial<CcteamSettings> {
  completionPollIntervalMs?: number
  completionMaxPolls?: number
}

export const Config: Schema<Config> = Schema.object({
  daemonUrl: Schema.string().default(DEFAULT_DAEMON_URL).description('ccteam daemon URL.'),
  enrollment: Schema.string().default('').role('secret').description('Enrollment credential from ccteam config.'),
  connectionStatus: Schema.string().default('Not checked. If the daemon is unreachable, run: ccteam start'),
  boundProject: Schema.string().default(''),
  completionPollIntervalMs: Schema.number().default(5000),
  completionMaxPolls: Schema.number().default(720),
})

const PACKAGE_VERSION = '0.9.15-alpha.0'

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
  on?(event: string, handler: (...args: never[]) => unknown): () => void
  effect?<T extends (() => void | Promise<void>) | void>(setup: () => T, label?: string): () => void
  logger?: {
    warn(message: string): void
  }
}

export function apply(ctx: ApplyContext, config: Config = {}): void {
  const bootBearer = process.env.CCTEAM_MCP_BEARER
  delete process.env.CCTEAM_MCP_BEARER

  const settings = registerCcteamSettings(ctx, {
    daemonUrl: config.daemonUrl,
    enrollment: config.enrollment,
    connectionStatus: config.connectionStatus,
    boundProject: config.boundProject,
  })
  const daemonUrl = () => config.daemonUrl ?? settings.get().daemonUrl ?? DEFAULT_DAEMON_URL
  const credential = () => {
    if (bootBearer !== undefined && bootBearer.trim() !== '') return bootBearer
    const enrolled = config.enrollment ?? settings.get().enrollment
    return enrolled === undefined || enrolled.trim() === '' ? undefined : enrolled
  }

  const client = new CcteamMcpClient({
    daemonUrl: daemonUrl(),
    credential,
    clientName: 'ccteam-dsh-client',
    clientVersion: PACKAGE_VERSION,
  })
  const notifier = new CcteamCompletionNotifier(client, {
    pollIntervalMs: config.completionPollIntervalMs,
    maxPolls: config.completionMaxPolls,
  })
  ctx.effect?.(() => () => {
    notifier.close()
    client.close()
  }, 'ccteam.mcp.client')
  registerCcteamTools(
    ctx as Parameters<typeof registerCcteamTools>[0],
    client,
    notifier,
  )

  if (shouldStartTransport(process.env, bootBearer)) {
    startDshTransport(ctx as Parameters<typeof startDshTransport>[0], {
      version: PACKAGE_VERSION,
      approvalMode: process.env.CCTEAM_DSH_APPROVAL === 'hitl' ? 'hitl' : 'skip',
    })
  }
}
