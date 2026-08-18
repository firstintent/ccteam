/**
 * Per-session ccteam identity carried by ACP `_meta.ccteam`.
 *
 * One DSH runtime serves many ccteam hires plus the human at the DSH web UI, so
 * a credential belongs to a session, never to the process. Nothing here ever
 * touches `process.env`.
 */
export interface CcteamSessionMeta {
  /** ccteam gateway session id (`s<N>`), for diagnostics only. */
  sid?: string
  /** Per-session MCP bearer (`ccteam-sid:<sid>:<secret>`). */
  bearer?: string
  /** Daemon base URL override for this session. */
  mcpUrl?: string
  /** Tool-approval posture for turns this transport owns. */
  approvalMode?: 'skip' | 'hitl'
}

/** Plugin-scoped map of DSH session id → ccteam identity. */
export class SessionCredentialStore {
  private readonly entries = new Map<string, CcteamSessionMeta>()
  private readonly removalListeners = new Set<(sessionId: string, removed: CcteamSessionMeta) => void>()

  /** Overwrite the identity of one session (later `session/new` or `session/load` wins). */
  set(sessionId: string, meta: CcteamSessionMeta): void {
    this.entries.set(sessionId, meta)
  }

  get(sessionId: string | undefined): CcteamSessionMeta | undefined {
    if (sessionId === undefined) return undefined
    return this.entries.get(sessionId)
  }

  /** Best-effort cleanup; publishes the removed entry so credential caches can drop it. */
  delete(sessionId: string): void {
    const removed = this.entries.get(sessionId)
    if (removed === undefined) return
    this.entries.delete(sessionId)
    for (const listener of this.removalListeners) {
      try {
        listener(sessionId, removed)
      } catch {
        // a listener failure must not break session teardown
      }
    }
  }

  onRemoved(listener: (sessionId: string, removed: CcteamSessionMeta) => void): () => void {
    this.removalListeners.add(listener)
    return () => {
      this.removalListeners.delete(listener)
    }
  }

  get size(): number {
    return this.entries.size
  }
}

/** Read `params._meta.ccteam` from an ACP request; returns undefined when absent. */
export function parseCcteamMeta(params: unknown): CcteamSessionMeta | undefined {
  const meta = asRecord(asRecord(params)._meta).ccteam
  if (!isRecord(meta)) return undefined
  const parsed: CcteamSessionMeta = {}
  const sid = trimmedString(meta.sid)
  if (sid !== undefined) parsed.sid = sid
  const bearer = trimmedString(meta.bearer)
  if (bearer !== undefined) parsed.bearer = bearer
  const mcpUrl = trimmedString(meta.mcpUrl)
  if (mcpUrl !== undefined) parsed.mcpUrl = mcpUrl
  parsed.approvalMode = meta.approvalMode === 'hitl' ? 'hitl' : 'skip'
  return parsed
}

function trimmedString(value: unknown): string | undefined {
  if (typeof value !== 'string') return undefined
  const trimmed = value.trim()
  return trimmed === '' ? undefined : trimmed
}

function asRecord(value: unknown): Record<string, unknown> {
  return isRecord(value) ? value : {}
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}
