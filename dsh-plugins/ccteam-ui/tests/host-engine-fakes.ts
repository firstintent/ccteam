/**
 * Hermetic stand-ins for the two things the engine face talks to: the `ccteam`
 * CLI and a daemon's `GET /health`.
 *
 * Both are REAL — a real executable on a real path, a real HTTP server — for
 * the same reason the transport tests speak real ACP over a real socket: the
 * bugs this file exists to catch (an argv that never reaches the binary, a
 * verdict on the wrong stream, a home that differs by one symlink) all live in
 * the seam a mock would replace.
 *
 * The CLI and the health server share ONE state file, so coexistence behaves
 * the way it does on a machine: `start` publishes a daemon, `daemon stop`
 * withdraws it, and a second `start` reports `alreadyRunning` with the same
 * pid — nobody's mock has to remember to stay consistent.
 */
import { createServer, type Server } from 'node:http'
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readFileSync, realpathSync, rmSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { join } from 'node:path'
import type { AddressInfo } from 'node:net'
import type { EngineEnvironment } from '../src/host/engine/locate.js'

export interface Sandbox {
  root: string
  home: string
  installDir: string
  binDir: string
  /** Where the fake CLI records what it was asked to do. */
  callLog: string
  /** The daemon the fake CLI publishes; absent = nothing is running. */
  healthFile: string
  cleanup(): void
}

export function makeSandbox(name = 'engine'): Sandbox {
  // realpath so the canonical home the plugin resolves equals the one the fake
  // daemon reports; a bare mkdtemp differs on macOS (/var vs /private/var).
  const root = realpathSync(mkdtempSync(join(tmpdir(), `ccteam-${name}-`)))
  const home = join(root, 'home', '.ccteam')
  const installDir = join(root, 'bin')
  const binDir = join(root, 'path')
  mkdirSync(home, { recursive: true })
  mkdirSync(installDir, { recursive: true })
  mkdirSync(binDir, { recursive: true })
  return {
    root,
    home,
    installDir,
    binDir,
    callLog: join(root, 'calls.log'),
    healthFile: join(root, 'health.json'),
    cleanup: () => rmSync(root, { recursive: true, force: true }),
  }
}

export interface FakeCliOptions {
  /** Version the fake answers `--version` with. */
  version?: string
  /** `home` the started daemon reports; defaults to the sandbox home. */
  home?: string
  /** Make `start` fail with this JSON error code. */
  startError?: string
  /** Make `daemon stop` fail with this JSON error code. */
  stopError?: string
}

/**
 * Write a `ccteam` that honours the argv the supervisor actually sends:
 * `--version`, `start --json` (and its `daemon start --json` spelling),
 * `daemon stop --json`, `daemon status --json`, and
 * `update --channel npm --binary <path> --json`.
 */
export function writeFakeCcteam(sandbox: Sandbox, path: string, options: FakeCliOptions = {}): string {
  const version = options.version ?? '0.10.3'
  const home = options.home ?? sandbox.home
  const script = `#!/usr/bin/env bash
set -u
LOG=${JSON.stringify(sandbox.callLog)}
HEALTH=${JSON.stringify(sandbox.healthFile)}
VERSION=${JSON.stringify(version)}
HOME_DIR=${JSON.stringify(home)}
echo "$*" >> "$LOG"

publish() {
  cat > "$HEALTH" <<JSON
{"status":"ok","version":"$VERSION","build":"fake","home":"$HOME_DIR","pid":$1,"web_bind":"127.0.0.1:7331","dsh_web_bind":null,"uptime_secs":1}
JSON
}

case "$1" in
  --version)
    echo "ccteam $VERSION (fake)"
    ;;
  start|daemon)
    sub="$1"
    if [ "$sub" = "daemon" ]; then sub="$2"; fi
    case "$sub" in
      start)
        ${options.startError === undefined
          ? `if [ -f "$HEALTH" ]; then
          pid=$(sed -n 's/.*"pid":\\([0-9]*\\).*/\\1/p' "$HEALTH")
          echo "{\\"status\\":\\"alreadyRunning\\",\\"pid\\":$pid,\\"version\\":\\"$VERSION\\",\\"home\\":\\"$HOME_DIR\\"}"
        else
          pid=$$
          publish "$pid"
          echo "{\\"status\\":\\"started\\",\\"pid\\":$pid,\\"version\\":\\"$VERSION\\",\\"home\\":\\"$HOME_DIR\\"}"
        fi`
          : `echo "{\\"status\\":\\"error\\",\\"code\\":\\"${options.startError}\\",\\"message\\":\\"fake start refused\\"}"
        exit 1`}
        ;;
      stop)
        ${options.stopError === undefined
          ? `if [ -f "$HEALTH" ]; then
          pid=$(sed -n 's/.*"pid":\\([0-9]*\\).*/\\1/p' "$HEALTH")
          rm -f "$HEALTH"
          echo "{\\"status\\":\\"stopped\\",\\"pid\\":$pid}"
        else
          echo '{"status":"notRunning"}'
        fi`
          : `echo "{\\"status\\":\\"error\\",\\"code\\":\\"${options.stopError}\\",\\"message\\":\\"fake stop refused\\"}"
        exit 1`}
        ;;
      status)
        if [ -f "$HEALTH" ]; then cat "$HEALTH"; else echo '{"status":"down"}'; fi
        ;;
      *)
        echo "{\\"status\\":\\"error\\",\\"code\\":\\"badArgs\\",\\"message\\":\\"unknown daemon subcommand\\"}"
        exit 1
        ;;
    esac
    ;;
  update)
    echo "{\\"status\\":\\"updated\\",\\"version\\":\\"$VERSION\\"}"
    ;;
  *)
    echo "{\\"status\\":\\"error\\",\\"code\\":\\"badArgs\\",\\"message\\":\\"unknown command\\"}"
    exit 1
    ;;
esac
`
  writeFileSync(path, script)
  chmodSync(path, 0o755)
  return path
}

/**
 * The engine inside the plugin's platform package. It is the SAME fake CLI the
 * PATH copy is, because that is what makes the install path end-to-end: once
 * copied into place, it has to behave like an engine, not like a stub.
 */
export function writeEnginePackageBin(sandbox: Sandbox, dir: string, version: string): string {
  mkdirSync(dir, { recursive: true })
  return writeFakeCcteam(sandbox, join(dir, 'ccteam'), { version })
}

/**
 * What the supervisor ASKED the CLI to do. `--version` is excluded on purpose:
 * it is a read, like the `/health` probe, and the assertions here are about
 * actions — "did anything start a second daemon", "was the binary touched".
 */
export function calls(sandbox: Sandbox): string[] {
  return allCalls(sandbox).filter(line => line !== '--version')
}

/** Every invocation, version probes included. */
export function allCalls(sandbox: Sandbox): string[] {
  if (!existsSync(sandbox.callLog)) return []
  return readFileSync(sandbox.callLog, 'utf8').split('\n').filter(line => line.trim() !== '')
}

export interface FakeHealth {
  url: string
  /** Requests served, so "attached, did not start" is provable. */
  readonly hits: number
  close(): Promise<void>
}

/**
 * A `/health` endpoint backed by the same state file the fake CLI writes:
 * whatever `ccteam start` published is what a probe sees.
 */
export async function startHealthServer(sandbox: Sandbox, override?: () => unknown): Promise<FakeHealth> {
  let hits = 0
  const server: Server = createServer((req, res) => {
    if (!(req.url ?? '').startsWith('/health')) {
      res.writeHead(404).end()
      return
    }
    hits += 1
    const body = override?.() ?? readState(sandbox)
    if (body === undefined) {
      res.writeHead(503, { 'content-type': 'application/json' }).end('{"status":"down"}')
      return
    }
    res.writeHead(200, { 'content-type': 'application/json' }).end(JSON.stringify(body))
  })
  await new Promise<void>(resolve => server.listen(0, '127.0.0.1', resolve))
  const { port } = server.address() as AddressInfo
  return {
    url: `http://127.0.0.1:${port}`,
    get hits() {
      return hits
    },
    close: () =>
      new Promise<void>(resolve => {
        server.close(() => resolve())
      }),
  }
}

function readState(sandbox: Sandbox): unknown {
  if (!existsSync(sandbox.healthFile)) return undefined
  try {
    return JSON.parse(readFileSync(sandbox.healthFile, 'utf8')) as unknown
  } catch {
    return undefined
  }
}

/** The ambient facts a supervisor reads, pinned to one sandbox. */
export function sandboxEnvironment(sandbox: Sandbox, extra: NodeJS.ProcessEnv = {}): EngineEnvironment {
  return {
    env: {
      // The sandbox bin dir FIRST, then only the system utility dirs. The
      // owner's own `~/.local/bin/ccteam` must never be reachable from a test:
      // this process would then run a real engine against the sandbox home.
      PATH: [sandbox.binDir, '/usr/bin', '/bin'].join(':'),
      CCTEAM_HOME: sandbox.home,
      CCTEAM_INSTALL_DIR: sandbox.installDir,
      ...extra,
    },
    platform: 'linux',
    arch: 'x64',
    homedir: () => join(sandbox.root, 'home'),
  }
}
