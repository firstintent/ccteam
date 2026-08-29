import { createRequire } from 'node:module'
import { readFileSync } from 'node:fs'
import { fileURLToPath } from 'node:url'
import { dirname, join } from 'node:path'
import { describe, expect, it } from 'vitest'
import { PACKAGE_VERSION } from '../src/index.js'

const root = join(dirname(fileURLToPath(import.meta.url)), '..')

/**
 * DSH's client-module scanner discovers a plugin by
 * `require.resolve('<pkg>/package.json')` and, on ANY resolve failure,
 * silently records it as "not a client package" (dsh-client-modules
 * `resolveMeta`, verified on 0.1.0-rc.7). An exports map that omits
 * "./package.json" therefore hides the panel from the web console with no
 * error anywhere — found on a real machine, invisible to every unit test
 * that reads package.json directly.
 */
describe('package exports keep the DSH scanner working', () => {
  const pkg = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8')) as {
    exports: Record<string, unknown>
    version: string
  }

  it('exports ./package.json alongside ./client', () => {
    expect(pkg.exports['./package.json']).toBe('./package.json')
    expect(pkg.exports['./client']).toBeDefined()
  })

  it('resolves <pkg>/package.json through the real Node resolver', () => {
    // The scanner's exact probe, using Node's package self-reference rule
    // (resolution from inside the package honors the same exports map the
    // profile-symlink resolution does): this throws
    // ERR_PACKAGE_PATH_NOT_EXPORTED when "./package.json" is missing.
    const requireFrom = createRequire(join(root, 'package.json'))
    const resolved = requireFrom.resolve('@ccteam/ccteam-ui/package.json')
    expect(resolved.endsWith('package.json')).toBe(true)
  })
})

/**
 * The version the transport reports in ACP `agentInfo.version` is what ccteam's
 * Rust floor check gates on (`MIN_DSH_CLIENT_VERSION`, dsh_acp/handshake.rs).
 * A constant that drifts from package.json fails the handshake with a version
 * message naming a version nobody shipped.
 */
describe('reported plugin version', () => {
  it('matches package.json', () => {
    expect(PACKAGE_VERSION).toBe(pkgVersion())
  })
})

function pkgVersion(): string {
  return (JSON.parse(readFileSync(join(root, 'package.json'), 'utf8')) as { version: string }).version
}
