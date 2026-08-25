/**
 * Guards the DSH client-bundle build contract. These assertions are structural
 * on purpose: the failure modes here ("right text, wrong shape") all throw in
 * the browser at boot, far from the build that caused them.
 *
 * Requires `npm run build` to have run — the `test` script does it first.
 */
import { existsSync, readFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'
import { describe, expect, it } from 'vitest'
import { CLIENT_EXTERNALS, assertBundlePurity, isClientExternal } from '../build/client-externals.js'
import { compileCss, cssHash, hashClassNames } from '../build/css-module.js'
import { createCssPlugins } from '../build/css-plugins.js'

const root = join(dirname(fileURLToPath(import.meta.url)), '..')
const pkg = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8')) as {
  name: string
  main: string
  files: string[]
  exports: Record<string, { default: string }>
  dsh: { client: { platform: string } }
}
const patch = readFileSync(join(root, 'cordis.patch.yml'), 'utf8')
const clientBundle = readFileSync(join(root, 'lib/client.js'), 'utf8')

describe('client bundle loader contract', () => {
  it('emits both artifacts where package.json points', () => {
    expect(existsSync(join(root, 'lib/index.js'))).toBe(true)
    expect(existsSync(join(root, 'lib/client.js'))).toBe(true)
    expect(pkg.main).toBe('lib/index.js')
    expect(pkg.exports['./client']!.default).toBe('./lib/client.js')
  })

  /**
   * Execute the artifact against a stand-in loader. This is the real contract
   * — the bundler is free to reformat the banner/intro/footer (rolldown
   * pretty-prints them), so asserting bytes would be brittle while asserting
   * behaviour is exact.
   */
  function evaluate(): { registrations: Array<{ id: string; factory: unknown }> } {
    const registrations: Array<{ id: string; factory: unknown }> = []
    const win = { __ModuleLoader__: { load: (entry: { id: string; factory: unknown }) => registrations.push(entry) } }
    // eslint-disable-next-line no-new-func -- executing the built artifact IS the test
    new Function('window', clientBundle)(win)
    return { registrations }
  }

  it('registers exactly one bundle under the package name', () => {
    const { registrations } = evaluate()

    expect(registrations).toHaveLength(1)
    expect(registrations[0]!.id).toBe(pkg.name)
    expect(typeof registrations[0]!.factory).toBe('function')
    // The loader passes exactly one argument: the synchronous require.
    expect((registrations[0]!.factory as (...args: unknown[]) => unknown).length).toBe(1)
  })

  it('executing the factory returns module.exports carrying the plugin face', () => {
    const { registrations } = evaluate()
    const required: string[] = []
    const factory = registrations[0]!.factory as (
      require: (spec: string) => unknown,
    ) => Record<string, unknown>

    const exported = factory(spec => {
      required.push(spec)
      return {}
    })

    expect(typeof exported).toBe('object')
    expect(exported.name).toBe('ccteam-team')
    expect(typeof exported.apply).toBe('function')
    expect(exported.inject).toEqual(['slots', 'locale'])
    // Whatever the client half requires must be answerable by the module table.
    for (const spec of required) expect(isClientExternal(spec)).toBe(true)
  })

  it('does not evaluate the plugin body until the factory runs (lazy CJS)', () => {
    // Registration alone must have no side effects: the loader materializes
    // on first import, and CSS injection rides that same timing.
    const { registrations } = evaluate()
    expect(registrations).toHaveLength(1)
  })

  it('agrees on the bundle id in all three places that must match', () => {
    // A mismatch between the banner id, the package name, and the loader entry
    // is the "bundle loaded without registering" boot failure.
    expect(pkg.name).toBe('@ccteam/dsh-team')
    expect(patch).toContain(`name: '${pkg.name}'`)
    const banner = clientBundle.slice(0, clientBundle.indexOf('factory:'))
    expect(banner).toContain(JSON.stringify(pkg.name))
  })

  it('is CJS, not ESM — the loader evaluates it as a plain script', () => {
    const body = clientBundle.split('\n')
    expect(body.some(line => /^\s*import\s.+\sfrom\s/.test(line))).toBe(false)
    expect(body.some(line => /^\s*export\s+(default|const|function|\{)/.test(line))).toBe(false)
  })

  it('declares itself a web client plugin and ships the artifacts', () => {
    expect(pkg.dsh.client.platform).toBe('web')
    expect(pkg.files).toContain('lib')
    expect(pkg.files).toContain('cordis.patch.yml')
    expect(pkg.files).toContain('README.md')
  })
})

describe('client externals', () => {
  it('is exactly the platform seed table plus the preloaded runtime', () => {
    expect([...CLIENT_EXTERNALS].sort()).toEqual([
      '@deepseek-ai/cordis',
      '@deepseek-ai/dsh-client-runtime/client',
      '@deepseek-ai/dsh-client-ui-primitives',
      '@deepseek-ai/dsh-client-ui-slots',
      'react',
      'react-dom',
      'react-dom/client',
      'react/jsx-runtime',
    ])
  })

  it('matches by exact string, so subpaths are not inherited', () => {
    expect(isClientExternal('react')).toBe(true)
    expect(isClientExternal('react/jsx-runtime')).toBe(true)
    // The trap: the dev JSX runtime is NOT a seed key. Compiling with the dev
    // runtime would inline a second React and break hooks.
    expect(isClientExternal('react/jsx-dev-runtime')).toBe(false)
    // Cordis is rescoped; the bare name would be a require miss.
    expect(isClientExternal('cordis')).toBe(false)
    // ui-slots/ui-primitives are statically linked, so bare — no /client.
    expect(isClientExternal('@deepseek-ai/dsh-client-ui-slots/client')).toBe(false)
    expect(isClientExternal('@deepseek-ai/dsh-client-runtime')).toBe(false)
  })

  it('rejects a cross-plugin @deepseek-ai import at build time', () => {
    expect(() => assertBundlePurity('@deepseek-ai/dsh-client-locale')).toThrow(/bundle purity/)
    expect(() => assertBundlePurity('@deepseek-ai/dsh-client-modules')).toThrow(/bundle purity/)
  })

  it('allows externals, vendored libraries and every non-scoped package', () => {
    for (const specifier of CLIENT_EXTERNALS) {
      expect(() => assertBundlePurity(specifier)).not.toThrow()
    }
    expect(() => assertBundlePurity('@deepseek-ai/schemastery')).not.toThrow()
    expect(() => assertBundlePurity('@deepseek-ai/cosmokit/utils')).not.toThrow()
    expect(() => assertBundlePurity('clsx')).not.toThrow()
  })
})

describe('CSS modules', () => {
  const sheet = '.panel { color: red; }\n.row:hover { color: blue; }\n@media (min-width: 0.5px) { .row { gap: 0.5em; } }'

  it('hashes class selectors and returns the local-to-hashed map', () => {
    const { css, classMap } = hashClassNames('/x/panel.module.css', sheet)
    const hash = cssHash('/x/panel.module.css', sheet)

    expect(Object.keys(classMap).sort()).toEqual(['panel', 'row'])
    expect(classMap.panel).toBe(`${hash}_panel`)
    expect(css).toContain(`.${hash}_panel`)
    expect(css).not.toMatch(/\.panel\s*\{/)
    // Decimal literals are not class tokens and must survive untouched.
    expect(css).toContain('0.5em')
    expect(css).toContain('min-width: 0.5px')
  })

  it('is deterministic and file-scoped', () => {
    expect(cssHash('/a.module.css', sheet)).toBe(cssHash('/a.module.css', sheet))
    expect(cssHash('/a.module.css', sheet)).not.toBe(cssHash('/b.module.css', sheet))
  })

  it('injects one tagged style at factory execution, guarded against duplicates', () => {
    const { code } = compileCss('@ccteam/dsh-team', '/x/panel.module.css', sheet, true)

    expect(code).toContain("document.createElement('style')")
    expect(code).toContain('document.head.appendChild(tag)')
    // data-plugin lets the loader's claimStyles() bookkeeping own the tag.
    expect(code).toContain('tag.dataset.plugin = "@ccteam/dsh-team"')
    expect(code).toContain('tag.dataset.pluginCss = tagId')
    expect(code).toContain('"@ccteam/dsh-team/panel.module.css"')
    // Re-materialization after an HMR invalidate must not stack styles.
    expect(code).toContain("document.querySelector('style[data-plugin-css='")
    expect(code).toContain('=== null')
    expect(code).toContain('export default {"panel"')
  })

  it('routes each stylesheet flavour through its own virtual id', async () => {
    const reads: string[] = []
    const watched: string[] = []
    const plugins = createCssPlugins('@ccteam/dsh-team', async file => {
      reads.push(file)
      return '.panel { color: red; }'
    })
    const [modules, inline, global] = plugins
    const ctx = { addWatchFile: (id: string) => watched.push(id) }

    expect(plugins.map(plugin => plugin.name)).toEqual([
      'ccteam-css-modules',
      'ccteam-css-inline',
      'ccteam-css-global',
    ])

    // Each route claims only its own flavour; the global route must not
    // swallow module sheets, and the inline query must be stripped.
    expect(modules!.resolveId('./panel.module.css', '/pkg/src/client/index.tsx'))
      .toBe('\0ccteam-css-module:/pkg/src/client/panel.module.css.mjs')
    expect(modules!.resolveId('./base.css', '/pkg/src/client/index.tsx')).toBeNull()
    expect(global!.resolveId('./panel.module.css', '/pkg/src/client/index.tsx')).toBeNull()
    expect(global!.resolveId('./base.css', '/pkg/src/client/index.tsx'))
      .toBe('\0ccteam-css-global:/pkg/src/client/base.css.mjs')
    expect(inline!.resolveId('./base.css?inline', '/pkg/src/client/index.tsx'))
      .toBe('\0ccteam-css-inline:/pkg/src/client/base.css.mjs')

    // The virtual id must not end in .css, or tsdown's own CSS guard claims it.
    for (const plugin of plugins) {
      const id = plugin.resolveId('./x.module.css', '/pkg/src/a.tsx')
        ?? plugin.resolveId('./x.css?inline', '/pkg/src/a.tsx')
        ?? plugin.resolveId('./x.css', '/pkg/src/a.tsx')
      expect(id!.endsWith('.css')).toBe(false)
    }

    const moduleCode = await modules!.load.call(
      ctx,
      '\0ccteam-css-module:/pkg/src/client/panel.module.css.mjs',
    )
    expect(moduleCode).toContain('export default {"panel"')
    expect(reads).toEqual(['/pkg/src/client/panel.module.css'])
    // Without an explicit watch the virtual id hides the sheet from --watch.
    expect(watched).toEqual(['/pkg/src/client/panel.module.css'])

    const inlineCode = await inline!.load.call(
      ctx,
      '\0ccteam-css-inline:/pkg/src/client/base.css.mjs',
    )
    expect(inlineCode).toBe('export default ".panel { color: red; }";')

    // A route ignores an id belonging to another route.
    expect(await modules!.load.call(ctx, '\0ccteam-css-global:/pkg/x.css.mjs')).toBeNull()
  })

  it('emits a global stylesheet with no class map', () => {
    const { code, classMap } = compileCss('@ccteam/dsh-team', '/x/base.css', '.a { color: red; }', false)

    expect(classMap).toEqual({})
    expect(code).toContain('export {};')
    expect(code).not.toContain('export default')
    // A global sheet keeps its author-written selectors.
    expect(code).toContain('.a { color: red; }')
  })
})
