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

/**
 * Services the workbench's BFF cannot work without.
 *
 * The ENGINE face rides this list too, and that placement is deliberate: a
 * runtime with no web server has no card to show a state on and no button to
 * press, so installing a binary and starting a daemon behind its back would be
 * an invisible side effect of loading a plugin.
 */
const WEB_SERVICES = ['webServer']

/**
 * The profile ROW config, as ccteam's materializer writes it and Cordis
 * validates it. Deliberately NARROWER than the settings card: `autoStart` and
 * `enginePath` are card-only.
 *
 * The reason is the same trap the `resolve()` helper below documents, in its
 * boolean form. Cordis fills every schema default before `apply`, so a key
 * listed here arrives with a value whether or not the row mentioned it — which
 * is survivable for a string (empty means "not pinned") but not for a boolean:
 * `autoStart` would arrive `true` for every profile and the user's `false`
 * could never win.
 */
export interface Config extends Partial<Omit<CcteamSettings, 'autoStart' | 'enginePath'>> {
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
export const PACKAGE_VERSION = '0.10.5'

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
export const ENGINE_VERSION = '0.10.5'

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
 * Resolve one field: the value the profile's patch row PINNED wins, otherwise
 * the user's settings card decides.
 *
 * What counts as "pinned" is the whole function, and it has two halves —
 * because Cordis validates the row against this plugin's `Config` schema and
 * fills in every default BEFORE `apply` runs, so a row that mentioned nothing
 * still arrives fully populated:
 *
 *   - an EMPTY value is a blank, not a pin (shipped as a bug once: every
 *     credential field defaults to `''`, so a plain `??` never fell through and
 *     the settings card — the documented way a hand-started `dsh web` supplies
 *     its credentials — was silently dead);
 *   - a value equal to the SCHEMA DEFAULT is also a blank, for exactly the same
 *     reason. This half only bites fields whose default is not empty, which
 *     today means `daemonUrl`: its default is a real URL, so without this the
 *     card's daemon URL could never win either, and every profile would report
 *     itself as one whose engine somebody else owns.
 *
 * @param pinned - the value from the row config, if any.
 * @param stored - the value from the settings card, if any.
 * @param schemaDefault - what this field's schema fills a blank row with.
 * @returns the effective value, or `undefined` when neither layer has one.
 */
function resolve(
  pinned: string | undefined,
  stored: string | undefined,
  schemaDefault = '',
): string | undefined {
  const named = (value: string | undefined): string | undefined =>
    typeof value === 'string' && value.trim() !== '' ? value : undefined
  const row = named(pinned)
  if (row !== undefined && row.trim() !== schemaDefault) return row
  return named(stored) ?? row
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
  // The engine's lifetime is NOT this plugin's (PRD D1), so nothing below ever
  // stops a daemon. `managed` and `pinnedDaemonUrl` are the two runtimes where
  // it must not even start one — see host/engine/supervisor.ts's header.
  const environment = defaultEnvironment()
  const managed = isPinned(config.transportSocket)
  /**
   * The row describes an engine somebody else set up. A ccteam-materialized
   * profile is recognized by the CREDENTIALS in its row — ccteam writes them,
   * a human's `dsh plugin --profile <name> add` never does — rather than by the daemon URL,
   * whose schema default is present in every row and would otherwise mark
   * every profile as somebody else's.
   */
  const externallyOwned =
    !managed &&
    (isPinned(config.restToken) ||
      isPinned(config.enrollment) ||
      (isPinned(config.daemonUrl) &&
        config.daemonUrl!.trim().replace(/\/+$/, '') !== DEFAULT_DAEMON_URL))

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
    const value = (resolve(config.daemonUrl, settings.get().daemonUrl, DEFAULT_DAEMON_URL) ?? '')
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
    const supplied = resolve(config.restToken, settings.get().restToken)
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
    held: () => resolve(config.enrollment, settings.get().enrollment),
    persist: async (bearer: string) => {
      await settings.update?.({ enrollment: bearer })
    },
    logger: ctx.logger,
  })
  const enrollment = (): string | undefined =>
    resolve(config.enrollment, settings.get().enrollment) ?? enrollmentBootstrap.value()

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
      defaultProject: () => resolve(config.defaultProject, settings.get().defaultProject) ?? '',
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
