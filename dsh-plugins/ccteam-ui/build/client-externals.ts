/**
 * The DSH client-bundle module table — build-time only, never shipped.
 *
 * Verbatim from references/deepseek-harness/packages/client/web/src/platform.ts
 * (`PLATFORM_MODULES` lines 8-12 + `PRELOADED_CLIENT_EXTERNALS` lines 15-17).
 * These are the only specifiers the browser loader's injected `require` can
 * answer; everything else must be bundled inline or it throws at
 * materialization (system.ts:174-187).
 *
 * MATCHING IS EXACT STRING EQUALITY, never prefix. `react` being external does
 * not make `react/jsx-runtime` external — it is listed separately because it
 * is a separate seed key. The practical trap: `react/jsx-dev-runtime` is NOT a
 * seed key, so the client must be compiled with the production JSX runtime.
 */
export const CLIENT_EXTERNALS: ReadonlySet<string> = new Set([
  'react',
  'react/jsx-runtime',
  'react-dom',
  'react-dom/client',
  // Cordis is vendored and rescoped repo-wide; bare `cordis` is a require miss.
  '@deepseek-ai/cordis',
  '@deepseek-ai/dsh-client-ui-slots',
  '@deepseek-ai/dsh-client-ui-primitives',
  // The `/client` subpath is part of the specifier; the loader strips it to key the row.
  '@deepseek-ai/dsh-client-runtime/client',
])

/** Vendored libraries carry no shared identity, so inlining them is correct. */
const VENDORED_LIBRARY = /^@deepseek-ai\/(cosmokit|schemastery)(\/|$)/

export function isClientExternal(source: string): boolean {
  return CLIENT_EXTERNALS.has(source)
}

/**
 * Mirror of the in-tree bundle-purity gate (tsdown.client.ts:479-497): a
 * `@deepseek-ai/*` specifier that is neither a module-table row nor a vendored
 * library would compile to a `require()` the loader cannot answer. Failing the
 * build is cheaper than failing in the browser.
 *
 * @throws when `source` is a forbidden cross-plugin value import.
 */
export function assertBundlePurity(source: string): void {
  if (!source.startsWith('@deepseek-ai/')) return
  if (isClientExternal(source)) return
  if (VENDORED_LIBRARY.test(source)) return
  throw new Error(
    `client bundle purity: "${source}" is not one of the DSH client externals — `
    + 'cross-plugin value imports are forbidden; collaborate through cordis services '
    + '(type-only imports are erased and never reach this gate)',
  )
}
