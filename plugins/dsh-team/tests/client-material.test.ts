/**
 * Material guards enforcing the owner decree (2026-08-21): semantic DSH
 * tokens only (no hex, no rgb(), no --dsw-static-), zero ccteam-web imports,
 * imports confined to the client-bundle external allowlist, and bilingual
 * dictionary parity. Mechanical on purpose — these fail the moment a
 * foreign color or import sneaks in.
 */
import { readFileSync, readdirSync } from 'node:fs'
import { join } from 'node:path'
import { describe, expect, it } from 'vitest'
import { en, zh } from '../src/client/locales.js'

const CLIENT_DIR = join(__dirname, '..', 'src', 'client')

function clientFiles(extension: string): string[] {
  return readdirSync(CLIENT_DIR)
    .filter(name => name.endsWith(extension))
    .map(name => join(CLIENT_DIR, name))
}

describe('CSS material', () => {
  const cssFiles = clientFiles('.css')

  it('has stylesheet(s) to guard', () => {
    expect(cssFiles.length).toBeGreaterThan(0)
  })

  it('contains no hardcoded hex colors, no rgb()/rgba()/hsl(), no --dsw-static-*', () => {
    for (const file of cssFiles) {
      const text = readFileSync(file, 'utf8')
      expect(text).not.toMatch(/#[0-9a-fA-F]{3,8}\b/)
      expect(text).not.toMatch(/\brgba?\(/)
      expect(text).not.toMatch(/\bhsla?\(/)
      expect(text).not.toContain('--dsw-static-')
    }
  })

  it('references only the semantic token families', () => {
    const allowed = [
      '--dsw-alias-',
      '--dsw-specific-',
      '--dsw-font-',
      '--dsw-shadow-',
      '--dsw-mask-',
      '--ds-font-family-',
      '--ds-transition-duration',
      '--ds-ease-',
    ]
    for (const file of cssFiles) {
      const text = readFileSync(file, 'utf8')
      for (const match of text.matchAll(/var\((--[a-z0-9-]+)/gi)) {
        const token = match[1]!
        expect(
          allowed.some(prefix => token.startsWith(prefix)),
          `${file}: token ${token} is outside the semantic families`,
        ).toBe(true)
      }
    }
  })
})

describe('import material', () => {
  const sourceFiles = [...clientFiles('.ts'), ...clientFiles('.tsx')]

  function specifiersOf(file: string): string[] {
    const text = readFileSync(file, 'utf8')
    const specifiers: string[] = []
    for (const match of text.matchAll(/from\s+'([^']+)'/g)) specifiers.push(match[1]!)
    for (const match of text.matchAll(/import\(\s*'([^']+)'\s*\)/g)) specifiers.push(match[1]!)
    return specifiers
  }

  it('imports nothing from ccteam-web (structural: specifiers, not prose)', () => {
    for (const file of sourceFiles) {
      for (const specifier of specifiersOf(file)) {
        expect(specifier.includes('ccteam-web'), `${file} imports "${specifier}"`).toBe(false)
      }
    }
  })

  it('imports only relative modules and the client-bundle externals', () => {
    const allowlist = new Set([
      'react',
      'react/jsx-runtime',
      'react-dom',
      'react-dom/client',
      '@deepseek-ai/cordis',
      '@deepseek-ai/dsh-client-ui-slots',
      '@deepseek-ai/dsh-client-ui-primitives',
      '@deepseek-ai/dsh-client-runtime/client',
    ])
    for (const file of sourceFiles) {
      for (const specifier of specifiersOf(file)) {
        const ok = specifier.startsWith('./') || specifier.startsWith('../') || allowlist.has(specifier)
        expect(ok, `${file}: import "${specifier}" is outside the external allowlist`).toBe(true)
      }
    }
  })
})

describe('locale material', () => {
  it('zh and en carry the same key set', () => {
    expect(Object.keys(en).sort()).toEqual(Object.keys(zh).sort())
  })

  it('every value is non-empty', () => {
    for (const dict of [zh, en]) {
      for (const [key, value] of Object.entries(dict)) {
        expect(value.length, `empty copy for ${key}`).toBeGreaterThan(0)
      }
    }
  })
})
