// Per-session context-window breakdown — `GET /api/v1/sessions/{sid}/context`.
//
// What the window is MADE OF, mechanically (no model asked): system prompt,
// tool schemas, memory files, skills, MCP tools, and — inside the
// conversation — how much each tool's calls/results and each attachment type
// cost. The gateway asks the harness when the session is resident (`live:
// true`); otherwise it serves the snapshot persisted at the end of the last
// turn (`live: false`, dated by `captured_at`). `breakdown: null` = this
// harness offers no breakdown and no snapshot exists (non-Claude harnesses,
// terminal-protocol or remote-host sessions, or a session that has not
// finished a turn since the feature shipped) — a fact to explain, not an
// error.
//
// Every field below the top level is optional/nullable on purpose: the panel
// degrades section by section, and unknown extra fields are ignored.

import { httpError } from "./httpError";
import { sessionUrl } from "./sessionsApi";

/** `used` occupies the window; `deferred` = tool schemas NOT loaded (never
 *  counted — list them apart, muted); `buffer` = reserved for auto-compaction;
 *  `free` = headroom. Unknown kinds render as plain rows, outside the bar. */
export type ContextCategoryKind = "used" | "deferred" | "buffer" | "free";

export interface ContextItem {
  name: string;
  /** Where it came from (`ccteam`, `Project`, `User`, `projectSettings`…). */
  source?: string | null;
  /** Secondary line — usually a path. */
  detail?: string | null;
  tokens?: number | null;
  /** `false` = present but NOT loaded into the context (deferred schema). */
  loaded?: boolean | null;
}

export interface ContextCategory {
  id: string;
  name: string;
  tokens: number;
  kind: ContextCategoryKind | string;
  items?: ContextItem[] | null;
}

export interface ContextToolUsage {
  name: string;
  call_tokens?: number | null;
  result_tokens?: number | null;
}

export interface ContextAttachmentUsage {
  name: string;
  tokens?: number | null;
}

export interface ContextMessages {
  tool_call_tokens?: number | null;
  tool_result_tokens?: number | null;
  attachment_tokens?: number | null;
  assistant_tokens?: number | null;
  user_tokens?: number | null;
  other_tokens?: number | null;
  tools?: ContextToolUsage[] | null;
  attachments?: ContextAttachmentUsage[] | null;
}

/** The API's real accounting for the most recent request. Cache hit ratio =
 *  `cache_read / (cache_read + cache_creation + input)`. */
export interface ContextLastRequest {
  input_tokens?: number | null;
  output_tokens?: number | null;
  cache_creation_tokens?: number | null;
  cache_read_tokens?: number | null;
}

export interface ContextBreakdown {
  /** Harness id on the wire (`claude`…); the UI says "harness". */
  vendor?: string | null;
  model?: string | null;
  /** RFC 3339 — when these numbers were taken. */
  captured_at?: string | null;
  used_tokens?: number | null;
  window_tokens?: number | null;
  auto_compact_threshold?: number | null;
  categories?: ContextCategory[] | null;
  messages?: ContextMessages | null;
  last_request?: ContextLastRequest | null;
}

export interface SessionContextResponse {
  sid: string;
  live: boolean;
  breakdown: ContextBreakdown | null;
}

export function sessionContextUrl(sid: string): string {
  return `${sessionUrl(sid)}/context`;
}

/** Same auth + error mapping as the other `/api/v1/sessions/{sid}/*` reads:
 *  401 → `UNAUTHENTICATED`, 404 (unknown sid) → `NOT_FOUND`, anything else
 *  non-2xx (503 = no gateway) lifts the server's `{error}` body. */
export async function getSessionContext(
  sid: string,
  options: { signal?: AbortSignal } = {},
): Promise<SessionContextResponse> {
  let res: Response;
  try {
    res = await fetch(sessionContextUrl(sid), {
      headers: { Accept: "application/json" },
      credentials: "same-origin",
      ...(options.signal ? { signal: options.signal } : {}),
    });
  } catch (e) {
    throw new Error(`network: ${e instanceof Error ? e.message : "connection failed"}`);
  }
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (res.status === 404) throw new Error("NOT_FOUND");
  if (!res.ok) throw await httpError(res);
  return (await res.json()) as SessionContextResponse;
}
