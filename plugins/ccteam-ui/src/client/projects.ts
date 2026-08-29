/**
 * The first-run 「添加工作区」 flow: an absolute directory (plus an optional
 * slug) becomes a ccteam project through the BFF. The slug rule mirrors the
 * daemon's grammar (`[a-z0-9-]`, ≤60, no edge dashes) the way ccteam web's
 * own form derives it from the directory name — pure, so the panel can
 * refuse a relative path before anything crosses the wire.
 */
import type { ProjectCreateRequest } from '../shared/contract.js'
import type { ApiClient } from './api.js'
import type { Action } from './store.js'

type Dispatch = (action: Action) => void

/**
 * Whether the text names an absolute directory (the daemon's host is POSIX).
 * @param path - user input.
 * @returns true for `/…`.
 */
export function isAbsolutePath(path: string): boolean {
  return path.startsWith('/')
}

/**
 * Derive a slug from a path's basename: lowercase `[a-z0-9-]+`, ≤60, no
 * leading/trailing `-`.
 * @param path - directory path.
 * @returns the slug (empty when nothing survives).
 */
export function slugFromPath(path: string): string {
  const base = path.trim().replace(/\/+$/, '').split('/').pop() ?? ''
  return base
    .toLowerCase()
    .replace(/[^a-z0-9-]+/g, '-')
    .replace(/-{2,}/g, '-')
    .replace(/^-+|-+$/g, '')
    .slice(0, 60)
    .replace(/-+$/, '')
}

/**
 * Build the create request: trimmed absolute path (trailing slashes dropped),
 * the user's slug or one derived from the directory name.
 * @param path - user input.
 * @param slug - user input (blank = derive).
 * @returns the request, or null when the path is not absolute or yields no slug.
 */
export function projectCreateRequest(path: string, slug: string): ProjectCreateRequest | null {
  const trimmed = path.trim()
  const withoutSlashes = trimmed.replace(/\/+$/, '')
  const cleanPath = withoutSlashes === '' ? trimmed : withoutSlashes
  if (!isAbsolutePath(cleanPath)) return null
  const chosen = slug.trim() === '' ? slugFromPath(cleanPath) : slug.trim()
  if (chosen === '') return null
  return { path: cleanPath, slug: chosen }
}

/**
 * Create the project through the BFF and land the outcome in the store.
 * @param dispatch - store write path.
 * @param api - BFF client.
 * @param path - user input.
 * @param slug - user input (blank = derive).
 * @returns true when the project now exists.
 */
export async function createProject(dispatch: Dispatch, api: ApiClient, path: string, slug: string): Promise<boolean> {
  const request = projectCreateRequest(path, slug)
  if (request === null) return false
  dispatch({ type: 'project_create_started' })
  try {
    const response = await api.call('projects.create', request)
    if (!response.ok || response.project === undefined) {
      dispatch({ type: 'project_create_failed', message: response.error ?? response.errorKind ?? 'unknown' })
      return false
    }
    dispatch({ type: 'project_create_done', project: response.project })
    return true
  } catch (error) {
    dispatch({ type: 'project_create_failed', message: error instanceof Error ? error.message : String(error) })
    return false
  }
}
