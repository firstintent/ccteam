/**
 * The engine face: locate → install → supervise.
 *
 * Every assertion below is about the ONE invariant the coexistence table is
 * made of (PRD v0.10.5 §5): the daemon outlives this plugin, and whoever
 * started it first wins. The interesting cases are all refusals — do not start
 * a second daemon, do not swap a binary under a running one, do not stop
 * anything on dispose — so the tests assert what the fake CLI was NOT asked to
 * do as often as what it was.
 */
import { chmodSync, existsSync, lstatSync, mkdirSync, readFileSync, realpathSync, symlinkSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { afterEach, describe, expect, it } from 'vitest'
import { EngineSupervisor, isLoopbackUrl, lastJsonObject } from '../src/host/engine/supervisor.js'
import {
  canonicalBinaryPath,
  dialableUrl,
  discoverDaemonUrl,
  endpointPath,
  findOnPath,
  isExecutableFile,
  locateEngine,
  parseVersionOutput,
  processExists,
  readDaemonEndpoint,
  resolveCcteamHome,
  tailFile,
  webTokenPath,
} from '../src/host/engine/locate.js'
import { classifyDestPath, installEngine, resolveInstallDirWith } from '../src/host/engine/install.js'
import { createEnrollmentBootstrap, createTokenBootstrap } from '../src/host/engine/bootstrap.js'
import {
  calls,
  makeSandbox,
  sandboxEnvironment,
  startHealthServer,
  writeEnginePackageBin,
  writeFakeCcteam,
  type FakeHealth,
  type Sandbox,
} from './host-engine-fakes.js'

const ENGINE_VERSION = '0.10.3'
const root = dirname(fileURLToPath(import.meta.url))

const sandboxes: Sandbox[] = []
const servers: FakeHealth[] = []

function sandbox(name?: string): Sandbox {
  const created = makeSandbox(name)
  sandboxes.push(created)
  return created
}

afterEach(async () => {
  for (const server of servers.splice(0)) await server.close()
  for (const created of sandboxes.splice(0)) created.cleanup()
})

interface Harness {
  sbx: Sandbox
  health: FakeHealth
  supervisor: EngineSupervisor
}

/**
 * A supervisor wired to one sandbox. `options.installed` decides whether a
 * `ccteam` is already on PATH; `packageVersion` is what the plugin's platform
 * package would install.
 */
async function harness(options: {
  installed?: string | false
  autoStart?: boolean
  managed?: boolean
  externallyOwned?: boolean
  packageVersion?: string | false
  daemonHome?: string
  healthOverride?: () => unknown
} = {}): Promise<Harness> {
  const sbx = sandbox()
  const health = await startHealthServer(sbx, options.healthOverride)
  servers.push(health)
  if (options.installed !== false) {
    writeFakeCcteam(sbx, join(sbx.binDir, 'ccteam'), {
      version: options.installed ?? ENGINE_VERSION,
      ...(options.daemonHome === undefined ? {} : { home: options.daemonHome }),
    })
  }
  const packageBin =
    options.packageVersion === false
      ? undefined
      : writeEnginePackageBin(sbx, join(sbx.root, 'pkg', 'bin'), options.packageVersion ?? ENGINE_VERSION)
  const supervisor = new EngineSupervisor({
    daemonUrl: () => health.url,
    // The sandbox's port is a NAMED address: the tests exercise the same
    // layer a human filling in the settings card would.
    configuredDaemonUrl: () => health.url,
    autoStart: () => options.autoStart ?? true,
    pinnedVersion: ENGINE_VERSION,
    managed: options.managed ?? false,
    externallyOwned: options.externallyOwned ?? false,
    environment: sandboxEnvironment(sbx),
    resolvePackageBin: () => packageBin,
    readyTimeoutMs: 3_000,
    readyPollMs: 25,
    probeTimeoutMs: 2_000,
  })
  return { sbx, health, supervisor }
}

describe('locating the engine', () => {
  it('resolves the home and the canonical binary path exactly as the engine does', () => {
    const sbx = sandbox()
    const environment = sandboxEnvironment(sbx)

    expect(resolveCcteamHome(environment)).toBe(sbx.home)
    expect(canonicalBinaryPath(environment)).toBe(join(sbx.installDir, 'ccteam'))
    expect(webTokenPath(sbx.home)).toBe(join(sbx.home, 'secrets', 'web-token'))

    // No CCTEAM_HOME: `~/.ccteam`, the same fallback as CcteamPaths::from_env.
    const bare = { ...environment, env: { PATH: sbx.binDir } }
    expect(resolveCcteamHome(bare)).toBe(join(sbx.root, 'home', '.ccteam'))
  })

  it('reports Unsupported on a platform ccteam publishes no engine for', async () => {
    const sbx = sandbox()
    const location = await locateEngine({
      environment: { ...sandboxEnvironment(sbx), platform: 'win32', arch: 'x64' },
    })

    expect(location.supported).toBe(false)
    expect(location.platform).toBeUndefined()
    expect(location.binary).toBeUndefined()
    // The home is env, not architecture: still resolved, still honest.
    expect(location.home).toBe(sbx.home)

    const { supervisor } = await harness()
    const unsupported = new EngineSupervisor({
      daemonUrl: () => 'http://127.0.0.1:1',
      autoStart: () => true,
      pinnedVersion: ENGINE_VERSION,
      environment: { ...sandboxEnvironment(sbx), platform: 'win32', arch: 'x64' },
    })
    const status = await unsupported.ensure()
    expect(status.state).toBe('unsupported')
    expect(status.supervised).toBe(false)
    expect(status.detail).toContain('win32-x64')
    void supervisor
  })

  it('prefers PATH over the canonical path and parses the version the way the engine does', async () => {
    const sbx = sandbox()
    writeFakeCcteam(sbx, join(sbx.binDir, 'ccteam'), { version: '9.8.7' })
    writeFakeCcteam(sbx, join(sbx.installDir, 'ccteam'), { version: '1.2.3' })

    const location = await locateEngine({ environment: sandboxEnvironment(sbx) })
    expect(location.source).toBe('path')
    expect(location.binary).toBe(join(sbx.binDir, 'ccteam'))
    expect(location.version).toBe('9.8.7')
    expect(findOnPath(sandboxEnvironment(sbx))).toBe(join(sbx.binDir, 'ccteam'))

    expect(parseVersionOutput('ccteam 0.10.5 (abc1234)\n')).toBe('0.10.5')
    expect(parseVersionOutput('ccteam v1.2.3\n')).toBe('1.2.3')
    expect(parseVersionOutput('usage: something\n')).toBeUndefined()
  })
})

describe('installing from the platform package', () => {
  it('installs into the canonical location and verifies the copy before publishing it', async () => {
    const sbx = sandbox()
    const source = writeEnginePackageBin(sbx, join(sbx.root, 'pkg', 'bin'), '0.10.3')
    const dest = join(sbx.installDir, 'ccteam')

    const outcome = await installEngine({ source, dest })

    expect(outcome).toMatchObject({ ok: true, binary: dest, version: '0.10.3' })
    expect(existsSync(dest)).toBe(true)
    // 755, or the install is a binary nothing can run.
    expect(isExecutableFile(dest)).toBe(true)
    // The staging name never survives a success.
    expect(existsSync(`${dest}.ccteam-new-${process.pid}`)).toBe(false)
  })

  it('never overwrites a symlinked or package-manager-owned install', async () => {
    const sbx = sandbox()
    const source = writeEnginePackageBin(sbx, join(sbx.root, 'pkg', 'bin'), '0.10.3')
    const real = join(sbx.root, 'elsewhere-ccteam')
    writeFileSync(real, '#!/bin/sh\n')
    chmodSync(real, 0o755)
    const link = join(sbx.installDir, 'ccteam')
    symlinkSync(real, link)

    const outcome = await installEngine({ source, dest: link })

    expect(outcome.ok).toBe(false)
    expect(outcome).toMatchObject({ errorKind: 'destIsSymlink' })
    expect(readFileSync(real, 'utf8')).toBe('#!/bin/sh\n')

    // Same refusal by path shape, before the file even exists.
    expect(classifyDestPath('/home/u/node_modules/.bin/ccteam')).toContain('node_modules')
    expect(classifyDestPath('/nix/store/abc/bin/ccteam')).toBe('nix')
    expect(classifyDestPath('/home/u/.local/bin/ccteam')).toBeUndefined()
    const managed = await installEngine({ source, dest: '/home/u/node_modules/.bin/ccteam' })
    expect(managed).toMatchObject({ ok: false, errorKind: 'destPackageManaged' })
  })

  it('refuses bytes that do not answer --version, leaving the previous engine in place', async () => {
    const sbx = sandbox()
    const dest = join(sbx.installDir, 'ccteam')
    writeFileSync(dest, '#!/bin/sh\necho "ccteam 0.10.3"\n')
    chmodSync(dest, 0o755)
    const junk = join(sbx.root, 'junk')
    writeFileSync(junk, '#!/bin/sh\nexit 3\n')
    chmodSync(junk, 0o755)

    const outcome = await installEngine({ source: junk, dest })

    expect(outcome).toMatchObject({ ok: false, errorKind: 'binaryVersionUnreadable' })
    expect(readFileSync(dest, 'utf8')).toContain('0.10.3')
  })

  /**
   * Rung-by-rung parity with the shell ladder, in install.sh's order — the
   * same table `update.rs::install_dir_ladder_walks_install_sh_rungs_in_order`
   * walks, with the same fixture paths, so a change to one ladder that is not
   * made to the other shows up as a diff between two tests rather than as a
   * user with two ccteam binaries.
   */
  it('walks install.sh’s rungs in order, exactly as the Rust copy does', () => {
    const home = '/home/u'
    const exec = (p: string): boolean =>
      ['/opt/first/ccteam', '/home/u/.local/bin/ccteam', '/ro/ccteam', '/src/target/debug/ccteam'].includes(p)
    const writable = (d: string): boolean => d !== '/ro'
    const ladder = (env: string | undefined, path: string | undefined): string =>
      resolveInstallDirWith(env, path, home, exec, writable)

    // Rung 1 — the explicit override wins over everything…
    expect(ladder('/custom', '/opt/first:/home/u/.local/bin')).toBe('/custom')
    // …but an EMPTY override is not an override.
    expect(ladder('', '/opt/first')).toBe('/opt/first')

    // Rung 2 — `command -v ccteam`: the FIRST PATH hit, which is the binary a
    // shell would run. Not whatever discovery picked: installing beside a
    // shadowing copy instead of over it is the whole failure.
    expect(ladder(undefined, '/nope:/opt/first:/home/u/.local/bin')).toBe('/opt/first')
    // Rung 2 skips a cargo build tree (`cargo clean` would delete it)…
    expect(ladder(undefined, '/src/target/debug:/home/u/.local/bin')).toBe('/home/u/.local/bin')
    // …and a directory it cannot write.
    expect(ladder(undefined, '/ro')).toBe('/home/u/.local/bin')
    // POSIX says an empty PATH entry means the current directory; an installer
    // must not honour that.
    expect(ladder(undefined, ':/opt/first')).toBe('/opt/first')

    // Rung 3 — nothing on PATH, no PATH at all.
    expect(ladder(undefined, '/nope')).toBe('/home/u/.local/bin')
    expect(ladder(undefined, undefined)).toBe('/home/u/.local/bin')
  })

  /**
   * Drift guard, the same one the Rust copy carries: this ladder exists in
   * THREE places (install.sh, update.rs, here), and a copy with no test rots.
   * Read the shell source and require every rung to still be there — if
   * install.sh's ladder changes, both copies fail rather than silently
   * disagreeing.
   */
  it('still mirrors every rung install.sh actually has', () => {
    const script = readFileSync(join(root, '..', '..', '..', 'install.sh'), 'utf8')
    const ladder = script.split('resolve_install_dir() {')[1]?.split('\n}')[0]
    expect(ladder, 'install.sh still defines resolve_install_dir()').toBeDefined()

    for (const [rung, marker] of [
      ['1: explicit override', 'CCTEAM_INSTALL_DIR'],
      ['2: PATH lookup', 'command -v ccteam'],
      ['2: symlink resolution', 'canonical_bin'],
      ['2: build-tree exclusion', '*/target/release|*/target/debug'],
      ['2: writability', '-w "$_dir"'],
      ['3: default', '$HOME/.local/bin'],
    ] as const) {
      expect(ladder, `install.sh's ladder lost rung ${rung} (marker ${marker})`).toContain(marker)
    }
    // Order matters: the override is checked before the PATH lookup.
    expect(ladder!.indexOf('CCTEAM_INSTALL_DIR')).toBeLessThan(ladder!.indexOf('command -v ccteam'))
  })

  /**
   * The repro the ladder's symlink rung exists for. A link earlier on PATH
   * pointing at the real install is an ordinary setup (`~/bin/ccteam ->
   * /usr/local/bin/ccteam`, a package manager's `bin` shim). Taking the
   * parent of the UNRESOLVED entry targets the link itself, which
   * `classifyDestination` then refuses — so the plugin would report
   * `destIsSymlink` and install nothing, on a machine where install.sh
   * upgrades cleanly.
   */
  it('installs beside the real binary when PATH finds it through a symlink', async () => {
    const sbx = sandbox()
    const realDir = join(sbx.root, 'real-bin')
    const pathDir = join(sbx.root, 'path-bin')
    mkdirSync(realDir, { recursive: true })
    mkdirSync(pathDir, { recursive: true })
    const real = writeFakeCcteam(sbx, join(realDir, 'ccteam'))
    const link = join(pathDir, 'ccteam')
    symlinkSync(real, link)

    // The link is first on PATH and its directory IS writable — the rung-2
    // filters alone would happily choose it.
    const resolved = resolveInstallDirWith(undefined, `${pathDir}:${realDir}`, join(sbx.root, 'home'))
    expect(resolved).toBe(realDir)

    const source = writeEnginePackageBin(sbx, join(sbx.root, 'pkg', 'bin'), ENGINE_VERSION)
    // What an unresolved-parent ladder would have targeted, and why that is
    // not merely cosmetic: the link is refused, so nothing installs at all.
    expect(await installEngine({ source, dest: link })).toMatchObject({
      ok: false,
      errorKind: 'destIsSymlink',
    })

    const outcome = await installEngine({ source, dest: join(resolved, 'ccteam') })

    expect(outcome).toMatchObject({ ok: true, binary: join(realDir, 'ccteam'), version: ENGINE_VERSION })
    // The link is untouched and still points at the binary that was upgraded.
    expect(lstatSync(link).isSymbolicLink()).toBe(true)
    expect(realpathSync(link)).toBe(realpathSync(join(realDir, 'ccteam')))
  })
})

describe('apply(): probe, attach, start', () => {
  it('attaches to a daemon that is already running instead of starting a second one', async () => {
    const { sbx, supervisor } = await harness()
    // Somebody else — the CLI, systemd, another front end — started it first.
    writeFileSync(
      sbx.healthFile,
      JSON.stringify({ status: 'ok', version: ENGINE_VERSION, home: sbx.home, pid: 31337, web_bind: '127.0.0.1:7331' }),
    )

    const status = await supervisor.ensure()

    expect(status.state).toBe('attached')
    expect(status.reachable).toBe(true)
    expect(status.pid).toBe(31337)
    expect(status.daemonHome).toBe(sbx.home)
    expect(status.webBind).toBe('127.0.0.1:7331')
    // The whole point: not one CLI action ran.
    expect(calls(sbx)).toEqual([])
  })

  it('starts the daemon when nothing is running and auto-start is on', async () => {
    const { sbx, supervisor } = await harness()

    const status = await supervisor.ensure()

    expect(status.state).toBe('running')
    expect(status.pid).toBeGreaterThan(0)
    expect(calls(sbx).filter(line => line.includes('start'))).toHaveLength(1)
  })

  /**
   * The daemon's bind is a compiled default with no config-file key, so a
   * plugin pointed anywhere else has to say where — and a plugin pointed at
   * the default must NOT, or it would narrow a CLI user's `0.0.0.0` console
   * to loopback without being asked.
   */
  it('forwards a non-default daemon URL as --web-bind, and forwards nothing otherwise', async () => {
    const { sbx, supervisor } = await harness()
    await supervisor.start()

    const started = calls(sbx).find(line => line.startsWith('start'))
    expect(started).toContain('--web-bind 127.0.0.1:')
    expect(started).toContain(new URL((await supervisor.status()).daemonUrl).port)

    const sbx2 = sandbox()
    writeFakeCcteam(sbx2, join(sbx2.binDir, 'ccteam'))
    const defaultUrl = new EngineSupervisor({
      daemonUrl: () => 'http://127.0.0.1:7331',
      configuredDaemonUrl: () => undefined,
      autoStart: () => true,
      pinnedVersion: ENGINE_VERSION,
      environment: sandboxEnvironment(sbx2),
      // Nothing is listening on the default port in this test — and the probe
      // is stubbed rather than left to the real socket precisely so a daemon
      // that happens to run on THIS machine cannot decide the outcome.
      fetchImpl: async () => {
        throw new Error('connection refused')
      },
      readyTimeoutMs: 200,
      readyPollMs: 50,
      probeTimeoutMs: 200,
    })
    await defaultUrl.start()
    expect(calls(sbx2)).toEqual(['start --json'])
  })

  it('treats alreadyRunning as success, with the same pid', async () => {
    const { supervisor } = await harness()
    const first = await supervisor.start()
    const second = await supervisor.start()

    expect(first.ok).toBe(true)
    expect(second.ok).toBe(true)
    expect(second.status.pid).toBe(first.status.pid)
    expect(second.status.state).toBe('running')
  })

  it('stays Stopped when auto-start is off, and still starts on an explicit action', async () => {
    const { sbx, supervisor } = await harness({ autoStart: false })

    const status = await supervisor.ensure()

    expect(status.state).toBe('stopped')
    expect(status.autoStart).toBe(false)
    expect(calls(sbx).some(line => line.includes('start'))).toBe(false)

    const started = await supervisor.start()
    expect(started.ok).toBe(true)
    expect(started.status.state).toBe('running')
  })

  it('reports Missing (not a crash) when no engine and no platform package exist', async () => {
    const { sbx, supervisor } = await harness({ installed: false, packageVersion: false })

    const status = await supervisor.ensure()

    expect(status.state).toBe('missing')
    expect(status.binary).toBeUndefined()
    expect(calls(sbx)).toEqual([])
  })

  it('installs the engine from the platform package, then starts it', async () => {
    const { sbx, supervisor } = await harness({ installed: false })

    const status = await supervisor.ensure()

    expect(existsSync(join(sbx.installDir, 'ccteam'))).toBe(true)
    expect(status.state).toBe('running')
    expect(status.binary).toBe(join(sbx.installDir, 'ccteam'))
  })
})

describe('finding a daemon nobody named', () => {
  /**
   * The compiled default is not where a daemon necessarily is: `--web-bind`
   * has no config-file key, and `:0` means "any free port". So the RUNNING
   * daemon publishes its address, and a plugin that was told nothing reads it
   * — otherwise a CLI user on another port is invisible and the plugin starts
   * a second daemon beside theirs.
   */
  it('uses the endpoint the running daemon published', () => {
    const sbx = sandbox()
    mkdirSync(join(sbx.home, 'run'), { recursive: true })
    writeFileSync(
      endpointPath(sbx.home),
      JSON.stringify({ pid: process.pid, web_bind: '127.0.0.1:19222' }),
    )

    expect(readDaemonEndpoint(sbx.home)).toEqual({ pid: process.pid, webBind: '127.0.0.1:19222' })
    expect(discoverDaemonUrl(sbx.home)).toBe('http://127.0.0.1:19222')
  })

  it('ignores an endpoint whose publisher is gone', () => {
    const sbx = sandbox()
    mkdirSync(join(sbx.home, 'run'), { recursive: true })
    writeFileSync(endpointPath(sbx.home), JSON.stringify({ pid: process.pid, web_bind: '127.0.0.1:19222' }))

    // A SIGKILLed daemon leaves the file behind; dialing that port would reach
    // whatever now owns it — possibly another user's daemon.
    expect(discoverDaemonUrl(sbx.home, () => false)).toBeUndefined()
    expect(readDaemonEndpoint(sbx.home, () => false)).toBeUndefined()
    expect(processExists(0)).toBe(false)
    expect(processExists(process.pid)).toBe(true)

    // Absent, or unparseable, is the same answer.
    const empty = sandbox()
    expect(discoverDaemonUrl(empty.home)).toBeUndefined()
    mkdirSync(join(empty.home, 'run'), { recursive: true })
    writeFileSync(endpointPath(empty.home), 'not json')
    expect(discoverDaemonUrl(empty.home)).toBeUndefined()
  })

  it('dials a wildcard bind on loopback, because 0.0.0.0 is not an address to call', () => {
    expect(dialableUrl('0.0.0.0:7331')).toBe('http://127.0.0.1:7331')
    expect(dialableUrl('127.0.0.1:9000')).toBe('http://127.0.0.1:9000')
    expect(dialableUrl('[::]:7331')).toBe('http://[::1]:7331')
    expect(dialableUrl('192.168.1.5:7331')).toBe('http://192.168.1.5:7331')
    expect(dialableUrl('')).toBeUndefined()
    expect(dialableUrl('nonsense')).toBeUndefined()
  })

  it('reports how the address was found, and never binds a discovered one', async () => {
    const { sbx, supervisor, health } = await harness()
    expect((await supervisor.status()).daemonUrlSource).toBe('configured')

    const discovering = new EngineSupervisor({
      daemonUrl: () => health.url,
      configuredDaemonUrl: () => undefined,
      autoStart: () => true,
      pinnedVersion: ENGINE_VERSION,
      environment: sandboxEnvironment(sbx),
      readyTimeoutMs: 2_000,
      readyPollMs: 25,
      probeTimeoutMs: 1_000,
    })
    mkdirSync(join(sbx.home, 'run'), { recursive: true })
    writeFileSync(
      endpointPath(sbx.home),
      JSON.stringify({ pid: process.pid, web_bind: health.url.replace('http://', '') }),
    )
    expect((await discovering.status()).daemonUrlSource).toBe('endpoint')

    await discovering.start()
    // A discovered address describes a daemon that is ALREADY running, so it
    // must never be handed to a start as a bind.
    expect(calls(sbx).find(line => line.startsWith('start'))).toBe('start --json')
  })
})

describe('coexistence refusals', () => {
  it('reports Mismatch{home} and never starts a second daemon', async () => {
    const { sbx, supervisor } = await harness({ daemonHome: '/somewhere/else/.ccteam' })
    // A daemon in a different home is already up.
    writeFileSync(
      sbx.healthFile,
      JSON.stringify({ status: 'ok', version: ENGINE_VERSION, home: '/somewhere/else/.ccteam', pid: 4242 }),
    )

    const status = await supervisor.ensure()

    expect(status.state).toBe('mismatch')
    expect(status.mismatch).toBe('home')
    expect(status.daemonHome).toBe('/somewhere/else/.ccteam')
    expect(status.home).toBe(sbx.home)
    expect(calls(sbx)).toEqual([])

    const started = await supervisor.start()
    expect(started.ok).toBe(false)
    expect(calls(sbx)).toEqual([])
  })

  it('reports Mismatch{version} and leaves the running daemon’s binary untouched', async () => {
    const { sbx, supervisor } = await harness({ installed: '0.9.0' })
    writeFileSync(
      sbx.healthFile,
      JSON.stringify({ status: 'ok', version: '0.9.0', home: sbx.home, pid: 777 }),
    )
    const before = readFileSync(join(sbx.binDir, 'ccteam'), 'utf8')

    const status = await supervisor.ensure()

    expect(status.state).toBe('mismatch')
    expect(status.mismatch).toBe('version')
    expect(status.runningVersion).toBe('0.9.0')
    expect(status.pinnedVersion).toBe(ENGINE_VERSION)
    expect(status.pid).toBe(777)
    // Not a byte moved, and no install landed in the canonical location.
    expect(readFileSync(join(sbx.binDir, 'ccteam'), 'utf8')).toBe(before)
    expect(existsSync(join(sbx.installDir, 'ccteam'))).toBe(false)
    expect(calls(sbx)).toEqual([])
  })

  it('repairs a version mismatch only through `ccteam update --channel npm --binary`', async () => {
    const { sbx, supervisor } = await harness({ installed: '0.9.0' })
    writeFileSync(sbx.healthFile, JSON.stringify({ status: 'ok', version: '0.9.0', home: sbx.home, pid: 777 }))

    const result = await supervisor.update()

    expect(result.ok).toBe(true)
    const update = calls(sbx).find(line => line.startsWith('update'))
    expect(update).toBeDefined()
    expect(update).toContain('--channel npm --binary')
    expect(update).toContain('--json')
    expect(update).toContain(join(sbx.root, 'pkg', 'bin', 'ccteam'))
  })

  it('is inert inside a ccteam-managed DSH runtime: it attaches, it never manages', async () => {
    const { sbx, supervisor } = await harness({ managed: true, installed: false })
    writeFileSync(sbx.healthFile, JSON.stringify({ status: 'ok', version: ENGINE_VERSION, home: sbx.home, pid: 99 }))

    const status = await supervisor.ensure()

    expect(status.state).toBe('attached')
    expect(status.supervised).toBe(false)
    expect(status.unsupervisedReason).toBe('managed')
    expect(existsSync(join(sbx.installDir, 'ccteam'))).toBe(false)
    expect(calls(sbx)).toEqual([])

    for (const action of [supervisor.start(), supervisor.stop(), supervisor.restart(), supervisor.update()]) {
      const refused = await action
      expect(refused.ok).toBe(false)
      expect(refused.errorKind).toBe('managed')
    }
    expect(calls(sbx)).toEqual([])
  })

  it('only probes when the profile pins the daemon URL, or the daemon is not on loopback', async () => {
    const { sbx, supervisor } = await harness({ externallyOwned: true })
    writeFileSync(sbx.healthFile, JSON.stringify({ status: 'ok', version: ENGINE_VERSION, home: sbx.home, pid: 5 }))

    const status = await supervisor.ensure()
    expect(status.state).toBe('attached')
    expect(status.unsupervisedReason).toBe('pinned')
    expect(calls(sbx)).toEqual([])

    expect(isLoopbackUrl('http://127.0.0.1:7331')).toBe(true)
    expect(isLoopbackUrl('http://localhost:7331/')).toBe(true)
    expect(isLoopbackUrl('http://10.0.0.4:7331')).toBe(false)
  })

  it('disposes probes and log handles, and NEVER the daemon', async () => {
    const { sbx, supervisor } = await harness()
    await supervisor.ensure()
    const afterStart = calls(sbx)
    expect(afterStart.some(line => line.includes('start'))).toBe(true)

    supervisor.dispose()

    // No stop, no signal, nothing: the daemon outlives the plugin (D1).
    expect(calls(sbx)).toEqual(afterStart)
    expect(existsSync(sbx.healthFile)).toBe(true)
  })

  it('stops only on the explicit action, and reports notRunning honestly', async () => {
    const { sbx, supervisor } = await harness()
    await supervisor.ensure()

    const stopped = await supervisor.stop()
    expect(stopped.ok).toBe(true)
    expect(stopped.status.state).toBe('stopped')
    expect(existsSync(sbx.healthFile)).toBe(false)

    const again = await supervisor.stop()
    expect(again.ok).toBe(true)
    expect(calls(sbx).filter(line => line.includes('daemon stop'))).toHaveLength(2)
  })
})

describe('the daemon log tail', () => {
  it('reads the daemon’s own log file, and says so when there is none yet', async () => {
    const { sbx, supervisor } = await harness()

    const missing = await supervisor.log()
    expect(missing.ok).toBe(false)
    expect(missing.path).toBe(join(sbx.home, 'daemon.log'))
    expect(missing.error).toContain('no daemon log yet')

    writeFileSync(join(sbx.home, 'daemon.log'), ['one', 'two', 'three'].join('\n') + '\n')
    const tail = await supervisor.log(2)
    expect(tail.ok).toBe(true)
    expect(tail.lines).toEqual(['two', 'three'])
    expect(tailFile(join(sbx.home, 'daemon.log'), 10)).toEqual(['one', 'two', 'three'])
  })
})

describe('credential hygiene', () => {
  it('reads the web token exactly once, and never through the environment', () => {
    const sbx = sandbox()
    mkdirSync(join(sbx.home, 'secrets'), { recursive: true })
    writeFileSync(webTokenPath(sbx.home), 'deadbeefcafe\n')
    const before = { ...process.env }

    const bootstrap = createTokenBootstrap({ home: () => sbx.home })
    expect(bootstrap.read()).toBe('deadbeefcafe')
    expect(bootstrap.read()).toBe('deadbeefcafe')
    expect(bootstrap.read()).toBe('deadbeefcafe')

    expect(bootstrap.reads).toBe(1)
    expect(process.env).toEqual(before)
    expect(Object.values(process.env).some(value => (value ?? '').includes('deadbeefcafe'))).toBe(false)
  })

  it('reads nothing when the one file is absent, rather than hunting for another', () => {
    const sbx = sandbox()
    const opened: string[] = []
    const bootstrap = createTokenBootstrap({
      home: () => sbx.home,
      readTokenFile: path => {
        opened.push(path)
        return undefined
      },
    })

    expect(bootstrap.read()).toBeUndefined()
    expect(bootstrap.read()).toBeUndefined()
    expect(opened).toEqual([webTokenPath(sbx.home)])
  })

  it('asks the daemon for the enrollment credential over REST, once, and stores it', async () => {
    const posted: Array<{ url: string; authorization: string | undefined }> = []
    const stored: string[] = []
    const bootstrap = createEnrollmentBootstrap({
      daemonUrl: () => 'http://127.0.0.1:7331/',
      authorization: () => 'Bearer ccteam:deadbeef',
      persist: async bearer => {
        stored.push(bearer)
      },
      fetchImpl: async (url, init) => {
        posted.push({ url, authorization: (init?.headers as Record<string, string> | undefined)?.authorization })
        return new Response(JSON.stringify({ bearer: 'ccteam-enroll:e1:secret' }), {
          status: 201,
          headers: { 'content-type': 'application/json' },
        })
      },
    })

    expect(await bootstrap.ensure()).toBe('ccteam-enroll:e1:secret')
    expect(await bootstrap.ensure()).toBe('ccteam-enroll:e1:secret')

    expect(posted).toEqual([
      { url: 'http://127.0.0.1:7331/api/v1/enroll', authorization: 'Bearer ccteam:deadbeef' },
    ])
    expect(bootstrap.mints).toBe(1)
    expect(stored).toEqual(['ccteam-enroll:e1:secret'])
    expect(Object.values(process.env).some(value => (value ?? '').includes('ccteam-enroll:'))).toBe(false)
  })
})

describe('CLI verdict parsing', () => {
  it('takes the last JSON object on stdout, ignoring human text around it', () => {
    expect(lastJsonObject('banner\n{"status":"started","pid":7}\n')).toEqual({ status: 'started', pid: 7 })
    expect(lastJsonObject('{"status":"a"}\n{"status":"b"}\n')).toEqual({ status: 'b' })
    expect(lastJsonObject('not json at all\n')).toBeUndefined()
    expect(lastJsonObject('[1,2]\n')).toBeUndefined()
  })
})
