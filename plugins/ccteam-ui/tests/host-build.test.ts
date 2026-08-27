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
import { CSS_MODULES_PATTERN, compileCss, createCssPlugins, styleInjectionModule } from '../build/css-plugins.js'

const root = join(dirname(fileURLToPath(import.meta.url)), '..')
const pkg = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8')) as {
  name: string
  main: string
  files: string[]
  exports: Record<string, { default: string } | string>
  dsh: { client: { platform: string; inject: string[] } }
}
const patch = readFileSync(join(root, 'cordis.patch.yml'), 'utf8')
const clientBundle = readFileSync(join(root, 'lib/client.js'), 'utf8')

/** A CSS identifier: what a class selector may start with (never a digit). */
const CSS_IDENT = /^-?[A-Za-z_][\w-]*$/

describe('client bundle loader contract', () => {
  it('emits both artifacts where package.json points', () => {
    expect(existsSync(join(root, 'lib/index.js'))).toBe(true)
    expect(existsSync(join(root, 'lib/client.js'))).toBe(true)
    expect(pkg.main).toBe('lib/index.js')
    expect((pkg.exports['./client'] as { default: string }).default).toBe('./lib/client.js')
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
    expect(exported.name).toBe('ccteam-ui')
    expect(typeof exported.apply).toBe('function')
    expect(exported.inject).toEqual(['slots', 'locale', 'settingsScope'])
    // Whatever the client half requires must be answerable by the module table.
    expect(required.length).toBeGreaterThan(0)
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
    expect(pkg.name).toBe('@ccteam/ccteam-ui')
    expect(patch).toContain(`name: '${pkg.name}'`)
    expect(patch).toContain('id: ccteam-ui')
    const banner = clientBundle.slice(0, clientBundle.indexOf('factory:'))
    expect(banner).toContain(JSON.stringify(pkg.name))
  })

  it('is CJS, not ESM — the loader evaluates it as a plain script', () => {
    const body = clientBundle.split('\n')
    expect(body.some(line => /^\s*import\s.+\sfrom\s/.test(line))).toBe(false)
    expect(body.some(line => /^\s*export\s+(default|const|function|\{)/.test(line))).toBe(false)
  })

  it('declares itself a web client plugin, names the packages it composes against, and ships the artifacts', () => {
    expect(pkg.dsh.client.platform).toBe('web')
    // The packages whose seats / services this plugin registers into — the
    // convention every DSH client package follows (ui-goal lists
    // ui-conversation, ui-settings-general lists ui-sidebar).
    expect(pkg.dsh.client.inject).toEqual([
      '@deepseek-ai/dsh-client-runtime',
      '@deepseek-ai/dsh-client-locale',
      '@deepseek-ai/dsh-client-ui-layout',
      '@deepseek-ai/dsh-client-ui-sidebar',
      '@deepseek-ai/dsh-client-ui-settings',
      '@deepseek-ai/dsh-client-ui-settings-plugins',
    ])
    expect(pkg.files).toContain('lib')
    expect(pkg.files).toContain('cordis.patch.yml')
    expect(pkg.files).toContain('README.md')
  })

  it('bundles the plugin name where DSH derives the display name from', () => {
    // DSH's plugin list shows `moduleShortName(name)`: the scope is dropped,
    // then a `dsh-` / `dsh-host-` / `dsh-client-` prefix. `@ccteam/dsh-team`
    // therefore displayed as "team"; `ccteam-` is not a stripped prefix, so
    // this name displays whole.
    const unscoped = pkg.name.slice(pkg.name.indexOf('/') + 1)
    expect(unscoped.replace(/^dsh-(?:host-|client-)?/, '')).toBe('ccteam-ui')
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

  it('rejects a cross-plugin @deepseek-ai value import at build time', () => {
    expect(() => assertBundlePurity('@deepseek-ai/dsh-client-locale')).toThrow(/bundle purity/)
    expect(() => assertBundlePurity('@deepseek-ai/dsh-client-ui-sidebar/client')).toThrow(/bundle purity/)
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

  it('inlines clsx into the built bundle rather than requiring it', () => {
    expect(clientBundle).not.toMatch(/require\(["']clsx["']\)/)
  })
})

describe('CSS modules (lightningcss, the DSH preset pipeline)', () => {
  const sheet = '.panel { color: var(--dsw-alias-label-primary); }\n.row:hover { color: var(--dsw-alias-label-secondary); }\n@media (min-width: 0.5px) { .row { gap: 0.5em; } }\n.rowActive { composes: row; opacity: 0.5; }'

  it('uses the preset pattern and hashes every class into a valid identifier', () => {
    expect(CSS_MODULES_PATTERN).toBe('[hash]_[local]')
    const { css, classMap } = compileCss('@ccteam/ccteam-ui', '/x/panel.module.css', sheet, 'module')
    expect(Object.keys(classMap).sort()).toEqual(['panel', 'row', 'rowActive'])
    for (const [local, hashed] of Object.entries(classMap)) {
      for (const token of hashed.split(' ')) {
        expect(token, `${local} → ${token}`).toMatch(CSS_IDENT)
        expect(token.endsWith(`_${local}`) || token.endsWith('_row'), `${local} → ${token}`).toBe(true)
        expect(css).toContain(`.${token}`)
      }
    }
    // composes: the class list carries the composed class too.
    expect(classMap.rowActive!.split(' ')).toHaveLength(2)
    expect(classMap.rowActive!.split(' ')[1]).toBe(classMap.row)
    expect(css).not.toMatch(/\.panel\s*\{/)
    // Minified by lightningcss; decimal literals survive untouched.
    expect(css).toContain('.5em')
    expect(css).not.toContain('\n')
  })

  /**
   * THE regression this pipeline exists for: a hashed class whose hash starts
   * with a digit is not a selector, so the browser drops every rule under it
   * and the panel renders as bare DOM (v0.10.4, `.9a3484fd_entry`). Whatever
   * the hash input, the emitted class must start with a letter or underscore.
   */
  it('never emits a class selector that starts with a digit, whatever the file name', () => {
    for (let index = 0; index < 64; index += 1) {
      const { css, classMap } = compileCss('@ccteam/ccteam-ui', `/pkg/src/client/sheet-${index}.module.css`, '.entry { color: inherit; }', 'module')
      expect(classMap.entry, `file ${index}`).toMatch(CSS_IDENT)
      expect(css).toContain(`.${classMap.entry}{`)
    }
  })

  it('hashes by package-relative path when a root is given, so the tarball is checkout-independent', () => {
    const a = compileCss('@ccteam/ccteam-ui', '/home/alice/ccteam-ui/src/client/panel.module.css', sheet, 'module', '/home/alice/ccteam-ui')
    const b = compileCss('@ccteam/ccteam-ui', '/srv/build/ccteam-ui/src/client/panel.module.css', sheet, 'module', '/srv/build/ccteam-ui')
    const c = compileCss('@ccteam/ccteam-ui', '/srv/build/ccteam-ui/src/client/other.module.css', sheet, 'module', '/srv/build/ccteam-ui')
    expect(a.classMap).toEqual(b.classMap)
    expect(a.classMap.panel).not.toBe(c.classMap.panel)
  })

  it('injects one tagged style at factory execution, guarded against duplicates', () => {
    const { code } = compileCss('@ccteam/ccteam-ui', '/x/panel.module.css', sheet, 'module')
    expect(code).toContain("document.createElement('style')")
    expect(code).toContain('document.head.appendChild(tag)')
    // data-plugin lets the loader's claimStyles() bookkeeping own the tag.
    expect(code).toContain('tag.dataset.plugin = "@ccteam/ccteam-ui"')
    expect(code).toContain('tag.dataset.pluginCss = tagId')
    expect(code).toContain('"@ccteam/ccteam-ui/panel.module.css"')
    // Re-materialization after an HMR invalidate must not stack styles.
    expect(code).toContain("document.querySelector('style[data-plugin-css='")
    expect(code).toContain('=== null')
    expect(code).toContain('export default {"panel"')
    // Byte-for-byte the preset's injector shape.
    expect(code).toBe(styleInjectionModule('@ccteam/ccteam-ui', '/x/panel.module.css', compileCss('@ccteam/ccteam-ui', '/x/panel.module.css', sheet, 'module').css, compileCss('@ccteam/ccteam-ui', '/x/panel.module.css', sheet, 'module').classMap))
  })

  it('routes each stylesheet flavour through its own virtual id', async () => {
    const reads: string[] = []
    const watched: string[] = []
    const plugins = createCssPlugins('@ccteam/ccteam-ui', async (file) => {
      reads.push(file)
      return '.panel { color: inherit; }'
    })
    const [modules, inline, global] = plugins
    const ctx = { addWatchFile: (id: string) => watched.push(id) }

    expect(plugins.map(plugin => plugin.name)).toEqual([
      'ccteam-css-modules-inline',
      'ccteam-css-text-inline',
      'ccteam-css-global-inline',
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

    const moduleCode = await modules!.load.call(ctx, '\0ccteam-css-module:/pkg/src/client/panel.module.css.mjs')
    expect(moduleCode).toContain('export default {"panel"')
    expect(reads).toEqual(['/pkg/src/client/panel.module.css'])
    // Without an explicit watch the virtual id hides the sheet from --watch.
    expect(watched).toEqual(['/pkg/src/client/panel.module.css'])

    const inlineCode = await inline!.load.call(ctx, '\0ccteam-css-inline:/pkg/src/client/base.css.mjs')
    expect(inlineCode).toBe('export default ".panel{color:inherit}";')

    // A route ignores an id belonging to another route.
    expect(await modules!.load.call(ctx, '\0ccteam-css-global:/pkg/x.css.mjs')).toBeNull()
  })

  it('emits a global stylesheet with no class map', () => {
    const { code, classMap } = compileCss('@ccteam/ccteam-ui', '/x/base.css', '.a { color: inherit; }', 'global')
    expect(classMap).toEqual({})
    expect(code).toContain('export {};')
    expect(code).not.toContain('export default')
    // A global sheet keeps its author-written selectors (minified).
    expect(code).toContain('.a{color:inherit}')
  })

  it('ships identifier-safe hashed selectors in the built bundle', () => {
    // The built artifact, not a fixture: every hashed selector of the panel
    // sheet must be parseable, and the class map must agree with it.
    const selectors = [...clientBundle.matchAll(/\.([\w-]+)_(entry|panel|card)\{/g)]
    expect(selectors.length).toBeGreaterThan(0)
    for (const [, hash] of selectors) expect(`${hash}_x`).toMatch(CSS_IDENT)
    const mapped = clientBundle.match(/["']?entry["']?:\s*"([\w-]+)"/)
    expect(mapped, 'the entry class is exported from the class map').not.toBeNull()
    expect(clientBundle).toContain(`.${mapped![1]}{`)
  })
})
