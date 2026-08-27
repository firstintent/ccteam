/**
 * Rolldown plugins routing stylesheet imports into JavaScript, build-time only.
 *
 * This is DSH's own mechanism, spelled the way the in-tree preset spells it
 * (references/deepseek-harness/packages/client/tsdown.client.ts, the
 * `dsh-css-*-inline` plugins): every sheet is compiled by lightningcss inside
 * the bundle — `x.module.css` yields its hashed class map under the
 * `[hash]_[local]` pattern and injects one tagged <style> when the factory
 * runs, `x.css?inline` exports the compiled text, `x.css` injects only.
 *
 * lightningcss is the point, not a convenience: its hashes are identifier-safe
 * (a hash that would start with a digit is prefixed with `_`). A hand-rolled
 * hex hash shipped once (v0.10.4) and, six times out of ten, produced
 * `.9a3484fd_entry` — not a valid selector, so the browser dropped the whole
 * sheet and the panel rendered as bare DOM.
 *
 * Virtual ids carry the `.mjs` suffix: tsdown's own CSS guard matches ids
 * ending in `.css` and demands @tsdown/css, so a virtual id must not.
 */
import { readFile } from 'node:fs/promises'
import { basename, dirname, relative, resolve, sep } from 'node:path'
import { transform } from 'lightningcss'

const MODULE_PREFIX = '\0ccteam-css-module:'
const GLOBAL_PREFIX = '\0ccteam-css-global:'
const INLINE_PREFIX = '\0ccteam-css-inline:'
const VIRTUAL_SUFFIX = '.mjs'
const INLINE_QUERY = '?inline'

/** The DSH preset's CSS Modules class pattern (tsdown.client.ts:514). */
export const CSS_MODULES_PATTERN = '[hash]_[local]'

/** How one stylesheet import is consumed. */
export type CssMode = 'module' | 'global' | 'inline'

export interface CompiledCss {
  /** The JavaScript module source replacing the stylesheet import. */
  code: string
  /** local class name → hashed class list (empty outside `module` mode). */
  classMap: Record<string, string>
  /** The compiled (minified, hashed) stylesheet text. */
  css: string
}

/**
 * Emit the style injector module — byte-for-byte the DSH preset's
 * `styleInjectionModule` (tsdown.client.ts:34-53): one <style> per sheet,
 * stamped `data-plugin` (so the loader's `claimStyles` bookkeeping owns it)
 * and `data-plugin-css` (so a re-materialization after an HMR invalidate
 * never stacks a second copy), plus the class-map default export in module mode.
 * @param pluginId - npm package name.
 * @param fileId - stylesheet path; only its basename reaches the tag id.
 * @param css - compiled stylesheet text.
 * @param classMap - the CSS Modules export map; omitted for global sheets.
 * @returns the module source.
 */
export function styleInjectionModule(
  pluginId: string,
  fileId: string,
  css: string,
  classMap?: Readonly<Record<string, string>>,
): string {
  const source = [
    `const css = ${JSON.stringify(css)};`,
    `const tagId = ${JSON.stringify(`${pluginId}/${basename(fileId)}`)};`,
    "if (typeof document !== 'undefined' && document.querySelector('style[data-plugin-css=' + JSON.stringify(tagId) + ']') === null) {",
    "  const tag = document.createElement('style');",
    `  tag.dataset.plugin = ${JSON.stringify(pluginId)};`,
    '  tag.dataset.pluginCss = tagId;',
    '  tag.textContent = css;',
    '  document.head.appendChild(tag);',
    '}',
  ]
  source.push(classMap === undefined ? 'export {};' : `export default ${JSON.stringify(classMap)};`)
  return source.join('\n')
}

/**
 * Compile one stylesheet with lightningcss, exactly as the DSH preset does.
 * @param pluginId - npm package name (tag stamp).
 * @param fileId - absolute stylesheet path.
 * @param source - stylesheet text.
 * @param mode - how the import is consumed.
 * @param hashRoot - directory the `[hash]` input is taken relative to. The
 *   preset hashes the absolute path, which is fine for a build that only ever
 *   runs in one tree; this package is packed into a tarball that must be
 *   byte-reproducible across checkouts, so the hash input is the
 *   package-relative path when a root is given.
 * @returns the replacement module source, the class map, and the compiled text.
 */
export function compileCss(
  pluginId: string,
  fileId: string,
  source: string,
  mode: CssMode,
  hashRoot?: string,
): CompiledCss {
  const filename = hashRoot === undefined ? fileId : relative(hashRoot, fileId).split(sep).join('/')
  const { code, exports } = transform({
    filename,
    code: Buffer.from(source),
    ...(mode === 'module' ? { cssModules: { pattern: CSS_MODULES_PATTERN } } : {}),
    minify: true,
  })
  const css = code.toString()
  const classMap: Record<string, string> = {}
  for (const [local, exp] of Object.entries(exports ?? {}).sort(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0))) {
    // `composes:` appends the composed classes, which the preset (reading
    // `exp.name` alone) would drop; a class list is what the DOM needs.
    classMap[local] = [exp.name, ...exp.composes.map(composed => composed.name)].join(' ')
  }
  if (mode === 'inline') return { code: `export default ${JSON.stringify(css)};`, classMap, css }
  return {
    code: styleInjectionModule(pluginId, fileId, css, mode === 'module' ? classMap : undefined),
    classMap,
    css,
  }
}

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
 * @param pluginId - npm package name.
 * @param read - injected for tests; defaults to reading the real file.
 * @param hashRoot - see {@link compileCss}.
 * @returns the three routes, in matching order (module, inline, global).
 */
export function createCssPlugins(
  pluginId: string,
  read: (file: string) => Promise<string> = file => readFile(file, 'utf8'),
  hashRoot?: string,
): CssPlugin[] {
  const route = (
    name: string,
    prefix: string,
    matches: (source: string) => boolean,
    strip: (source: string) => string,
    mode: CssMode,
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
      return compileCss(pluginId, fileId, await read(fileId), mode, hashRoot).code
    },
  })

  // Order matters: `?inline` is checked before the bare `.css` route, and the
  // global route excludes `.module.css`.
  return [
    route('ccteam-css-modules-inline', MODULE_PREFIX, source => source.endsWith('.module.css'), source => source, 'module'),
    route(
      'ccteam-css-text-inline',
      INLINE_PREFIX,
      source => source.endsWith(`.css${INLINE_QUERY}`),
      source => source.slice(0, -INLINE_QUERY.length),
      'inline',
    ),
    route(
      'ccteam-css-global-inline',
      GLOBAL_PREFIX,
      source => source.endsWith('.css') && !source.endsWith('.module.css'),
      source => source,
      'global',
    ),
  ]
}
