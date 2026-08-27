/** Pure formatting helpers for the workbench (no DOM, unit-testable). */

/**
 * Compact cost text.
 * @param costUsd - accumulated cost.
 * @returns `$x.xx` (two decimals under $10, one above), or null when unknown.
 */
export function formatCost(costUsd: number | undefined): string | null {
  if (costUsd === undefined || !Number.isFinite(costUsd) || costUsd < 0.005) return null
  if (costUsd >= 10) return `$${costUsd.toFixed(1)}`
  return `$${costUsd.toFixed(2)}`
}

/**
 * Compact token count (`1.2k`, `34k`, `1.5M`).
 * @param tokens - token count.
 * @returns the text, or null when unknown.
 */
export function formatTokens(tokens: number | undefined): string | null {
  if (tokens === undefined || !Number.isFinite(tokens)) return null
  if (tokens < 1000) return String(Math.round(tokens))
  if (tokens < 1_000_000) return `${(tokens / 1000).toFixed(tokens < 10_000 ? 1 : 0)}k`
  return `${(tokens / 1_000_000).toFixed(1)}M`
}

/** Relative-time buckets, rendered through the locale (`time.*` keys). */
export type RelativeTime =
  | { unit: 'now' }
  | { unit: 'minutes' | 'hours' | 'days'; value: number }

/**
 * Bucket an ISO timestamp relative to `now`.
 * @param iso - ISO-8601 timestamp (undefined/unparseable = null).
 * @param now - reference time in ms.
 * @returns the bucket, or null when unknown.
 */
export function relativeTime(iso: string | undefined, now: number): RelativeTime | null {
  if (iso === undefined) return null
  const at = Date.parse(iso)
  if (!Number.isFinite(at)) return null
  const seconds = Math.max(0, (now - at) / 1000)
  if (seconds < 60) return { unit: 'now' }
  if (seconds < 3600) return { unit: 'minutes', value: Math.floor(seconds / 60) }
  if (seconds < 86_400) return { unit: 'hours', value: Math.floor(seconds / 3600) }
  return { unit: 'days', value: Math.floor(seconds / 86_400) }
}

/**
 * Elapsed text for the working indicator (`12s`, `1m 05s`, `1h 02m`).
 * @param ms - elapsed milliseconds.
 * @returns the text.
 */
export function formatElapsed(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000))
  if (total < 60) return `${total}s`
  const minutes = Math.floor(total / 60)
  const seconds = total % 60
  if (minutes < 60) return `${minutes}m ${String(seconds).padStart(2, '0')}s`
  const hours = Math.floor(minutes / 60)
  return `${hours}h ${String(minutes % 60).padStart(2, '0')}m`
}

/**
 * Title derived from a task's first line (the spawn form's auto-title).
 * @param task - the first task text.
 * @param max - character budget.
 * @returns the title, or undefined when the task is blank.
 */
export function titleFromTask(task: string, max = 60): string | undefined {
  const line = task.split('\n').map(s => s.trim()).find(s => s !== '')
  if (line === undefined) return undefined
  const chars = Array.from(line)
  return chars.length <= max ? line : `${chars.slice(0, max - 1).join('')}…`
}

/**
 * Two-letter vendor monogram for the tree glyph (text, never a brand mark).
 * @param vendor - vendor id.
 * @returns the glyph text.
 */
export function vendorGlyph(vendor: string): string {
  return vendor.slice(0, 2)
}

/**
 * Basename of a stored path (the attachment route's `name`).
 * @param path - stored path.
 * @returns the last path segment.
 */
export function basename(path: string): string {
  const cut = path.lastIndexOf('/')
  return cut === -1 ? path : path.slice(cut + 1)
}
