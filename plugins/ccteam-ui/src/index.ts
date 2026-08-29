/**
 * Host half of `@ccteam/ccteam-ui` — ONE package carrying all three faces of
 * the ccteam ⇄ DSH connection:
 *
 *   1. TOOL face      — the eight ccteam MCP tools as DSH tools, so a DSH agent
 *                       can hire and drive the rest of the team (tools.ts).
 *   2. TRANSPORT face — an ACP server on a unix socket, so ccteam can hire THIS
 *                       runtime's sessions (transport.ts); managed only, armed
 *                       by the `transportSocket` config key alone.
 *   3. UI face        — the workbench's backend-for-frontend under one web
 *                       route, plus the browser bundle it serves (bff.ts,
 *                       client/).
 *
 * Each face declares its OWN services through a nested `ctx.inject`, rather
 * than the union going into the module-level `inject`. That is load-bearing:
 * `webServer` ships in the web-app bundle only, so a union would dead-lock the
 * whole plugin — tools included — on every non-web profile, which is the same
 * trap `transport.ts` documents for `workspaceRegistry`. A face whose services
 * are absent simply never activates; the others still do.
 */
import Schema from '@deepseek-ai/schemastery'
import { registerBff, type BffContext } from './bff.js'
import { SessionCredentialStore } from './credentials.js'
import {
  DEFAULT_DAEMON_URL,
  UNCHECKED_STATUS,
  registerCcteamSettings,
  type CcteamSettings,
} from './settings.js'
import { CcteamCompletionNotifier, CcteamMcpClientPool, registerCcteamTools } from './tools.js'
import { startDshSocketTransport } from './transport.js'

export const name = 'ccteam-ui'

/**
 * Module-level services: only what EVERY face needs. Per-face services are
 * injected below (see the file header).
 */
export const inject = ['settings']

/** Services the tool + transport faces cannot work without. */
const HOST_SERVICES = ['agents', 'tools', 'agentDefaultModel']

/** Services the workbench's BFF cannot work without. */
const WEB_SERVICES = ['webServer']

export interface Config extends Partial<CcteamSettings> {
  completionPollIntervalMs?: number
  completionMaxPolls?: number
  transportSocket?: string
}

export const Config: Schema<Config> = Schema.object({
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
  completionPollIntervalMs: Schema.number().default(5000),
  completionMaxPolls: Schema.number().default(720),
  transportSocket: Schema.string().description('Unix socket path this plugin serves ACP on for ccteam. Empty = tool surface only.'),
})

/**
 * Reported as ACP `agentInfo.version` and as the MCP client version. ccteam's
 * Rust side gates the handshake on it (`MIN_DSH_CLIENT_VERSION`), so it must
 * stay in step with package.json — asserted by tests/host-exports.test.ts.
 */
export const PACKAGE_VERSION = '0.10.4-alpha.0'

export interface ApplyContext extends BffContext {
  /**
   * Cordis's own "run once these services exist" hook (`ctx.inject`). Each
   * face activates through it, so a profile without `webServer` still gets the
   * tools and a profile without `agents` still gets the workbench.
   */
  inject(deps: readonly string[], body: (ctx: never) => void): unknown
  settings?: {
    register<T>(
      ns: string,
      schema: Schema<T>,
      options?: { applies?: 'live' | 'restart'; base?: Partial<T> },
    ): { get(): T }
  }
}

/**
 * Resolve one field: the value pinned in the profile's patch row wins,
 * otherwise the user's settings card decides.
 *
 * An EMPTY config value is "not pinned", not "pinned to empty". That
 * distinction is the whole function: this plugin declares a `Config` schema
 * with defaults, and Cordis validates the row against it before `apply`, so
 * every key the row omits arrives as `''` (or the schema default) rather than
 * `undefined`. A plain `??` therefore never falls through, and the settings
 * card — the documented way a hand-started `dsh web` supplies its credentials
 * — would be silently dead for every field.
 *
 * @param pinned - the value from the row config, if any.
 * @param stored - the value from the settings card, if any.
 * @returns the effective value, or `undefined` when neither layer has one.
 */
function resolve(pinned: string | undefined, stored: string | undefined): string | undefined {
  for (const candidate of [pinned, stored]) {
    if (typeof candidate === 'string' && candidate.trim() !== '') return candidate
  }
  return undefined
}

/**
 * Plugin body: register the one settings namespace, then hand each face the
 * closures that resolve its configuration.
 *
 * Both layers are read through closures on every call, so editing the card
 * takes effect without a restart. Credentials never leave these closures.
 */
export function apply(ctx: ApplyContext, config: Config = {}): void {
  const settings = registerCcteamSettings(ctx, {
    daemonUrl: config.daemonUrl,
    enrollment: config.enrollment,
    restToken: config.restToken,
    defaultProject: config.defaultProject,
    connectionStatus: config.connectionStatus,
  })
  const daemonUrl = (): string =>
    resolve(config.daemonUrl, settings.get().daemonUrl) ?? DEFAULT_DAEMON_URL
  const enrollment = (): string | undefined =>
    resolve(config.enrollment, settings.get().enrollment)

  // One identity per ccteam session, never one per process: this runtime serves
  // many hires plus the human at the DSH UI.
  const credentials = new SessionCredentialStore()

  ctx.inject(HOST_SERVICES, (hostCtx: never) => {
    applyHostFaces(hostCtx as unknown as ApplyContext, config, { daemonUrl, enrollment, credentials })
  })

  ctx.inject(WEB_SERVICES, (webCtx: never) => {
    registerBff(webCtx as unknown as BffContext, {
      daemonUrl,
      restToken: () => resolve(config.restToken, settings.get().restToken) ?? '',
      defaultProject: () => resolve(config.defaultProject, settings.get().defaultProject) ?? '',
      logger: (webCtx as unknown as BffContext).logger,
    })
  })
}

/** The tool surface plus, when a socket is configured, the ACP transport. */
function applyHostFaces(
  ctx: ApplyContext,
  config: Config,
  wiring: {
    daemonUrl: () => string
    enrollment: () => string | undefined
    credentials: SessionCredentialStore
  },
): void {
  const clients = new CcteamMcpClientPool({
    daemonUrl: wiring.daemonUrl,
    enrollment: wiring.enrollment,
    credentials: wiring.credentials,
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
    ctx as unknown as Parameters<typeof registerCcteamTools>[0],
    exec => clients.clientFor(exec),
    notifier,
  )

  const socketPath = (config.transportSocket ?? '').trim()
  if (socketPath !== '') {
    startDshSocketTransport(ctx as unknown as Parameters<typeof startDshSocketTransport>[0], {
      version: PACKAGE_VERSION,
      socketPath,
      credentials: wiring.credentials,
    })
  }
}
