/**
 * Rolldown plugins routing stylesheet imports into JavaScript, build-time only.
 *
 * Three routes, mirroring the in-tree preset
 * (references/deepseek-harness/packages/client/tsdown.client.ts:498-553):
 *   `*.module.css`  → hashed class map (default export) + style injection
 *   `*.css?inline`  → the stylesheet text as a default export, no injection
 *   `*.css`         → style injection only, `export {}`
 *
 * Each real stylesheet is rewritten to a virtual id. The `.mjs` suffix is not
 * decorative: tsdown's built-in CSS guard matches ids ending in `.css` and
 * demands the @tsdown/css package, so the virtual id must not end that way.
 */
import { readFile } from 'node:fs/promises'
import { dirname, resolve } from 'node:path'
import { compileCss } from './css-module.ts'

const MODULE_PREFIX = '\0ccteam-css-module:'
const GLOBAL_PREFIX = '\0ccteam-css-global:'
const INLINE_PREFIX = '\0ccteam-css-inline:'
const VIRTUAL_SUFFIX = '.mjs'
const INLINE_QUERY = '?inline'

/** The rolldown plugin `this` surface these routes rely on. */
export interface LoadContext {
  addWatchFile(id: string): void
}

export interface CssPlugin {
  name: string
  resolveId(source: string, importer: string | undefined): string | null
  load(this: LoadContext, id: string): Promise<string | null>
}

function absoluteFrom(source: string, importer: string | undefined): string {
  return importer === undefined ? source : resolve(dirname(importer), source)
}

/**
 * Build the CSS route plugins for one package.
 *
 * @param pluginId - npm package name, stamped onto injected style tags so the
 *   loader's `claimStyles` bookkeeping sees an owned tag.
 * @param read - injected for tests; defaults to reading the real file.
 */
export function createCssPlugins(
  pluginId: string,
  read: (file: string) => Promise<string> = file => readFile(file, 'utf8'),
): CssPlugin[] {
  const route = (
    name: string,
    prefix: string,
    matches: (source: string) => boolean,
    strip: (source: string) => string,
    render: (fileId: string, css: string) => string,
  ): CssPlugin => ({
    name,
    resolveId(source, importer) {
      if (!matches(source)) return null
      return prefix + absoluteFrom(strip(source), importer) + VIRTUAL_SUFFIX
    },
    async load(id) {
      if (!id.startsWith(prefix)) return null
      const fileId = id.slice(prefix.length, -VIRTUAL_SUFFIX.length)
      // The virtual id otherwise hides the real stylesheet from the watch graph.
      this.addWatchFile(fileId)
      return render(fileId, await read(fileId))
    },
  })

  // Order matters: `?inline` is checked before the bare `.css` route, and the
  // global route excludes `.module.css`.
  return [
    route(
      'ccteam-css-modules',
      MODULE_PREFIX,
      source => source.endsWith('.module.css'),
      source => source,
      (fileId, css) => compileCss(pluginId, fileId, css, true).code,
    ),
    route(
      'ccteam-css-inline',
      INLINE_PREFIX,
      source => source.endsWith(`.css${INLINE_QUERY}`),
      source => source.slice(0, -INLINE_QUERY.length),
      (_fileId, css) => `export default ${JSON.stringify(css)};`,
    ),
    route(
      'ccteam-css-global',
      GLOBAL_PREFIX,
      source => source.endsWith('.css') && !source.endsWith('.module.css'),
      source => source,
      (fileId, css) => compileCss(pluginId, fileId, css, false).code,
    ),
  ]
}
