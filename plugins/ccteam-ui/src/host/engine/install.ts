/**
 * Getting the engine onto disk from the npm platform package.
 *
 * The binary rides `optionalDependencies` on `@ccteam/engine-<os>-<cpu>`
 * (esbuild's shape: `os`/`cpu` fields, no lifecycle script), because a DSH
 * profile is a pnpm workspace and pnpm 10 blocks postinstall by default.
 * `dsh plugin add @ccteam/ccteam-ui` therefore downloads the right engine for
 * this machine and nothing else — but it downloads it into `node_modules`,
 * where PATH cannot see it and where a `pnpm prune` can take it away.
 *
 * So the plugin COPIES it to the one location install.sh uses, and follows the
 * same existing-install-aware ladder the Rust side follows
 * (`update.rs::{resolve_install_dir, classify_destination}`): a symlink or a
 * package-manager-owned file is REPORTED, never overwritten. Whatever owns
 * such a path will put its own file back, and clobbering it leaves two rival
 * ccteam binaries with no record of which one PATH resolves.
 *
 * The copy is verified before it is published: the new bytes answer
 * `--version` from a temporary name, and only then does one `rename` swap them
 * in. A reader never sees a half-written binary, and a bad package can never
 * leave a broken `ccteam` behind.
 */
import { chmodSync, copyFileSync, existsSync, lstatSync, mkdirSync, readlinkSync, rmSync, renameSync, statSync, writeFileSync } from 'node:fs'
import { createRequire } from 'node:module'
import { join, sep } from 'node:path'
import {
  canonicalInstallDir,
  binaryVersion,
  enginePackageName,
  isExecutableFile,
  parentDir,
  runCommand,
  type EngineEnvironment,
  type EnginePlatform,
  type RunFn,
} from './locate.js'

/** What the install destination already holds. Only `writable` may be touched. */
export type DestVerdict =
  | { kind: 'writable' }
  | { kind: 'symlink'; target: string }
  | { kind: 'packageManaged'; owner: string }

/**
 * Pure path classification — every rule testable without building the tree.
 * Mirrors `update.rs::classify_dest_path`.
 */
export function classifyDestPath(dest: string): string | undefined {
  const parts = dest.split(sep)
  if (parts.includes('node_modules') || parts.includes('.pnpm')) return 'a node package tree (node_modules)'
  if (dest.startsWith('/nix/store')) return 'nix'
  if (parts.includes('Cellar')) return 'homebrew'
  if (dest.startsWith('/snap/')) return 'snap'
  return undefined
}

/** Path rules first (they apply whether or not the file exists), then the fs probe. */
export function classifyDestination(dest: string): DestVerdict {
  const owner = classifyDestPath(dest)
  if (owner !== undefined) return { kind: 'packageManaged', owner }
  try {
    if (lstatSync(dest).isSymbolicLink()) {
      let target: string
      try {
        target = readlinkSync(dest)
      } catch {
        target = '<unreadable>'
      }
      return { kind: 'symlink', target }
    }
  } catch {
    // absent — free to install
  }
  return { kind: 'writable' }
}

/** `<…>/target/{debug,release}` is a build output, not an install location. */
function isCargoBuildTree(dir: string): boolean {
  const parts = dir.split(sep)
  const last = parts.at(-1)
  return (last === 'debug' || last === 'release') && parts.at(-2) === 'target'
}

function isWritableDir(dir: string): boolean {
  try {
    if (!statSync(dir).isDirectory()) return false
  } catch {
    return false
  }
  const probe = join(dir, '.ccteam-write-probe')
  try {
    writeFileSync(probe, '')
    return true
  } catch {
    return false
  } finally {
    try {
      rmSync(probe, { force: true })
    } catch {
      // best effort
    }
  }
}

/**
 * install.sh's ladder: explicit override → wherever ccteam already lives
 * (writable, not a cargo build tree, not package-owned) → `$HOME/.local/bin`.
 * The middle rung is why a CLI user who installed to `/usr/local/bin` does not
 * get a second copy in `~/.local/bin` the first time they add the plugin.
 */
export function resolveInstallDir(environment: EngineEnvironment, currentBinary?: string): string {
  const pinned = (environment.env.CCTEAM_INSTALL_DIR ?? '').trim()
  if (pinned !== '') return pinned
  if (currentBinary !== undefined && currentBinary !== '') {
    const dir = parentDir(currentBinary)
    if (dir !== '' && !isCargoBuildTree(dir) && classifyDestPath(dir) === undefined && isWritableDir(dir)) {
      return dir
    }
  }
  return canonicalInstallDir(environment)
}

export type ResolvePackageBin = (platform: EnginePlatform) => string | undefined

const requireFromHere = createRequire(import.meta.url)

/**
 * The engine inside the platform package this plugin was installed with.
 *
 * `require.resolve` is the primary because it is the only rule that follows
 * the SAME node_modules tree the runtime loaded this plugin from — including
 * pnpm's `.pnpm/@ccteam+ccteam-ui@<v>/node_modules/@ccteam/engine-*` layout,
 * which no hand-written path walk gets right. The walk below is a fallback for
 * trees where the platform package is present but unresolvable (a hoisted
 * install with the plugin loaded from an absolute path).
 */
export const resolvePackageBin: ResolvePackageBin = platform => {
  const pkg = enginePackageName(platform)
  try {
    return requireFromHere.resolve(`${pkg}/bin/ccteam`)
  } catch {
    // fall through to the walk
  }
  let dir = parentDir(new URL(import.meta.url).pathname)
  for (let depth = 0; depth < 8; depth += 1) {
    const candidate = join(dir, 'node_modules', ...pkg.split('/'), 'bin', 'ccteam')
    if (existsSync(candidate)) return candidate
    const up = parentDir(dir)
    if (up === dir) break
    dir = up
  }
  return undefined
}

export type InstallOutcome =
  | { ok: true; binary: string; version: string; from: string }
  | { ok: false; errorKind: string; error: string }

export interface InstallOptions {
  /** The engine to copy — normally the platform package's `bin/ccteam`. */
  source: string
  /** Absolute destination (`<install dir>/ccteam`). */
  dest: string
  run?: RunFn
}

/**
 * Copy → chmod → verify → rename. Steps in that order, so a failure at any
 * point leaves the previous engine exactly as it was.
 */
export async function installEngine(options: InstallOptions): Promise<InstallOutcome> {
  const { source, dest } = options
  const run = options.run ?? runCommand
  if (!existsSync(source)) {
    return {
      ok: false,
      errorKind: 'packageMissing',
      error:
        `the engine package is not installed next to the plugin (${source}). ` +
        'Reinstall the plugin so its platform package comes with it, or install ccteam yourself.',
    }
  }
  const verdict = classifyDestination(dest)
  if (verdict.kind === 'symlink') {
    return {
      ok: false,
      errorKind: 'destIsSymlink',
      error:
        `${dest} is a symlink to ${verdict.target} — refusing to replace it. Whatever owns ` +
        'that link would put it back, leaving two ccteam binaries. Replace the link yourself, ' +
        'or install elsewhere with CCTEAM_INSTALL_DIR=<dir>.',
    }
  }
  if (verdict.kind === 'packageManaged') {
    return {
      ok: false,
      errorKind: 'destPackageManaged',
      error:
        `${dest} is owned by ${verdict.owner} — refusing to write into it. Update it with that ` +
        'tool, or install elsewhere with CCTEAM_INSTALL_DIR=<dir>.',
    }
  }

  const dir = parentDir(dest)
  try {
    mkdirSync(dir, { recursive: true })
  } catch (error) {
    return { ok: false, errorKind: 'installDirUnwritable', error: `could not create ${dir}: ${describe(error)}` }
  }

  const staged = `${dest}.ccteam-new-${process.pid}`
  try {
    copyFileSync(source, staged)
    chmodSync(staged, 0o755)
  } catch (error) {
    cleanup(staged)
    return { ok: false, errorKind: 'installFailed', error: `could not stage ${source} into ${dir}: ${describe(error)}` }
  }
  if (!isExecutableFile(staged)) {
    cleanup(staged)
    return { ok: false, errorKind: 'binaryNotExecutable', error: `${source} could not be made executable` }
  }
  const version = await binaryVersion(staged, run)
  if (version === undefined) {
    cleanup(staged)
    return {
      ok: false,
      errorKind: 'binaryVersionUnreadable',
      error: `${source} did not answer \`--version\` with a parseable version; it is not a usable ccteam binary`,
    }
  }
  try {
    renameSync(staged, dest)
  } catch (error) {
    cleanup(staged)
    return { ok: false, errorKind: 'installFailed', error: `could not install into ${dest}: ${describe(error)}` }
  }
  return { ok: true, binary: dest, version, from: source }
}

function cleanup(path: string): void {
  try {
    rmSync(path, { force: true })
  } catch {
    // best effort
  }
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error)
}
