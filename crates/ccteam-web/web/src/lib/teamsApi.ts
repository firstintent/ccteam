// V0.5.0 F96 — Agent Teams API client + shared types.
//
// Mirrors the wire shapes in `crates/ccteam-web/src/teams/` 1:1.
// Wire-stable: any rename here MUST also update the Rust structs and
// the integration test (`api_v1_teams_test.rs`).
//
// Auth: every request goes with `credentials: "same-origin"` so the
// `ccteam_token` cookie (set by the URL shim) is forwarded; the global
// fetchInterceptor handles 401 → token-entry-gate.

/** Member view emitted by `/api/v1/teams/{name}` and used by the
 *  Topology panel. Field set matches Rust `MemberView`. */
export interface TeamMember {
  agent_id: string;
  name: string;
  agent_type: string | null;
  model: string | null;
  color: string | null;
  joined_at: number | null;
  cwd: string | null;
  /** Inline prompt — only present for ad-hoc teammates
   *  (`agent_type ∈ {general-purpose, team-lead}`). */
  prompt: string | null;
  subscriptions: string[];
  tmux_pane_id: string | null;
  backend_type: string | null;
  plan_mode_required: boolean | null;
  /** Computed server-side. `false` → 📝 ad-hoc badge. `true` →
   *  ↗ definition link. */
  definition_backed: boolean;
}

export interface TeamConfig {
  name: string;
  description: string | null;
  created_at: number | null;
  lead_agent_id: string | null;
  lead_session_id: string | null;
  members: TeamMember[];
}

export interface TeamListEntry {
  name: string;
  description: string | null;
  member_count: number;
  /** RFC3339 timestamp of the most-recently-joined member, or null
   *  when no members carry a `joinedAt`. The list page uses this to
   *  surface "active recently" hints. */
  last_activity: string | null;
}

export interface TaskCounts {
  pending: number;
  in_progress: number;
  completed: number;
}

export interface TeamTask {
  id: string;
  title: string;
  status: string;
  assignee: string | null;
  description: string | null;
  dependencies: string[];
  created_at: string | null;
  completed_at: string | null;
}

export interface InboxMessage {
  from: string;
  to: string;
  text: string;
  timestamp: string;
  color: string | null;
  read: boolean;
  summary: string | null;
  /** Server-derived: true iff `text` parses as a JSON
   *  `{type: "idle_notification"}` system message. The Mailbox panel
   *  hides these. */
  is_idle_notification: boolean;
}

export interface TeamDetailResponse {
  config: TeamConfig;
  task_count: TaskCounts;
  recent_messages: InboxMessage[];
}

export type AgentDefinitionScope = "project" | "user" | "plugin" | "managed";

export interface AgentDefinition {
  path: string;
  scope: AgentDefinitionScope;
  /** YAML frontmatter parsed as a free-form JSON object. May contain
   *  `tools`, `model`, `description`, etc. */
  frontmatter: Record<string, unknown>;
  body: string;
  /** Frontmatter `skills` list (Anthropic does NOT apply these when
   *  the definition runs as a teammate — UI surfaces a warning). */
  skills_not_applied: string[];
  mcp_servers_not_applied: string[];
}

export interface DefinitionResponse {
  agent_type: string;
  teammate: string;
  definition: AgentDefinition | null;
  /** True iff the member is definition-backed but no `.md` file
   *  resolved on disk (PRD §F96 acceptance #4 — warning banner). */
  definition_missing: boolean;
}

async function getJson<T>(url: string): Promise<T> {
  const resp = await fetch(url, { credentials: "same-origin" });
  if (resp.status === 401) {
    throw new Error("UNAUTHENTICATED");
  }
  if (resp.status === 404) {
    throw new Error("NOT_FOUND");
  }
  if (!resp.ok) {
    throw new Error(`${url}: ${resp.status}`);
  }
  return (await resp.json()) as T;
}

/** `GET /api/v1/teams` — list every team under `~/.claude/teams/`. */
export function fetchTeams(): Promise<TeamListEntry[]> {
  return getJson<TeamListEntry[]>("/api/v1/teams");
}

/** `GET /api/v1/teams/{name}` — config + task counts + recent
 *  messages preview for the detail page header. */
export function fetchTeamDetail(name: string): Promise<TeamDetailResponse> {
  return getJson<TeamDetailResponse>(
    `/api/v1/teams/${encodeURIComponent(name)}`,
  );
}

/** `GET /api/v1/teams/{name}/tasks` — Kanban data. */
export function fetchTeamTasks(name: string): Promise<TeamTask[]> {
  return getJson<TeamTask[]>(
    `/api/v1/teams/${encodeURIComponent(name)}/tasks`,
  );
}

/** `GET /api/v1/teams/{name}/inbox` — every inbox merged by default;
 *  pass `teammate` to filter and `since` for incremental polling.
 *  Returns oldest-first to match the on-disk JSON order. */
export function fetchTeamInbox(
  name: string,
  opts: { teammate?: string; since?: string } = {},
): Promise<InboxMessage[]> {
  const params = new URLSearchParams();
  if (opts.teammate) params.set("teammate", opts.teammate);
  if (opts.since) params.set("since", opts.since);
  const qs = params.toString();
  const url = `/api/v1/teams/${encodeURIComponent(name)}/inbox${qs ? `?${qs}` : ""}`;
  return getJson<InboxMessage[]>(url);
}

/** `GET /api/v1/teams/{name}/member/{teammate}/definition` — only for
 *  definition-backed members. Throws `NOT_FOUND` for ad-hoc. */
export function fetchMemberDefinition(
  team: string,
  teammate: string,
): Promise<DefinitionResponse> {
  return getJson<DefinitionResponse>(
    `/api/v1/teams/${encodeURIComponent(team)}/member/${encodeURIComponent(teammate)}/definition`,
  );
}

/** Build the SSE URL for `/api/v1/teams/{name}/events`. Caller is
 *  responsible for opening the EventSource (the existing
 *  `useProgressStream` hook is tied to `/sse/*` so the Teams feed
 *  uses a local hook in `TeamDetailPage`). */
export function teamEventsUrl(name: string): string {
  return `/api/v1/teams/${encodeURIComponent(name)}/events`;
}

/** Format the JS-epoch-ms `created_at` / `joined_at` integers into a
 *  short relative-time string ("3h ago", "yesterday", ...). Locale-
 *  agnostic; falls back to "—" when input is null. */
export function relativeFromEpoch(ms: number | null, now: number = Date.now()): string {
  if (ms === null) return "—";
  const delta = Math.round((ms - now) / 1000);
  const abs = Math.abs(delta);
  const rtf = new Intl.RelativeTimeFormat(undefined, { numeric: "auto" });
  if (abs < 60) return rtf.format(delta, "second");
  if (abs < 3600) return rtf.format(Math.round(delta / 60), "minute");
  if (abs < 86400) return rtf.format(Math.round(delta / 3600), "hour");
  return rtf.format(Math.round(delta / 86400), "day");
}

/** Map Anthropic config colors → Tailwind utility classes. The host
 *  fixture surfaces blue / green / yellow / red / purple; anything
 *  unrecognised falls back to a neutral surface tone. */
export function colorClasses(color: string | null | undefined): {
  bg: string;
  text: string;
  border: string;
} {
  switch ((color ?? "").toLowerCase()) {
    case "blue":
      return { bg: "bg-blue-500/20", text: "text-blue-300", border: "border-blue-400/40" };
    case "green":
      return { bg: "bg-emerald-500/20", text: "text-emerald-300", border: "border-emerald-400/40" };
    case "yellow":
      return { bg: "bg-amber-500/20", text: "text-amber-300", border: "border-amber-400/40" };
    case "red":
      return { bg: "bg-rose-500/20", text: "text-rose-300", border: "border-rose-400/40" };
    case "purple":
      return { bg: "bg-violet-500/20", text: "text-violet-300", border: "border-violet-400/40" };
    case "cyan":
      return { bg: "bg-cyan-500/20", text: "text-cyan-300", border: "border-cyan-400/40" };
    case "pink":
      return { bg: "bg-pink-500/20", text: "text-pink-300", border: "border-pink-400/40" };
    default:
      return {
        bg: "bg-surface-700/50",
        text: "text-text-secondary",
        border: "border-surface-700/40",
      };
  }
}

/** Topology node state — what color halo to draw around a member. */
export type MemberState = "in-process" | "tmux" | "missing" | "idle";

/** Derive the state badge tone from raw backendType + recent idle
 *  events. `idle` is set by the SPA when an `idle_notification` for
 *  this teammate is observed within the last 30s (PRD §F96 spec). */
export function deriveMemberState(
  backendType: string | null | undefined,
  isIdle: boolean,
): MemberState {
  if (isIdle) return "idle";
  switch ((backendType ?? "").toLowerCase()) {
    case "in-process":
      return "in-process";
    case "tmux":
      return "tmux";
    case "":
      return "missing";
    default:
      return "missing";
  }
}
