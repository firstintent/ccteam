/**
 * Host half of `@ccteam/ccteam-ui` — ONE package carrying all three faces of
 * the ccteam ⇄ DSH connection:
 *
 *   1. TOOL face      — the six ccteam MCP tools as DSH tools, so a DSH agent
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
  EngineSupervisor,
  createTokenBootstrap,
  defaultEnvironment,
  discoverDaemonUrl,
  isLoopbackUrl,
  resolveCcteamHome,
} from './host/engine/index.js'
import { createEnrollmentBootstrap } from './host/engine/bootstrap.js'
import {
  DEFAULT_DAEMON_URL,
  bindCcteamSettings,
  CCTEAM_SETTINGS_FIELDS,
} from './settings.js'
import { CcteamCompletionNotifier, CcteamMcpClientPool, registerCcteamTools } from './tools.js'
import { startDshSocketTransport } from './transport.js'

export const name = 'ccteam-ui'

/**
 * Module-level services: only what EVERY face needs. Per-face services are
 * injected below (see the file header).
 */
export const inject: string[] = []

/** Services the tool + transport faces cannot work without. */
const HOST_SERVICES = ['agents', 'tools', 'agentDefaultModel']

/**
 * Services the workbench's BFF cannot work without.
 *
 * The ENGINE face rides this list too, and that placement is deliberate: a
 * runtime with no web server has no card to show a state on and no button to
 * press, so installing a binary and starting a daemon behind its back would be
 * an invisible side effect of loading a plugin.
 */
const WEB_SERVICES = ['webServer']

/** Runtime-only fields are static; user preferences are native live Config references. */
export const Config = Schema.object({
  ...CCTEAM_SETTINGS_FIELDS,
  completionPollIntervalMs: Schema.number().default(5000),
  completionMaxPolls: Schema.number().default(720),
  transportSocket: Schema.string(),
  engineManaged: Schema.boolean().default(false),
})
export type Config = ReturnType<typeof Config>

/**
 * Reported as ACP `agentInfo.version` and as the MCP client version. ccteam's
 * Rust side gates the handshake on it (`MIN_DSH_CLIENT_VERSION`), so it must
 * stay in step with package.json — asserted by tests/host-exports.test.ts.
 */
export const PACKAGE_VERSION = '0.13.0'

/**
 * The engine version this plugin is published against — package.json
 * `ccteam.engine`, and the version of the platform packages in
 * `optionalDependencies` (all three asserted equal by tests/host-engine-bff.test.ts).
 *
 * Pre-1.0 the two move in lockstep (PRD D5): a running daemon whose version
 * differs is REPORTED, never silently replaced. Both directions are one-way
 * repairs — the engine is older, so update the engine; the plugin is older, so
 * update the plugin.
 */
export const ENGINE_VERSION = '0.13.0'

export interface ApplyContext extends BffContext {
  inject(deps: readonly string[], body: (ctx: never) => void): unknown
  get?(name: string): unknown
  fiber?: { entry?: { id: string } }
}

/**
 * Plugin body: register the one settings namespace, then hand each face the
 * closures that resolve its configuration.
 *
 * Native Config references are read through closures on every call, so edits
 * take effect without a restart. Credentials never leave these closures.
 */
export function apply(ctx: ApplyContext, config: Config = Config({})): void {
  const settings = bindCcteamSettings(ctx, config)
  const environment = defaultEnvironment()
  const managed = isPinned(config.transportSocket)
  const externallyOwned = config.engineManaged

  /**
   * The console token, bootstrapped from `$CCTEAM_HOME/secrets/web-token` when
   * — and only when — nobody supplied one and the daemon is on loopback. A
   * user-entered token names a daemon this home knows nothing about, so it
   * always wins; a non-loopback URL means the local file describes a different
   * engine and must not be read at all.
   */
  /**
   * Where the daemon is. Three layers, in this order:
   *
   *   1. what a human or a profile row NAMED — always wins, and is the only
   *      layer that can point off this machine;
   *   2. what the RUNNING daemon published in
   *      `$CCTEAM_HOME/run/daemon-endpoint.json` (pid-gated) — this is what
   *      makes a CLI user who started their daemon on another port visible to
   *      the plugin instead of getting a second one started next to it;
   *   3. the compiled default.
   *
   * Layer 2 is memoized for a second: every upstream REST call resolves this,
   * and a daemon does not move that often.
   */
  const configuredDaemonUrl = (): string | undefined => {
    const value = (settings.get().daemonUrl ?? '')
      .trim()
      .replace(/\/+$/, '')
    // The compiled default is not an instruction — it is what a blank resolves
    // to, and treating it as one would keep endpoint discovery from ever running.
    return value === '' || value === DEFAULT_DAEMON_URL ? undefined : value
  }
  const discovered = memoize(() => discoverDaemonUrl(resolveCcteamHome(environment)), 1_000)
  const daemonUrl = (): string => configuredDaemonUrl() ?? discovered() ?? DEFAULT_DAEMON_URL

  const tokens = createTokenBootstrap({
    home: () => resolveCcteamHome(environment),
    logger: ctx.logger,
  })
  const restToken = (): string => {
    const supplied = settings.get().restToken.trim() || undefined
    if (supplied !== undefined) return supplied
    return isLoopbackUrl(daemonUrl()) ? tokens.read() ?? '' : ''
  }

  /**
   * The tool face's credential is ASKED OF THE DAEMON, never scavenged from a
   * file: `POST /api/v1/enroll` with `ensure` is idempotent per (identity,
   * label), so this installation ends up with exactly one credential however
   * many times DSH restarts. The bearer is still stored in this plugin's own
   * settings — that is what lets the next boot skip the call entirely, and
   * what makes the credential visible and revocable from both ends.
   */
  const enrollmentBootstrap = createEnrollmentBootstrap({
    daemonUrl,
    authorization: () => authorizationFor(restToken()),
    held: () => settings.get().enrollment.trim() || undefined,
    persist: async (bearer: string) => {
      await settings.update?.({ enrollment: bearer })
    },
    logger: ctx.logger,
  })
  const enrollment = (): string | undefined =>
    settings.get().enrollment.trim() || enrollmentBootstrap.value()

  // One identity per ccteam session, never one per process: this runtime serves
  // many hires plus the human at the DSH UI.
  const credentials = new SessionCredentialStore()

  ctx.inject(HOST_SERVICES, (hostCtx: never) => {
    applyHostFaces(hostCtx as unknown as ApplyContext, config, { daemonUrl, enrollment, credentials })
    // Ask only a LOCAL daemon, and only when nobody already supplied one.
    if (enrollment() === undefined && isLoopbackUrl(daemonUrl())) {
      void enrollmentBootstrap.ensure()
    }
  })

  ctx.inject(WEB_SERVICES, (webCtx: never) => {
    const webContext = webCtx as unknown as BffContext
    const supervisor = new EngineSupervisor({
      daemonUrl,
      configuredDaemonUrl,
      autoStart: () => settings.get().autoStart !== false,
      enginePath: () => settings.get().enginePath,
      pinnedVersion: ENGINE_VERSION,
      managed,
      externallyOwned,
      environment,
      logger: webContext.logger,
    })
    // Releases probes and log handles ONLY. Never the daemon: a DSH restart is
    // not a reason to drop somebody else's IM gateway (D1).
    webContext.effect?.(() => () => supervisor.dispose(), 'ccteam-ui.engine')

    registerBff(webContext, {
      daemonUrl,
      restToken,
      defaultProject: () => settings.get().defaultProject ?? '',
      engine: supervisor,
      logger: webContext.logger,
    })

    // Install + start on load, when the user left auto-start on. Fire and
    // forget: a plugin that cannot reach an engine must still load its panel.
    void supervisor.ensure().catch((error: unknown) => {
      webContext.logger?.warn(`ccteam-ui: engine bootstrap failed: ${describeError(error)}`)
    })
  })
}

/** Cache a cheap-but-not-free lookup for `ttlMs`, without a timer. */
function memoize<T>(read: () => T, ttlMs: number): () => T {
  let at = 0
  let cached: T
  let primed = false
  return (): T => {
    const now = Date.now()
    if (!primed || now - at >= ttlMs) {
      cached = read()
      at = now
      primed = true
    }
    return cached
  }
}

/** A row value the profile actually pinned (the schema default is `''`). */
function isPinned(value: string | undefined): boolean {
  return typeof value === 'string' && value.trim() !== ''
}

/**
 * ccteam presents web tokens as `Authorization: Bearer ccteam:<hex>` and
 * rejects a bare hex there (crates/ccteam-web/src/auth.rs). Same normalization
 * as the BFF's, applied to the one call this file makes itself.
 */
function authorizationFor(token: string): string | undefined {
  const trimmed = token.trim()
  if (trimmed === '') return undefined
  return `Bearer ${trimmed.includes(':') ? trimmed : `ccteam:${trimmed}`}`
}

function describeError(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
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
