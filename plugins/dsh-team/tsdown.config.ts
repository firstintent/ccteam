/**
 * Two artifacts from one build, both into `lib/`:
 *
 *   src/index.ts        → lib/index.js   plain ESM, loaded by Cordis in node
 *   src/client/index.tsx → lib/client.js  DSH closure-factory bundle
 *
 * The client half is NOT an ordinary bundle. DSH's browser module loader reads
 * `lib/client.js` verbatim (it never rebuilds it) and expects the file to call
 * `window.__ModuleLoader__.load({id, factory})`, where `factory(require)` is a
 * synchronous CJS shim returning `module.exports`. That shape is produced by
 * emitting CJS and wrapping it in banner/intro/footer — see
 * references/deepseek-harness/packages/client/tsdown.client.ts:437-567 and
 * packages/client/modules/src/client/{manifest,system}.ts.
 *
 * Two rules are load-bearing; getting either wrong throws at browser boot:
 *   1. `id` must be byte-identical to package.json `name` and to the `name:`
 *      in cordis.patch.yml, else system.ts:120 raises "bundle loaded without
 *      registering".
 *   2. Externals are matched by EXACT STRING, never by prefix. `react` being
 *      external does not make `react/jsx-runtime` external — it is listed
 *      separately because it is a separate seed key. Anything not on the list
 *      must inline, because the loader's `require` can only answer seed words
 *      and registered rows (system.ts:174-187).
 */
import { defineConfig } from 'tsdown'
// Extension-bearing specifier: tsdown loads this config through Node's native
// TypeScript support, which does not remap `.js` onto a `.ts` source.
import { assertBundlePurity, CLIENT_EXTERNALS } from './build/client-externals.ts'
import { createCssPlugins } from './build/css-plugins.ts'

/** Must equal package.json `name` and the cordis.patch.yml `name:` row. */
const PLUGIN_ID = '@ccteam/dsh-team'

/**
 * Reject cross-plugin `@deepseek-ai/*` value imports, mirroring the in-tree
 * purity gate (tsdown.client.ts:479-497). A specifier that is neither external
 * nor inline-safe would become a `require()` the loader table cannot answer —
 * a guaranteed runtime throw. Failing the build is the cheaper error.
 */
const purityGate = {
  name: 'ccteam-client-bundle-purity',
  resolveId(source: string): null {
    assertBundlePurity(source)
    return null
  },
}

/**
 * CSS routes: `.module.css` yields a hashed class map plus a tagged style
 * injected when the factory runs, `.css?inline` yields the text, `.css`
 * injects only. Compiled by build/css-module.ts rather than lightningcss;
 * the loader only requires that a <style> exist after materialization, so
 * the gap is class-name spelling and minification, not behaviour.
 */
const cssPlugins = createCssPlugins(PLUGIN_ID)

export default defineConfig([
  {
    name: PLUGIN_ID,
    entry: { index: 'src/index.ts' },
    outDir: 'lib',
    format: 'esm',
    platform: 'node',
    target: 'es2024',
    dts: false,
    sourcemap: false,
    // Both configs write into lib/; a default clean would wipe the sibling.
    clean: false,
    deps: { neverBundle: [/^node:/, '@deepseek-ai/schemastery'] },
    // package.json `main` is lib/index.js; the ESM default would emit .mjs.
    outputOptions: { entryFileNames: 'index.js' },
  },
  {
    name: `${PLUGIN_ID}/client`,
    entry: { client: 'src/client/index.tsx' },
    outDir: 'lib',
    format: 'cjs',
    platform: 'browser',
    target: 'es2024',
    // A .d.cts would swallow the banner/footer and fail to parse.
    dts: false,
    sourcemap: true,
    clean: false,
    // Exact-match externals only: everything else inlines, because a require()
    // the loader table cannot answer throws at materialization.
    deps: {
      neverBundle: (source: string) => CLIENT_EXTERNALS.has(source),
      alwaysBundle: (source: string) => !CLIENT_EXTERNALS.has(source),
    },
    define: {
      'process.env': '{}',
      'process.env.NODE_ENV': JSON.stringify(process.env.NODE_ENV ?? 'production'),
      'import.meta.env.MODE': JSON.stringify(process.env.NODE_ENV ?? 'production'),
      'import.meta.env': JSON.stringify({ MODE: process.env.NODE_ENV ?? 'production' }),
    },
    plugins: [purityGate, ...cssPlugins],
    outputOptions: {
      entryFileNames: 'client.js',
      banner: `window.__ModuleLoader__.load({ id: ${JSON.stringify(PLUGIN_ID)}, factory: (require) => {`,
      footer: 'return module.exports; } });',
      intro: 'var module = { exports: {} }; var exports = module.exports;',
    },
  },
])
