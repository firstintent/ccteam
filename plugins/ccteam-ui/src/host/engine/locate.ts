/**
 * Where the ccteam engine is, and whether this machine can run one.
 *
 * Every rule here MIRRORS a rule the Rust engine already owns, because two
 * resolvers that disagree produce the failure ccteam's installer documents:
 * two binaries, two homes, and whichever sorts first on PATH wins.
 *
 *   - `$CCTEAM_HOME` > `~/.ccteam`         → `CcteamPaths::from_env`
 *     (canonicalized, because `GET /health` reports a canonical path and the
 *     plugin compares the two byte-for-byte to decide "is this MY daemon?")
 *   - `${CCTEAM_INSTALL_DIR:-$HOME/.local/bin}/ccteam`
 *                                          → `install.sh::resolve_install_dir`
 *                                            / `update.rs::resolve_install_dir`
 *   - version = first dotted-numeric token of `ccteam --version`
 *                                          → `update.rs::parse_version_output`
 *
 * Nothing in this file mutates anything: locating is a read.
 */
import { execFile } from 'node:child_process'
import { openSync, readSync, readFileSync, closeSync, fstatSync, statSync, accessSync, constants, realpathSync } from 'node:fs'
import { homedir } from 'node:os'
import { delimiter, dirname, join } from 'node:path'
// Type-only: the seam owns this union, so the card and the resolver cannot
// drift apart. Erased at build time, so it adds no runtime edge.
import type { EngineBinarySource } from '../../shared/contract.js'

/** The `<os>-<cpu>` tuples ccteam publishes a platform package for. */
export type EnginePlatform = 'linux-x64' | 'linux-arm64' | 'darwin-x64' | 'darwin-arm64'

export const ENGINE_PLATFORMS: readonly EnginePlatform[] = [
  'linux-x64',
  'linux-arm64',
  'darwin-x64',
  'darwin-arm64',
]

/** Ambient facts the resolvers read. Injectable so tests never guess the host. */
export interface EngineEnvironment {
  env: NodeJS.ProcessEnv
  platform: NodeJS.Platform
  arch: string
  homedir: () => string
}

export function defaultEnvironment(): EngineEnvironment {
  return { env: process.env, platform: process.platform, arch: process.arch, homedir }
}

export interface RunResult {
  /** Exit code, or -1 when the process could not be spawned at all. */
  code: number
  stdout: string
  stderr: string
}

export type RunFn = (
  bin: string,
  args: readonly string[],
  options?: { timeoutMs?: number; env?: NodeJS.ProcessEnv },
) => Promise<RunResult>

/** Cap on a CLI's output we are willing to buffer (the JSON verdicts are tiny). */
const RUN_MAX_BUFFER = 4 * 1024 * 1024
const RUN_DEFAULT_TIMEOUT_MS = 60_000

/**
 * Run a command and RESOLVE with its outcome — a non-zero exit is data here,
 * not an exception: `ccteam daemon stop` exits 1 with a JSON verdict on stdout,
 * and a thrown error would throw that verdict away.
 */
export const runCommand: RunFn = (bin, args, options = {}) =>
  new Promise<RunResult>(resolve => {
    execFile(
      bin,
      [...args],
      {
        timeout: options.timeoutMs ?? RUN_DEFAULT_TIMEOUT_MS,
        maxBuffer: RUN_MAX_BUFFER,
        ...(options.env === undefined ? {} : { env: options.env }),
      },
      (error, stdout, stderr) => {
        const code =
          error === null
            ? 0
            : typeof (error as { code?: unknown }).code === 'number'
              ? ((error as { code: number }).code)
              : -1
        resolve({ code, stdout: String(stdout ?? ''), stderr: String(stderr ?? '') })
      },
    )
  })

/** `undefined` on any platform ccteam does not publish an engine for. */
export function enginePlatform(environment: EngineEnvironment): EnginePlatform | undefined {
  const os = environment.platform === 'linux' || environment.platform === 'darwin'
    ? environment.platform
    : undefined
  const cpu = environment.arch === 'x64' || environment.arch === 'arm64' ? environment.arch : undefined
  if (os === undefined || cpu === undefined) return undefined
  return `${os}-${cpu}` as EnginePlatform
}

/** npm package carrying the prebuilt engine for one platform. */
export function enginePackageName(platform: EnginePlatform): string {
  return `@ccteam/engine-${platform}`
}

/**
 * `$CCTEAM_HOME` > `~/.ccteam`, symlinks resolved.
 *
 * The canonicalization is load-bearing rather than cosmetic: `/health` reports
 * `std::fs::canonicalize($CCTEAM_HOME)`, and this value is compared against it
 * to decide whether an already-running daemon is ours. A daemon started
 * through `/home/u/.ccteam` and a plugin resolving `/home/u/link/.ccteam`
 * would otherwise look like two different homes and the plugin would refuse to
 * attach to its own engine.
 */
export function resolveCcteamHome(environment: EngineEnvironment): string {
  const pinned = (environment.env.CCTEAM_HOME ?? '').trim()
  const root = pinned !== '' ? pinned : join(environment.homedir(), '.ccteam')
  return canonicalPath(root)
}

/** Resolve symlinks when the path exists; report it as given when it does not. */
export function canonicalPath(path: string): string {
  try {
    return realpathSync(path)
  } catch {
    return path
  }
}

/**
 * Where install.sh's DEFAULT rung puts the binary. Discovery uses only this
 * rung (plus PATH); the install ladder in `install.ts` has one more.
 */
export function canonicalInstallDir(environment: EngineEnvironment): string {
  const pinned = (environment.env.CCTEAM_INSTALL_DIR ?? '').trim()
  if (pinned !== '') return pinned
  return join(environment.homedir(), '.local', 'bin')
}

export function canonicalBinaryPath(environment: EngineEnvironment): string {
  return join(canonicalInstallDir(environment), 'ccteam')
}

export function daemonLogPath(home: string): string {
  return join(home, 'daemon.log')
}

/**
 * `$CCTEAM_HOME/run/daemon-endpoint.json` — where a RUNNING daemon publishes
 * the address it actually bound (`ccteam_core::daemon::endpoint_path`).
 */
export function endpointPath(home: string): string {
  return join(home, 'run', 'daemon-endpoint.json')
}

/** The published endpoint, as the engine writes it (snake_case on the wire). */
export interface DaemonEndpoint {
  pid: number
  webBind: string
}

/** Alive-or-not, without signalling: `kill(pid, 0)`. EPERM still means alive. */
export function processExists(pid: number): boolean {
  if (!Number.isInteger(pid) || pid <= 0) return false
  try {
    process.kill(pid, 0)
    return true
  } catch (error) {
    return (error as { code?: unknown } | null)?.code === 'EPERM'
  }
}

/**
 * Read the endpoint pointer, but ONLY while its publisher is alive.
 *
 * A SIGKILLed daemon leaves the file behind, and dialing that address would
 * point this plugin at whatever now owns the port — quite possibly another
 * user's daemon, which is the one mistake the home check exists to catch and
 * the one it could not catch, since the answer would come from a real
 * `/health`. The engine applies the same pid gate on its side
 * (`ccteam_core::daemon::read_endpoint`).
 */
export function readDaemonEndpoint(
  home: string,
  isAlive: (pid: number) => boolean = processExists,
): DaemonEndpoint | undefined {
  let parsed: { pid?: unknown; web_bind?: unknown }
  try {
    parsed = JSON.parse(readFileSync(endpointPath(home), 'utf8')) as { pid?: unknown; web_bind?: unknown }
  } catch {
    return undefined
  }
  const pid = typeof parsed.pid === 'number' ? parsed.pid : undefined
  const webBind = typeof parsed.web_bind === 'string' ? parsed.web_bind.trim() : ''
  if (pid === undefined || webBind === '' || !isAlive(pid)) return undefined
  return { pid, webBind }
}

/**
 * Turn a BIND address into one that can be DIALED. `0.0.0.0` and `[::]` mean
 * "every interface" to a listener and "no host" to a client, so they become
 * loopback — the interface a plugin on the same machine should use anyway.
 */
export function dialableUrl(bind: string): string | undefined {
  const trimmed = bind.trim()
  if (trimmed === '') return undefined
  const at = trimmed.lastIndexOf(':')
  if (at <= 0) return undefined
  const host = trimmed.slice(0, at).replace(/^\[|\]$/g, '')
  const port = trimmed.slice(at + 1)
  if (!/^[0-9]+$/.test(port)) return undefined
  const dialHost = host === '0.0.0.0' || host === '' ? '127.0.0.1' : host === '::' ? '[::1]' : host.includes(':') ? `[${host}]` : host
  return `http://${dialHost}:${port}`
}

/**
 * Where the running daemon actually is, when nobody named an address.
 *
 * The launcher's recorded argv cannot answer this (`:0` requests "any free
 * port"), and neither can a compiled default: a CLI user who started their
 * daemon on another port would otherwise be invisible to the plugin, which
 * would then start a SECOND one — the exact coexistence failure the pointer
 * exists to prevent.
 */
export function discoverDaemonUrl(
  home: string,
  isAlive: (pid: number) => boolean = processExists,
): string | undefined {
  const endpoint = readDaemonEndpoint(home, isAlive)
  return endpoint === undefined ? undefined : dialableUrl(endpoint.webBind)
}

/**
 * The ONE file the credential bootstrap is allowed to read (`secrets/` is
 * 0700 and same-uid). Everything else a session needs is asked of the daemon.
 */
export function webTokenPath(home: string): string {
  return join(home, 'secrets', 'web-token')
}

export function isExecutableFile(path: string): boolean {
  try {
    if (!statSync(path).isFile()) return false
    accessSync(path, constants.X_OK)
    return true
  } catch {
    return false
  }
}

/** First executable named `ccteam` on `PATH`, in PATH order. */
export function findOnPath(environment: EngineEnvironment, name = 'ccteam'): string | undefined {
  const raw = environment.env.PATH ?? ''
  for (const entry of raw.split(delimiter)) {
    const dir = entry.trim()
    if (dir === '') continue
    const candidate = join(dir, name)
    if (isExecutableFile(candidate)) return candidate
  }
  return undefined
}

/**
 * `ccteam --version` prints `ccteam 0.10.5 (<commit>)`; take the first
 * dotted-numeric token of the first line — byte-identical to
 * `update.rs::parse_version_output`, including the leading-`v` strip.
 */
export function parseVersionOutput(text: string): string | undefined {
  const line = text.split('\n')[0]
  if (line === undefined) return undefined
  for (const token of line.split(/\s+/)) {
    const stripped = token.replace(/^v/, '')
    if (stripped.includes('.') && /^[0-9]/.test(stripped)) return stripped
  }
  return undefined
}

export async function binaryVersion(binary: string, run: RunFn = runCommand): Promise<string | undefined> {
  const result = await run(binary, ['--version'], { timeoutMs: 10_000 })
  if (result.code !== 0) return undefined
  return parseVersionOutput(result.stdout)
}

export interface EngineLocation {
  /** False on a platform with no published engine; nothing else is attempted. */
  supported: boolean
  platform?: EnginePlatform
  /** Canonical `$CCTEAM_HOME`. Resolved even when unsupported: it is env, not arch. */
  home: string
  binary?: string
  source?: EngineBinarySource
  version?: string
  /** A binary was found but did not answer `--version` — honest, not silent. */
  unreadable?: boolean
}

export interface LocateOptions {
  environment?: EngineEnvironment
  run?: RunFn
  /** `enginePath` from the settings card (advanced): an explicit override. */
  configuredPath?: string
}

/**
 * Find the engine: the configured path, then `PATH`, then the canonical
 * install location. PATH comes before the canonical path because PATH is what
 * the user's own shell — and every `ccteam` command they type — resolves.
 */
export async function locateEngine(options: LocateOptions = {}): Promise<EngineLocation> {
  const environment = options.environment ?? defaultEnvironment()
  const run = options.run ?? runCommand
  const home = resolveCcteamHome(environment)
  const platform = enginePlatform(environment)
  if (platform === undefined) return { supported: false, home }

  const configured = (options.configuredPath ?? '').trim()
  const candidates: Array<{ path: string; source: EngineBinarySource }> = []
  if (configured !== '') candidates.push({ path: configured, source: 'configured' })
  const onPath = findOnPath(environment)
  if (onPath !== undefined) candidates.push({ path: onPath, source: 'path' })
  const canonical = canonicalBinaryPath(environment)
  if (!candidates.some(c => c.path === canonical)) candidates.push({ path: canonical, source: 'canonical' })

  for (const candidate of candidates) {
    if (!isExecutableFile(candidate.path)) continue
    const version = await binaryVersion(candidate.path, run)
    return {
      supported: true,
      platform,
      home,
      binary: candidate.path,
      source: candidate.source,
      ...(version === undefined ? { unreadable: true } : { version }),
    }
  }
  return { supported: true, platform, home }
}

/** Bounded tail window, mirroring `daemon_cli.rs::tail_lines` (unrotated log). */
const TAIL_WINDOW_BYTES = 1024 * 1024

/**
 * Last `n` lines of a file, reading only the trailing window so a large
 * unrotated daemon log never lands in memory whole.
 */
export function tailFile(path: string, n: number): string[] {
  let fd: number | undefined
  try {
    fd = openSync(path, 'r')
    const size = fstatSync(fd).size
    const start = size > TAIL_WINDOW_BYTES ? size - TAIL_WINDOW_BYTES : 0
    const length = size - start
    const buffer = Buffer.alloc(Number(length))
    if (length > 0) readSync(fd, buffer, 0, Number(length), start)
    const lines = buffer.toString('utf8').split('\n')
    // A trailing newline yields a final empty element that is not a line.
    if (lines.at(-1) === '') lines.pop()
    // The window may have cut mid-line; that fragment is not a line either.
    if (start > 0 && lines.length > 0) lines.shift()
    return lines.slice(Math.max(0, lines.length - n))
  } finally {
    if (fd !== undefined) closeSync(fd)
  }
}

/** Directory of a path, exported so the install ladder and tests agree. */
export function parentDir(path: string): string {
  return dirname(path)
}
