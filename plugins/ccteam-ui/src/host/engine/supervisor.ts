/**
 * The engine's state machine: probe, attach, install, start — and the four
 * explicit actions the settings card offers.
 *
 * ONE invariant governs every branch (PRD v0.10.5 §5, decision D1): the daemon
 * OUTLIVES this plugin. It is a `setsid` supervisor with its own lifetime,
 * shared with the CLI, ccteam web, IM, and any systemd unit under the same
 * `$CCTEAM_HOME`. So:
 *
 *   - whoever starts it first wins, and everyone else ATTACHES;
 *   - `dispose()` releases probes and nothing else — a DSH restart or a
 *     `dsh plugin --profile <name> update` must never drop the Telegram gateway or a running
 *     A2A delegation;
 *   - a daemon reporting a DIFFERENT home is never "fixed" by starting a
 *     second one; it is reported (`Mismatch{home}`) and left alone;
 *   - a daemon whose version differs from the one this plugin was published
 *     against is reported (`Mismatch{version}`) and its binary is NOT
 *     swapped under it — the repair is the explicit "update engine" action,
 *     which is `ccteam update --channel npm --binary <pkg>` and carries the
 *     engine's own drain + graceful restart + version verify contract.
 *
 * The supervisor is INERT in two runtimes, and the difference matters:
 *   - MANAGED (`transportSocket` pinned in the profile row): ccteam started
 *     this DSH runtime. The daemon is the parent process; supervising it from
 *     inside its own child is the loop this plugin exists to avoid.
 *   - PINNED (`daemonUrl` pinned in the row, or a non-loopback URL): somebody
 *     else — a tenant profile ccteam materialized, or a human naming a LAN
 *     daemon — owns that engine. Probing to Attached is the whole job.
 */
import {
  binaryVersion,
  canonicalPath,
  daemonLogPath,
  defaultEnvironment,
  discoverDaemonUrl,
  enginePlatform,
  locateEngine,
  runCommand,
  tailFile,
  type EngineEnvironment,
  type EngineLocation,
  type EnginePlatform,
  type RunFn,
} from './locate.js'
import {
  installEngine,
  resolveInstallDir,
  resolvePackageBin as defaultResolvePackageBin,
  type ResolvePackageBin,
} from './install.js'
import { DEFAULT_DAEMON_URL } from '../../settings.js'
import type {
  EngineActionResult,
  EngineLogResponse,
  EngineState,
  EngineStatus,
  EngineUnsupervisedReason,
} from '../../shared/contract.js'
import { join } from 'node:path'
import { statSync } from 'node:fs'

export type FetchLike = (input: string, init?: RequestInit) => Promise<Response>

/** `GET /health`, as PLUG-1 shaped it (crates/ccteam-web/src/routes/health.rs). */
export interface HealthBody {
  status?: string
  version?: string
  build?: string | null
  home?: string
  pid?: number
  web_bind?: string | null
  dsh_web_bind?: string | null
  uptime_secs?: number
}

export interface SupervisorOptions {
  /**
   * The EFFECTIVE daemon URL — configured, else discovered from the running
   * daemon's endpoint pointer, else the default. Read live: a settings edit
   * must take effect without a restart.
   */
  daemonUrl: () => string
  /**
   * The url a human or a profile row actually NAMED, or `undefined` when
   * nobody did. Separate from `daemonUrl` because the two answer different
   * questions: where to look, versus where to tell a new daemon to bind.
   */
  configuredDaemonUrl?: () => string | undefined
  autoStart: () => boolean
  /** `enginePath` (advanced) — an explicit binary the user named. */
  enginePath?: () => string | undefined
  /** The engine version this plugin ships against (package.json `ccteam.engine`). */
  pinnedVersion: string
  /** True when ccteam itself started this DSH runtime (row pins `transportSocket`). */
  managed?: boolean
  /** True when the profile row describes an engine somebody else set up. */
  externallyOwned?: boolean
  environment?: EngineEnvironment
  run?: RunFn
  fetchImpl?: FetchLike
  sleep?: (ms: number) => Promise<void>
  resolvePackageBin?: ResolvePackageBin
  logger?: { warn(message: string): void }
  probeTimeoutMs?: number
  readyTimeoutMs?: number
  readyPollMs?: number
}

const DEFAULT_PROBE_TIMEOUT_MS = 4_000
const DEFAULT_READY_TIMEOUT_MS = 30_000
const DEFAULT_READY_POLL_MS = 250
const DEFAULT_LOG_LINES = 200
const MAX_LOG_LINES = 2_000

/** Only a daemon we could actually reach with a signal is ours to supervise. */
export function isLoopbackUrl(url: string): boolean {
  let parsed: URL
  try {
    parsed = new URL(url)
  } catch {
    return false
  }
  const host = parsed.hostname.replace(/^\[|\]$/g, '')
  return host === 'localhost' || host === '::1' || host === '0.0.0.0' || /^127\./.test(host)
}

export class EngineSupervisor {
  private readonly options: SupervisorOptions
  private readonly environment: EngineEnvironment
  private readonly run: RunFn
  private readonly doFetch: FetchLike
  private readonly sleep: (ms: number) => Promise<void>
  private readonly packageBin: ResolvePackageBin
  /** In-flight probes, aborted by `dispose()`. The ONLY thing dispose touches. */
  private readonly probes = new Set<AbortController>()
  /** Serializes the mutating actions; never held across a re-entrant call. */
  private queue: Promise<unknown> = Promise.resolve()
  private ensuring: Promise<EngineStatus> | undefined
  private phase: 'idle' | 'installing' | 'starting' = 'idle'
  /** `running` vs `attached`: did THIS plugin start the daemon it sees? */
  private startedHere = false
  private disposed = false
  /** `--version` memo keyed by the binary's identity, not just its path. */
  private readonly versions = new Map<string, { key: string; version?: string }>()

  constructor(options: SupervisorOptions) {
    this.options = options
    this.environment = options.environment ?? defaultEnvironment()
    this.run = options.run ?? runCommand
    this.doFetch = options.fetchImpl ?? ((input, init) => fetch(input, init))
    this.sleep = options.sleep ?? (ms => new Promise(resolve => setTimeout(resolve, ms)))
    this.packageBin = options.resolvePackageBin ?? defaultResolvePackageBin
  }

  /**
   * Release probes and log handles. NEVER stops the daemon: a DSH restart is
   * not a reason to end somebody else's Telegram gateway (D1).
   */
  dispose(): void {
    this.disposed = true
    for (const controller of this.probes) controller.abort()
    this.probes.clear()
  }

  /** Read-only: what is true right now. No install, no start, no side effects. */
  async status(): Promise<EngineStatus> {
    return await this.describe()
  }

  /**
   * The `apply()` path: probe → attach, otherwise install and start when the
   * user left auto-start on. Concurrent callers share ONE run.
   */
  async ensure(): Promise<EngineStatus> {
    if (this.ensuring !== undefined) return await this.ensuring
    const run = this.runEnsure().finally(() => {
      this.ensuring = undefined
    })
    this.ensuring = run
    return await run
  }

  private async runEnsure(): Promise<EngineStatus> {
    let status = await this.describe()
    if (this.disposed) return status
    if (!status.supervised) return status
    if (status.reachable) return status
    if (!this.options.autoStart()) return status
    if (status.binary === undefined || this.needsInstall(status)) {
      const installed = await this.serialize(() => this.installFromPackage(status))
      if (!installed.ok) return { ...installed.status, detail: installed.error ?? installed.status.detail }
      status = installed.status
    }
    const started = await this.serialize(() => this.startEngine())
    return started.status
  }

  /**
   * A binary that is not the version this plugin ships against — including one
   * that would not say which version it is, because an engine that cannot
   * answer `--version` is not one to hand a daemon to.
   */
  private needsInstall(status: EngineStatus): boolean {
    return status.binaryVersion !== this.options.pinnedVersion
  }

  async start(): Promise<EngineActionResult> {
    return await this.serialize(async () => {
      const status = await this.describe()
      if (!status.supervised) return this.refuse(status)
      if (status.reachable && status.mismatch !== 'home') {
        return { ok: true, status }
      }
      if (status.mismatch === 'home') return this.refuse(status)
      if (status.binary === undefined) {
        const installed = await this.installFromPackage(status)
        if (!installed.ok) return installed
      }
      return await this.startEngine()
    })
  }

  /**
   * Explicit user command (never a daemon-initiated kill). The card says what
   * this costs: ccteam web and the IM gateway stop with it.
   */
  async stop(): Promise<EngineActionResult> {
    return await this.serialize(async () => {
      const before = await this.describe()
      if (!before.supervised) return this.refuse(before)
      if (before.binary === undefined) {
        return {
          ok: false,
          status: before,
          errorKind: 'engineMissing',
          error: 'no ccteam binary to run `ccteam daemon stop` with',
        }
      }
      const verdict = await this.runCli(before.binary, ['daemon', 'stop', '--json'])
      this.startedHere = false
      const state = typeof verdict.body?.status === 'string' ? verdict.body.status : undefined
      const status = await this.describe()
      if (state === 'stopped' || state === 'notRunning') return { ok: true, status }
      return {
        ok: false,
        status,
        errorKind: typeof verdict.body?.code === 'string' ? verdict.body.code : 'stopFailed',
        error: verdict.message ?? 'ccteam daemon stop failed',
      }
    })
  }

  /**
   * Stop, then start — as two serialized actions rather than one, because a
   * lock held across a re-entrant call is how `start_for` deadlocked itself in
   * v0.10.0 dogfood. The window between them is harmless: the worst another
   * caller can do inside it is start the daemon we were about to start.
   */
  async restart(): Promise<EngineActionResult> {
    const stopped = await this.stop()
    // A daemon that refused to stop must not be raced by a second start.
    if (!stopped.ok && stopped.status.reachable) return stopped
    return await this.start()
  }

  /**
   * Swap the binary through the ENGINE's own updater, not by copying over it:
   * `ccteam update --channel npm --binary <pkg bin>` drains in-flight turns,
   * restarts gracefully, and verifies the new version. Copying under a running
   * daemon would do none of that.
   */
  async update(): Promise<EngineActionResult> {
    return await this.serialize(async () => {
      const before = await this.describe()
      if (!before.supervised) return this.refuse(before)
      const platform = enginePlatform(this.environment)
      const source = platform === undefined ? undefined : this.packageBin(platform)
      if (source === undefined) {
        return {
          ok: false,
          status: before,
          errorKind: 'packageMissing',
          error:
            'the engine package that ships with this plugin is not installed; reinstall the ' +
            'plugin (`dsh plugin --profile <name> add @ccteam/ccteam-ui`, profile = the one you started `dsh web` with) so its platform package comes with it',
        }
      }
      if (before.binary === undefined) {
        // Nothing to update through: this is a first install.
        const installed = await this.installFromPackage(before)
        return installed
      }
      const verdict = await this.runCli(before.binary, [
        'update',
        '--channel',
        'npm',
        '--binary',
        source,
        '--json',
      ])
      this.versions.clear()
      const status = await this.describe()
      if (verdict.code === 0 && verdict.body?.status !== 'error') return { ok: true, status }
      return {
        ok: false,
        status,
        errorKind: typeof verdict.body?.code === 'string' ? verdict.body.code : 'updateFailed',
        error: verdict.message ?? 'ccteam update failed',
      }
    })
  }

  /** Tail of the daemon's own log file — the same file `ccteam daemon logs` reads. */
  async log(lines = DEFAULT_LOG_LINES): Promise<EngineLogResponse> {
    const home = this.home()
    const path = daemonLogPath(home)
    const want = Math.max(1, Math.min(MAX_LOG_LINES, Math.trunc(lines) || DEFAULT_LOG_LINES))
    try {
      return { ok: true, path, lines: tailFile(path, want) }
    } catch (error) {
      return {
        ok: false,
        path,
        lines: [],
        error:
          isNotFound(error)
            ? `no daemon log yet at ${path} (it appears on the first start)`
            : describe(error),
      }
    }
  }

  // ------------------------------------------------------------------ internals

  private home(): string {
    const pinned = (this.environment.env.CCTEAM_HOME ?? '').trim()
    return canonicalPath(pinned !== '' ? pinned : join(this.environment.homedir(), '.ccteam'))
  }

  private async serialize<T>(body: () => Promise<T>): Promise<T> {
    const next = this.queue.then(body, body)
    this.queue = next.then(
      () => undefined,
      () => undefined,
    )
    return await next
  }

  private refuse(status: EngineStatus): EngineActionResult {
    return {
      ok: false,
      status,
      errorKind: status.unsupervisedReason ?? 'notSupervised',
      error: status.detail,
    }
  }

  private async locate(): Promise<EngineLocation> {
    return await locateEngine({
      environment: this.environment,
      configuredPath: this.options.enginePath?.(),
      run: (bin, args, opts) => {
        if (args.length === 1 && args[0] === '--version') return this.cachedVersionRun(bin)
        return this.run(bin, args, opts)
      },
    })
  }

  /** `--version` memo keyed by (size, mtime): a `rename` install invalidates it. */
  private async cachedVersionRun(bin: string): Promise<{ code: number; stdout: string; stderr: string }> {
    let key: string
    try {
      const stat = statSync(bin)
      key = `${stat.size}:${stat.mtimeMs}`
    } catch {
      key = 'unknown'
    }
    const hit = this.versions.get(bin)
    if (hit !== undefined && hit.key === key) {
      return hit.version === undefined
        ? { code: 1, stdout: '', stderr: '' }
        : { code: 0, stdout: `ccteam ${hit.version}\n`, stderr: '' }
    }
    const version = await binaryVersion(bin, this.run)
    this.versions.set(bin, { key, version })
    return version === undefined
      ? { code: 1, stdout: '', stderr: '' }
      : { code: 0, stdout: `ccteam ${version}\n`, stderr: '' }
  }

  private async probe(): Promise<HealthBody | undefined> {
    const base = this.options.daemonUrl().trim().replace(/\/+$/, '')
    if (base === '') return undefined
    const controller = new AbortController()
    this.probes.add(controller)
    const timer = setTimeout(() => controller.abort(), this.options.probeTimeoutMs ?? DEFAULT_PROBE_TIMEOUT_MS)
    try {
      const response = await this.doFetch(`${base}/health`, {
        headers: { accept: 'application/json' },
        signal: controller.signal,
      })
      if (!response.ok) return undefined
      const body = (await response.json()) as HealthBody
      return body !== null && typeof body === 'object' && body.status === 'ok' ? body : undefined
    } catch {
      return undefined
    } finally {
      clearTimeout(timer)
      this.probes.delete(controller)
    }
  }

  /** How the address in `daemonUrl` was arrived at — reported, not inferred. */
  private daemonUrlSource(): 'configured' | 'endpoint' | 'default' {
    const configured = (this.options.configuredDaemonUrl?.() ?? '').trim()
    if (configured !== '') return 'configured'
    return discoverDaemonUrl(this.home()) === undefined ? 'default' : 'endpoint'
  }

  private async describe(): Promise<EngineStatus> {
    const daemonUrl = this.options.daemonUrl().trim()
    const location = await this.locate()
    const health = location.supported || this.options.managed === true ? await this.probe() : undefined
    const home = location.home
    const daemonHome = typeof health?.home === 'string' ? health.home : undefined
    const runningVersion = typeof health?.version === 'string' ? health.version : undefined

    const base: EngineStatus = {
      state: 'stopped',
      reachable: health !== undefined,
      supervised: true,
      daemonUrl,
      daemonUrlSource: this.daemonUrlSource(),
      pinnedVersion: this.options.pinnedVersion,
      home,
      autoStart: this.options.autoStart(),
      logPath: daemonLogPath(home),
      detail: '',
      ...defined('platform', location.platform as EnginePlatform | undefined),
      ...defined('binary', location.binary),
      ...defined('binarySource', location.source),
      ...defined('binaryVersion', location.version),
      ...defined('daemonHome', daemonHome),
      ...defined('runningVersion', runningVersion),
      ...defined('pid', typeof health?.pid === 'number' ? health.pid : undefined),
      ...defined('webBind', typeof health?.web_bind === 'string' ? health.web_bind : undefined),
      ...defined('dshWebBind', typeof health?.dsh_web_bind === 'string' ? health.dsh_web_bind : undefined),
      ...defined('uptimeSecs', typeof health?.uptime_secs === 'number' ? health.uptime_secs : undefined),
    }

    const unsupervised = this.unsupervisedReason(location, daemonUrl)
    if (unsupervised !== undefined) {
      return {
        ...base,
        supervised: false,
        unsupervisedReason: unsupervised,
        ...this.unsupervisedFacts(unsupervised, base, location),
      }
    }

    // Reachable: decide attach vs mismatch BEFORE anything is installed or run.
    if (health !== undefined) {
      if (daemonHome !== undefined && daemonHome !== home) {
        return {
          ...base,
          state: 'mismatch',
          mismatch: 'home',
          detail:
            `the daemon at ${daemonUrl} runs in ${daemonHome}, not ${home}. ccteam will not ` +
            'start a second daemon; point CCTEAM_HOME at the same home (or the panel at that ' +
            "daemon's URL) so both halves share one engine.",
        }
      }
      if (runningVersion !== undefined && runningVersion !== this.options.pinnedVersion) {
        return {
          ...base,
          state: 'mismatch',
          mismatch: 'version',
          detail:
            `the running engine is ${runningVersion}; this plugin ships against ` +
            `${this.options.pinnedVersion}. The binary is left untouched — use “update engine” ` +
            '(it drains in-flight turns, restarts, and verifies the new version), or update the ' +
            'plugin if the engine is the newer one.',
        }
      }
      const state: EngineState = this.startedHere ? 'running' : 'attached'
      return {
        ...base,
        state,
        detail:
          state === 'running'
            ? `ccteam ${runningVersion ?? this.options.pinnedVersion} is running (pid ${health.pid ?? '?'}) in ${home}.`
            : `attached to the ccteam daemon already running in ${home} (pid ${health.pid ?? '?'}).`,
      }
    }

    if (this.phase === 'installing') {
      return { ...base, state: 'installing', detail: 'installing the ccteam engine from the plugin’s platform package…' }
    }
    if (this.phase === 'starting') {
      return { ...base, state: 'starting', detail: 'starting the ccteam daemon…' }
    }
    if (location.binary === undefined) {
      return {
        ...base,
        state: 'missing',
        detail: location.unreadable === true
          ? 'a ccteam binary was found but did not answer `--version`.'
          : 'no ccteam engine is installed yet; it is installed from the plugin’s platform package.',
      }
    }
    return {
      ...base,
      state: 'stopped',
      detail: `ccteam ${location.version ?? '(unknown version)'} is installed at ${location.binary}; the daemon is not running.`,
    }
  }

  private unsupervisedReason(location: EngineLocation, daemonUrl: string): EngineUnsupervisedReason | undefined {
    if (this.options.managed === true) return 'managed'
    if (this.options.externallyOwned === true) return 'pinned'
    if (!location.supported) return 'unsupported'
    if (daemonUrl !== '' && !isLoopbackUrl(daemonUrl)) return 'remote'
    return undefined
  }

  private unsupervisedFacts(
    reason: EngineUnsupervisedReason,
    base: EngineStatus,
    location: EngineLocation,
  ): Pick<EngineStatus, 'state' | 'detail'> {
    if (reason === 'unsupported') {
      return {
        state: 'unsupported',
        detail:
          `ccteam publishes engines for linux and macOS on x64 and arm64; this machine is ` +
          `${this.environment.platform}-${this.environment.arch}. Nothing was installed.`,
      }
    }
    if (base.reachable) {
      if (reason === 'managed') {
        return {
          state: 'attached',
          detail:
            'attached to the ccteam daemon that started this DSH runtime; its lifecycle is not ' +
            'this plugin’s to manage.',
        }
      }
      return {
        state: 'attached',
        detail:
          reason === 'remote'
            ? `attached to the ccteam daemon at ${base.daemonUrl}. It is not on this machine, so there is nothing here to start or stop.`
            : `attached to the ccteam daemon at ${base.daemonUrl}, which this profile names but does not manage.`,
      }
    }
    const missing = location.binary === undefined
    return {
      state: missing ? 'missing' : 'stopped',
      detail:
        reason === 'managed'
          ? 'the ccteam daemon that started this DSH runtime is not answering; it owns its own lifecycle.'
          : `the ccteam daemon at ${base.daemonUrl} is not answering. This profile points at an engine it does not manage, so nothing was started here.`,
    }
  }

  private async installFromPackage(status: EngineStatus): Promise<EngineActionResult> {
    const platform = enginePlatform(this.environment)
    const source = platform === undefined ? undefined : this.packageBin(platform)
    if (source === undefined) {
      return {
        ok: false,
        status,
        errorKind: 'packageMissing',
        error:
          'the engine package that ships with this plugin is not installed; reinstall the plugin ' +
          '(`dsh plugin --profile <name> add @ccteam/ccteam-ui`, profile = the one you started `dsh web` with) so its platform package comes with it',
      }
    }
    // A RUNNING daemon is never swapped from under itself — that is what
    // `ccteam update` is for (drain + graceful restart + verify).
    if (status.reachable) {
      return {
        ok: false,
        status,
        errorKind: 'daemonRunning',
        error: 'the daemon is running; use “update engine” so it drains and restarts cleanly',
      }
    }
    // The ladder does its OWN PATH lookup (see resolveInstallDir): "where does
    // ccteam live" is not the same question as "which binary did discovery
    // pick", and answering the wrong one installs beside a shadowing copy.
    const dest = join(resolveInstallDir(this.environment), 'ccteam')
    this.phase = 'installing'
    try {
      const outcome = await installEngine({ source, dest, run: this.run })
      this.versions.clear()
      if (!outcome.ok) {
        this.phase = 'idle'
        const next = await this.describe()
        return { ok: false, status: next, errorKind: outcome.errorKind, error: outcome.error }
      }
    } finally {
      this.phase = 'idle'
    }
    return { ok: true, status: await this.describe() }
  }

  private async startEngine(): Promise<EngineActionResult> {
    const located = await this.locate()
    if (located.binary === undefined) {
      const status = await this.describe()
      return { ok: false, status, errorKind: 'engineMissing', error: 'no ccteam binary to start' }
    }
    this.phase = 'starting'
    let verdict: CliVerdict
    try {
      // `ccteam start` IS `ccteam daemon start` (D7): a setsid launcher that
      // detaches, polls for readiness, and records the pid. Both verdicts —
      // "I started it" and "it was already up" — are success.
      verdict = await this.runCli(located.binary, this.startArgs())
    } finally {
      this.phase = 'idle'
    }
    const state = typeof verdict.body?.status === 'string' ? verdict.body.status : undefined
    if (state !== 'started' && state !== 'alreadyRunning') {
      const status = await this.describe()
      return {
        ok: false,
        status,
        errorKind: typeof verdict.body?.code === 'string' ? verdict.body.code : 'startFailed',
        error: verdict.message ?? 'ccteam start failed',
      }
    }
    this.startedHere = state === 'started'
    const ready = await this.waitForReady()
    const status = await this.describe()
    if (!ready) {
      return {
        ok: false,
        status,
        errorKind: 'notReady',
        error: `the daemon reported ${state} but ${status.daemonUrl}/health did not answer in time`,
      }
    }
    return { ok: true, status }
  }

  /**
   * argv for the start. Bare `start --json` in the normal case — and that is
   * deliberate, because `--web-bind` has ONE compiled default (`0.0.0.0:7331`)
   * and no config-file key, so passing it always would narrow a CLI user's
   * all-interfaces console down to loopback behind their back.
   *
   * When the user has pointed this plugin at a DIFFERENT address, though, that
   * address is an instruction: without forwarding it, the plugin would start a
   * daemon on 7331, then probe the port it was told about, find nothing, and
   * start nothing further — or find somebody ELSE's daemon on 7331 and report
   * a home mismatch it caused itself.
   */
  private startArgs(): readonly string[] {
    // The CONFIGURED url, never the discovered one: a discovered address comes
    // from a daemon that is already running, so it can never be what a start
    // should bind to.
    const url = (this.options.configuredDaemonUrl?.() ?? '').trim().replace(/\/+$/, '')
    if (url === '' || url === DEFAULT_DAEMON_URL.replace(/\/+$/, '')) return ['start', '--json']
    let parsed: URL
    try {
      parsed = new URL(url)
    } catch {
      return ['start', '--json']
    }
    const port = parsed.port !== '' ? parsed.port : parsed.protocol === 'https:' ? '443' : '80'
    return ['start', '--web-bind', `${parsed.hostname}:${port}`, '--json']
  }

  private async waitForReady(): Promise<boolean> {
    const deadline = this.options.readyTimeoutMs ?? DEFAULT_READY_TIMEOUT_MS
    const step = this.options.readyPollMs ?? DEFAULT_READY_POLL_MS
    for (let waited = 0; waited <= deadline; waited += step) {
      if (this.disposed) return false
      if ((await this.probe()) !== undefined) return true
      await this.sleep(step)
    }
    return false
  }

  private async runCli(binary: string, args: readonly string[]): Promise<CliVerdict> {
    const result = await this.run(binary, args, { env: this.environment.env })
    const body = lastJsonObject(result.stdout)
    const message = typeof body?.message === 'string'
      ? body.message
      : result.stderr.trim() !== ''
        ? result.stderr.trim()
        : undefined
    return { code: result.code, body, ...(message === undefined ? {} : { message }) }
  }
}

interface CliVerdict {
  code: number
  body: Record<string, unknown> | undefined
  message?: string
}

/**
 * The CLI prints its machine verdict as ONE JSON object on stdout and its
 * human sentence on stderr; take the last line that parses so a stray banner
 * cannot shadow the verdict.
 */
export function lastJsonObject(text: string): Record<string, unknown> | undefined {
  const lines = text.split('\n')
  for (let i = lines.length - 1; i >= 0; i -= 1) {
    const line = lines[i]?.trim()
    if (line === undefined || line === '' || !line.startsWith('{')) continue
    try {
      const parsed = JSON.parse(line) as unknown
      if (parsed !== null && typeof parsed === 'object' && !Array.isArray(parsed)) {
        return parsed as Record<string, unknown>
      }
    } catch {
      // not this line
    }
  }
  return undefined
}

function defined<K extends string, V>(key: K, value: V | undefined): Record<K, V> | Record<string, never> {
  return value === undefined ? {} : ({ [key]: value } as Record<K, V>)
}

function isNotFound(error: unknown): boolean {
  return (error as { code?: unknown } | null)?.code === 'ENOENT'
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}
