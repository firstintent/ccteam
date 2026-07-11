// v0.8.9 — the persistent chat SHELL.
//
// ChatConsole is the long-lived shell; the PER-SID session view is a keyed
// child, `<SessionView key={sid} sid={sid} />` (see ./SessionView.tsx). The
// `key` is the structural fix: React remounts a fresh SessionView on every
// session switch, so all per-sid state (transcript rows / SSE buffer / draft /
// chat|terminal view / scroll / HITL) resets ATOMICALLY — no state survives a
// switch (kills the "fresh session briefly shows the previous session's
// messages" bug + the latent `saveRows(newSid, oldRows)` persist race at once).
//
// What the SHELL owns (persists across switches):
//   - ROUTE  `/chat/s/:sid` (App.tsx) — `useParams` gives the active sid; the
//            shell routes globalView (插件市场/Status/Settings) vs sid vs empty.
//   - RAIL   the cross-project session list (listSessions fanned over
//            /api/v1/projects) + the registered-project union (config.yaml SoT,
//            so a session-less project still lists) — the sidebar + switcher.
//   - CHROME v0.8.24 A1 — no top bar; collapsible sidebar (search ⌘K +
//            新建/工作流/会话/设置) + CostPill/Avatar in the rail bottom;
//            NewSessionModal (create a session / brand-new project inline).
//            Home empty state replaces the old "选一个 session" stub.
//
// What moved OUT to SessionView (per-sid): the transcript rows + localStorage
// seed/persist, `useSessionEvents(sid)` + its fold, the draft + submitTurn, the
// chat|terminal toggle + TerminalView, stopSession, and HITL approval resolve.
//
// Red lines: reads structured turn/SSE frames (never scrapes a pane); the
// new-session form defaults to ROLELESS (v0.8.20 F4 — a bare-claude session),
// and the role picker + terminal runtimes are admin-only (UI-level beta-gate).

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { NavLink, useLocation, useNavigate, useParams } from "react-router-dom";
import {
  Activity,
  ChevronLeft,
  ChevronRight,
  LayoutGrid,
  Menu,
  MessageSquare,
  Pencil,
  Plus,
  Puzzle,
  Search,
  Server,
  Settings,
  Trash2,
  X,
} from "lucide-react";
import CostPill from "../components/CostPill";
import AvatarMenu from "../components/AvatarMenu";
import { Combobox, type ComboboxOption } from "../components/ui";
import MarketplaceView from "./MarketplaceView";
import StatusView from "./StatusView";
import HostsView from "./HostsView";
import SettingsPage from "./SettingsPage";
import SessionView from "./SessionView";
import WorkflowView from "./WorkflowView";
import {
  createProject as apiCreateProject,
  deleteProject,
  fetchDashboard,
} from "../lib/dashboardApi";
import {
  createSession as apiCreateSession,
  listHistorySessions,
  listExternalSessions,
  importExternalSession,
  renameSession,
  resumeSession,
  listProjectRoles,
  listSessions,
  type RoleSummary,
  type SessionView as SessionSummary,
  type HistorySessionView,
  type ExternalSessionView,
} from "../lib/sessionsApi";
import { toastBus } from "../lib/toastBus";
import { ROLE_SUGGESTIONS, ROLELESS, resolveRole } from "./chatDefaults";
import { mergeProjectSlugs } from "./projectList";
import { useWebSettings } from "../hooks/useWebSettings";
import { useMe } from "../hooks/useMe";
import { navLabel, tr } from "../lib/i18n";

/** A switcher entry — one live gateway session, grouped under its project. */
type RailSession = SessionSummary;

/** Display label for a rail/history session row (v0.8.22 P1 session-title
 *  system): the user-facing title when set, else the pre-existing role
 *  fallback (`"(无 role)"` for a roleless session — unchanged from before
 *  titles existed). Pure + exported so it has a cheap unit test: this repo
 *  has no DOM/interaction test harness (no `@testing-library/react`), so
 *  inline-rename UI is exercised via this extracted pure helper rather than
 *  a simulated click/keypress (mirrors `relativeTimeZh`/`mergeProjectSlugs`'s
 *  precedent of testing the pure logic directly). */
export function railSessionLabel(s: { title?: string | null; role: string }): string {
  return s.title || s.role || "(无 role)";
}

// ---------------------------------------------------------------------------
// v0.8.23 review §1.3-D item 9/10 — session attention (waiting-approval /
// unread / error) + real alive/busy dot semantics. Data all comes from the
// existing `SessionView` fields (`waiting_approval`, `status`, `turn_count`)
// — this is pure aggregation, no new backend polling. Split into small pure
// functions (no DOM/localStorage inside the ones that matter for logic) so
// they get the same direct-unit-test treatment as `railSessionLabel` /
// `relativeTimeZh` above (this repo has no `@testing-library/react` DOM
// harness).
// ---------------------------------------------------------------------------

/** Item 10 — the rail dot's color is the session's REAL liveness/business
 *  (the `status` field the backend already derives from progress.jsonl via
 *  `classify_progress_activity_for_sid`: `"idle" | "working" | "stale" |
 *  "stuck"`, or the `"live"` fallback when no progress event has landed
 *  yet), never "is this the currently-selected row" — that's now expressed
 *  ONLY by the row's background highlight. Mirrors `sessionActivityMeta` in
 *  `StatusView.tsx` (the fleet-table precedent for this same status
 *  vocabulary), specialized to a plain dot color. */
export function sessionDotClass(status: string | null | undefined): string {
  switch (status) {
    case "working":
      return "bg-status-waiting animate-pulse"; // amber, pulsing — busy in a turn
    case "stale":
      return "bg-status-waiting"; // amber, steady — aging without a fresh event
    case "stuck":
      return "bg-status-error"; // red — the watchdog's stuck verdict
    default:
      return "bg-status-running"; // green — idle/live, nothing wrong
  }
}

/** One row's attention state, in priority order (a row shows at most one
 *  badge — the most urgent). `null` = nothing needs the user's attention. */
export type AttentionKind = "approval" | "error" | "unread" | null;

/** Item 9b — a session's last completed turn is "unread" once its
 *  `turn_count` has grown past what was last recorded as viewed for that
 *  sid (see `markSessionViewed`/`getLastViewedTurnCount` below). A session
 *  that has never completed a turn (`turn_count` 0/absent) is never unread.
 *  Pure: takes the last-viewed count as a plain argument instead of reading
 *  localStorage itself, so it's directly unit-testable. */
export function isSessionUnread(
  turnCount: number | null | undefined,
  lastViewedTurnCount: number,
): boolean {
  const count = turnCount ?? 0;
  return count > 0 && count > lastViewedTurnCount;
}

/** Minimal shape `sessionAttention`/`attentionCount` need — satisfied by both
 *  the live `SessionView` (rail rows) and `HistorySessionView` (history rows,
 *  which carry no `status`/`waiting_approval` — a stopped session can't have
 *  either, so those rows only ever resolve to `"unread"` or `null`). */
export interface AttentionInput {
  sid: string;
  status?: string | null;
  waiting_approval?: boolean;
  turn_count?: number | null;
}

/** Item 9 — derive one row's attention kind, most-urgent-first: a pending
 *  approval outranks an error, which outranks a plain unread reply (an
 *  action-needed state always beats a passive one). Pure (see
 *  `isSessionUnread`). */
export function sessionAttention(s: AttentionInput, lastViewedTurnCount: number): AttentionKind {
  if (s.waiting_approval) return "approval";
  if (s.status === "stuck") return "error";
  if (isSessionUnread(s.turn_count, lastViewedTurnCount)) return "unread";
  return null;
}

/** Display copy + chip color for one attention kind (mirrors the compact
 *  `hitl` chip already on the rail row). `null` renders nothing. */
export function attentionMeta(kind: AttentionKind): { label: string; className: string } | null {
  switch (kind) {
    case "approval":
      return { label: "等待批准", className: "bg-status-waiting/15 text-status-waiting" };
    case "error":
      return { label: "报错", className: "bg-status-error/15 text-status-error" };
    case "unread":
      return { label: "未读", className: "bg-brand-500/15 text-brand-400" };
    default:
      return null;
  }
}

/** Item 9 — the global attention count for the bottom nav (sum across every
 *  visible session with a non-null attention kind). `lastViewedTurnCount` is
 *  injected as a lookup function (not read from localStorage internally) so
 *  this stays pure/testable; production calls it with `getLastViewedTurnCount`. */
export function attentionCount(
  sessions: AttentionInput[],
  lastViewedTurnCount: (sid: string) => number,
): number {
  return sessions.filter((s) => sessionAttention(s, lastViewedTurnCount(s.sid)) !== null).length;
}

const LAST_VIEWED_TURN_COUNT_PREFIX = "ccteam.lastViewedTurnCount.";

/** Best-effort read of the last turn_count this sid was marked viewed at
 *  (item 9b unread tracking — client-side only, no server per-user
 *  read-state store). `0` (never viewed) on any failure — private-mode
 *  storage, a missing/garbled entry, or no `localStorage` at all (SSR) all
 *  degrade the same way: never throws. */
export function getLastViewedTurnCount(sid: string): number {
  try {
    const raw = localStorage.getItem(`${LAST_VIEWED_TURN_COUNT_PREFIX}${sid}`);
    const n = raw ? Number(raw) : 0;
    return Number.isFinite(n) ? n : 0;
  } catch {
    return 0;
  }
}

/** Record `sid` as viewed at `turnCount` — call when the user opens/keeps
 *  looking at that session's chat. Best-effort (see `getLastViewedTurnCount`). */
export function markSessionViewed(sid: string, turnCount: number): void {
  try {
    localStorage.setItem(`${LAST_VIEWED_TURN_COUNT_PREFIX}${sid}`, String(turnCount));
  } catch {
    // Storage unavailable — the badge just won't clear; never throw.
  }
}

/** Frontend-only soft cap on how many active sessions one user may hold at
 *  once. "Active" = the sessions in this caller's own cross-project list
 *  (`railSessions`), which the backend already ACL-scopes to the caller — so
 *  counting that list is counting the user's own live sessions. This is a UX
 *  guard (block the create + toast), NOT a security/backend limit. */
const MAX_ACTIVE_SESSIONS = 10;

/** The three bottom-nav global views — each is a full route the shell hosts
 *  in its main area (sidebar persists, Chat|终端 tabs hide). `null` = a
 *  session-chat surface (a selected session or the empty state). */
type GlobalView = "marketplace" | "status" | "hosts" | "settings" | "workflow" | null;

function globalViewFor(pathname: string): GlobalView {
  if (pathname.startsWith("/marketplace")) return "marketplace";
  if (pathname.startsWith("/status")) return "status";
  if (pathname.startsWith("/hosts")) return "hosts";
  if (pathname.startsWith("/settings")) return "settings";
  if (pathname.startsWith("/workflow")) return "workflow";
  return null;
}

// v0.8.18 柱2/UI — crumb + bottom-nav labels are now language-aware; see
// `navLabel(key, lang)` in `lib/i18n.ts` (default 中文, switchable to English
// from the avatar popover).

/** One sidebar bottom-nav row (prototype `.nav a` / `.nav a.on`). `NavLink`
 *  drives the active highlight off the current route so a deep-link lands on
 *  the right item without prop-drilling. `onNavigate` lets the mobile drawer
 *  close itself when a global page is chosen. */
function SidebarNavLink({
  to,
  icon,
  label,
  onNavigate,
  collapsed,
  title,
}: {
  to: string;
  icon: React.ReactNode;
  label: string;
  onNavigate?: () => void;
  collapsed?: boolean;
  title?: string;
}) {
  return (
    <NavLink
      to={to}
      onClick={onNavigate}
      title={title ?? label}
      className={({ isActive }) =>
        `flex items-center rounded-md text-xs transition-colors ${
          collapsed ? "justify-center h-9 w-full" : "gap-2.5 px-2.5 py-2"
        } ${
          isActive
            ? "bg-surface-800 text-brand-400"
            : "text-text-secondary hover:bg-surface-800/70 hover:text-text-primary"
        }`
      }
    >
      <span className="grid w-4 place-items-center" aria-hidden>
        {icon}
      </span>
      {collapsed || !label ? null : label}
    </NavLink>
  );
}

export default function ChatConsole() {
  const { sid: routeSid } = useParams<{ sid: string }>();
  const sid = routeSid ?? null;
  const navigate = useNavigate();
  const location = useLocation();
  // On a global route (插件市场 / Status / Settings) the shell hosts that
  // view in its main area instead of the per-session chat/terminal.
  const globalView = globalViewFor(location.pathname);
  const { settings } = useWebSettings();
  const lang = settings.language;
  // v0.8.18 档1 — Status / 主机 / Settings are operator/admin surfaces; a
  // per-user tenant only gets Marketplace + their own projects/sessions.
  const { isAdmin } = useMe();

  // The switcher's session list (gateway `s{n}`), fanned out across every
  // project from /api/v1/projects → /api/v1/projects/{slug}/sessions.
  const [railSessions, setRailSessions] = useState<RailSession[]>([]);
  // v0.8.8 bug — every config.yaml-registered project's slug (the
  // /api/v1/projects SoT), kept SEPARATELY from railSessions so a project
  // with NO session yet is still listed (sidebar group + new-session modal
  // dropdown). Without this, the project list was derived purely from
  // sessions → a freshly `ccteam init`-ed project was invisible and you
  // could never create its first session (chicken-and-egg).
  const [registeredProjects, setRegisteredProjects] = useState<string[]>([]);
  // slug → real working-tree path (from GET /api/v1/projects). Lets the
  // sidebar + new-session picker show each project's directory so a
  // collision-suffixed slug (demo / demo2 / demo3) is identifiable. A
  // sessions-only project (live but not registered) simply has no entry —
  // the path line is then omitted for it.
  const [projectPaths, setProjectPaths] = useState<Record<string, string>>({});
  // Slugs the server flagged as ORPHANED registrations (config.yaml entry whose
  // `.ccteam/state.json` is gone). Admin-only — the server only emits `broken`
  // rows to the admin. The rail marks them and shows an always-on deregister
  // action so the operator can clean them up (DELETE is registry-only).
  const [brokenProjects, setBrokenProjects] = useState<Set<string>>(new Set());
  const [railError, setRailError] = useState<string | null>(null);
  const [modalOpen, setModalOpen] = useState(false);
  // When the new-session modal is opened from a specific project's "还没有
  // session" hint, pre-select that project; `null` falls back to the active
  // session's project / the first project (the header ＋ button path).
  const [modalProject, setModalProject] = useState<string | null>(null);
  // Mobile sidebar drawer (the rail is off-canvas under `md`; a hamburger
  // toggles it). Closed by default; auto-closed on a session switch /
  // global-nav pick so the chosen surface is visible without a manual close.
  const [sidebarOpen, setSidebarOpen] = useState(false);
  // v0.8.24 A1 — desktop collapsible rail (296px expanded / 64px icons).
  // Persisted so a reload keeps the operator's preference.
  const [sideCollapsed, setSideCollapsed] = useState(() => {
    try {
      return localStorage.getItem("ccteam-side-collapsed") === "1";
    } catch {
      return false;
    }
  });
  const [sideSearch, setSideSearch] = useState("");
  const sideSearchRef = useRef<HTMLInputElement | null>(null);
  // ⌘K / Ctrl+K focuses the sidebar search (and expands if collapsed).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        if (sideCollapsed) {
          setSideCollapsed(false);
          try {
            localStorage.setItem("ccteam-side-collapsed", "0");
          } catch {
            /* ignore */
          }
        }
        window.setTimeout(() => {
          sideSearchRef.current?.focus();
          sideSearchRef.current?.select();
        }, 50);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [sideCollapsed]);
  // Deregister-project confirm dialog. `null` = closed; else the target slug,
  // whether it's an orphan, and how many live sessions get stopped (so the
  // copy is honest about the side effect).
  const [deleteTarget, setDeleteTarget] = useState<{
    slug: string;
    broken: boolean;
    sessionCount: number;
  } | null>(null);
  const [deleting, setDeleting] = useState(false);
  // v0.8.21 — per-project history expand state + loaded history sessions.
  const [expandedHistory, setExpandedHistory] = useState<Set<string>>(new Set());
  const [historyByProject, setHistoryByProject] = useState<
    Record<string, HistorySessionView[]>
  >({});
  const [historyLoading, setHistoryLoading] = useState<Set<string>>(new Set());
  // External sessions import dialog.
  const [importSlug, setImportSlug] = useState<string | null>(null);
  const [externalSessions, setExternalSessions] = useState<ExternalSessionView[]>([]);
  const [externalLoading, setExternalLoading] = useState(false);
  // v0.8.22 P1 — inline rail rename: the sid currently in edit mode (`null` =
  // none), and its draft text. Enter commits, Escape/blur cancels.
  const [renamingSid, setRenamingSid] = useState<string | null>(null);
  const [renameDraft, setRenameDraft] = useState("");
  const [importing, setImporting] = useState<string | null>(null);

  // The active session's rail entry (vendor/role/project), passed to the
  // per-sid SessionView for its crumb + terminal gating. All per-sid state
  // (transcript rows, the SSE buffer, the draft, the chat|terminal toggle,
  // HITL approval) now lives in <SessionView key={sid}>, which the shell
  // remounts on every switch — so no per-sid state survives a session change.
  const activeView = useMemo(
    () => railSessions.find((s) => s.sid === sid) ?? null,
    [railSessions, sid],
  );

  // Item 9b — the session currently open is always "read": mark it viewed at
  // its latest known `turn_count` on every switch AND every time a poll
  // (`refreshSessions`) brings a fresh count in while it's still the active
  // one — so a turn completing while the user is looking never flashes an
  // "unread" badge once the next poll lands. Client-side only (localStorage),
  // no server per-user read-state.
  useEffect(() => {
    if (!sid || !activeView) return;
    markSessionViewed(sid, activeView.turn_count ?? 0);
  }, [sid, activeView]);

  // ---- switcher list (cross-project fan-out) -----------------------------
  const refreshSessions = useCallback(async () => {
    try {
      const projects = await fetchDashboard();
      // Keep the registered-project list (config.yaml SoT) so a project with
      // no session yet still shows up — don't discard it after the fan-out.
      setRegisteredProjects(projects.map((p) => p.slug));
      // Side-table of each registered project's real working-tree path, keyed
      // by slug — the sidebar group header + the picker's `hint` line read it
      // to disambiguate demo / demo2 / demo3.
      setProjectPaths(Object.fromEntries(projects.map((p) => [p.slug, p.path])));
      // Orphaned registrations the server flagged (admin-only). Drives the
      // rail's "broken" marker + always-on deregister action.
      setBrokenProjects(new Set(projects.filter((p) => p.broken).map((p) => p.slug)));
      const lists = await Promise.all(
        projects.map((p) => listSessions(p.slug).catch(() => [] as SessionSummary[])),
      );
      setRailSessions(lists.flat());
      setRailError(null);
    } catch (e) {
      if (e instanceof Error && e.message === "UNAUTHENTICATED") {
        // global TokenEntryGate handles re-auth; don't double-report.
        return;
      }
      setRailError(e instanceof Error ? e.message : "failed to load sessions");
    }
  }, []);

  useEffect(() => {
    queueMicrotask(() => {
      void refreshSessions();
    });
  }, [refreshSessions]);

  // v0.8.8 bug5 — pick up projects/sessions registered out-of-band (a CLI
  // `ccteam init` while this tab is open) without a manual page refresh. The
  // backend list (GET /api/v1/projects → collect_projects) reads config.yaml
  // live, so a refetch when the tab regains focus surfaces them; the
  // new-session modal also refetches on open (see the ＋ button) for the
  // create-a-session flow.
  useEffect(() => {
    const onFocus = () => {
      void refreshSessions();
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [refreshSessions]);

  // ---- deregister a project (registry only; dir + .ccteam stay on disk) ---
  // DELETE /api/v1/projects/{slug} removes the slug from config.yaml and stops
  // its live sessions server-side. If we're currently viewing a session in the
  // removed project, leave for the shell root so we don't sit on a gone session.
  const confirmDelete = useCallback(async () => {
    if (!deleteTarget || deleting) return;
    const targetSlug = deleteTarget.slug;
    const wasActive = activeView?.project === targetSlug;
    setDeleting(true);
    try {
      await deleteProject(targetSlug);
      toastBus.handler?.info(tr(lang, `已解除注册 ${targetSlug}`, `Deregistered ${targetSlug}`));
      setDeleteTarget(null);
      if (wasActive) navigate("/");
      await refreshSessions();
    } catch (e) {
      if (e instanceof Error && e.message === "UNAUTHENTICATED") return;
      toastBus.handler?.error(
        e instanceof Error ? e.message : tr(lang, "解除注册失败", "Deregister failed"),
      );
    } finally {
      setDeleting(false);
    }
  }, [
    deleteTarget,
    deleting,
    activeView,
    navigate,
    refreshSessions,
    lang,
    setDeleteTarget,
    setDeleting,
  ]);

  // ---- create a new session (optionally a brand-new project first) -------
  // `newProjectPath` present ⇒ B2: POST /projects to scaffold+register `slug`
  // first, then feed the returned slug into the existing create-session flow.
  // Returns `true` on success so the modal can close itself; on failure it
  // resolves `false` so the modal stays open (input preserved) and the error
  // is surfaced via toast (human-readable) — NOT pushed into the transcript.
  const createSession = useCallback(
    async (
      slug: string,
      role: string,
      vendor: string,
      permissionMode: "skip" | "hitl",
      protocol: "stream-json" | "terminal" | "acp",
      newProjectPath?: string,
      host?: string,
    ): Promise<boolean> => {
      // Frontend-only cap: block a new session once the caller already holds
      // MAX_ACTIVE_SESSIONS. `railSessions` is the caller's own (ACL-scoped)
      // cross-project session list, so its length == the user's active count.
      // Block BEFORE any create/spawn API call and surface a bilingual toast;
      // return false so the modal stays open (input preserved).
      if (railSessions.length >= MAX_ACTIVE_SESSIONS) {
        toastBus.handler?.error(
          tr(
            lang,
            "最多 10 个活跃 session,请先结束其他 session",
            "Max 10 active sessions — please end others first",
          ),
        );
        return false;
      }
      try {
        // B2: create the project first when a path was supplied.
        let targetSlug = slug;
        if (newProjectPath !== undefined) {
          const created = await apiCreateProject(slug, newProjectPath);
          targetSlug = created.slug;
        }
        const { sid: newSid, model_warning: modelWarning } = await apiCreateSession(targetSlug, {
          role,
          vendor,
          permission_mode: permissionMode,
          protocol,
          host: host && host !== "local" ? host : undefined,
        });
        if (modelWarning) toastBus.handler?.info(modelWarning);
        await refreshSessions();
        navigate(`/chat/s/${encodeURIComponent(newSid)}`);
        return true;
      } catch (e) {
        if (e instanceof Error && e.message === "UNAUTHENTICATED") {
          // global TokenEntryGate handles re-auth; don't toast/transcript.
          return false;
        }
        // Surface as a human-readable toast (e.g. "项目 demo 已存在"); never
        // leak a raw HTTP/stack into the transcript stream.
        const detail = e instanceof Error ? e.message : "unknown";
        toastBus.handler?.error(
          newProjectPath !== undefined ? `新建项目失败: ${detail}` : `新建 session 失败: ${detail}`,
        );
        return false;
      }
    },
    [refreshSessions, navigate, railSessions, lang],
  );

  // The project list = ALL registered projects (config.yaml SoT) ∪ the
  // projects that have a live session. The union (not sessions alone) is THE
  // fix so a session-less, freshly-registered project is still listed.
  const projects = useMemo(
    () => mergeProjectSlugs(registeredProjects, railSessions),
    [registeredProjects, railSessions],
  );
  // v0.8.24 A1 — filter sessions/projects by the sidebar search box.
  const q = sideSearch.trim().toLowerCase();
  const filteredRailSessions = useMemo(() => {
    if (!q) return railSessions;
    return railSessions.filter((s) => {
      const hay = `${s.sid} ${s.project} ${s.role} ${s.vendor} ${s.title ?? ""}`.toLowerCase();
      return hay.includes(q);
    });
  }, [railSessions, q]);
  const filteredProjects = useMemo(() => {
    if (!q) return projects;
    const withHits = new Set(filteredRailSessions.map((s) => s.project));
    return projects.filter((p) => p.toLowerCase().includes(q) || withHits.has(p));
  }, [projects, filteredRailSessions, q]);
  const roleOptions = useMemo(() => {
    const seen = new Set(ROLE_SUGGESTIONS);
    railSessions.forEach((s) => s.role && seen.add(s.role));
    return Array.from(seen);
  }, [railSessions]);
  // Frontend-only cap (see MAX_ACTIVE_SESSIONS) — disable the header "＋ 新建"
  // entry at the limit so the affordance reflects the block; the create
  // funnel (`createSession`) still hard-guards + toasts as the source of truth.
  const atSessionCap = railSessions.length >= MAX_ACTIVE_SESSIONS;
  // Item 9 — global attention count (waiting-approval / error / unread)
  // across every session this caller can see, surfaced as a badge near the
  // top of the nav so it's visible whether or not the sidebar/drawer is open.
  const totalAttention = useMemo(
    () => attentionCount(railSessions, getLastViewedTurnCount),
    [railSessions],
  );

  const switchTo = useCallback(
    (s: RailSession) => {
      // Navigate only — the remounted <SessionView key={sid}> resets its own
      // view to "chat" (a fresh instance), so the shell no longer tracks it.
      navigate(`/chat/s/${encodeURIComponent(s.sid)}`);
      setSidebarOpen(false);
    },
    [navigate],
  );

  const toggleSideCollapsed = () => {
    setSideCollapsed((c) => {
      const next = !c;
      try {
        localStorage.setItem("ccteam-side-collapsed", next ? "1" : "0");
      } catch {
        /* ignore */
      }
      return next;
    });
  };

  return (
    <div className="h-full min-h-0 flex flex-col bg-surface-900 text-text-primary">
      {/* v0.8.24 A1 — no top bar. Mobile hamburger floats over the main area. */}
      <button
        type="button"
        onClick={() => setSidebarOpen(true)}
        aria-label="打开会话列表"
        className="md:hidden fixed top-3 left-3 z-20 relative h-9 w-9 grid place-items-center rounded-lg border border-surface-700/50 bg-surface-950/90 text-text-secondary shadow-sm hover:text-text-primary"
      >
        <Menu className="h-4 w-4" />
        {totalAttention > 0 ? (
          <span
            className="absolute -top-1 -right-1 min-w-[14px] h-[14px] px-[3px] rounded-full bg-status-error text-surface-950 text-[9px] font-mono leading-[14px] text-center"
            title={`${totalAttention} 个 session 需要关注`}
          >
            {totalAttention > 9 ? "9+" : totalAttention}
          </span>
        ) : null}
      </button>

      <div className="flex flex-1 min-h-0">
        {sidebarOpen ? (
          <button
            type="button"
            aria-label="关闭会话列表"
            onClick={() => setSidebarOpen(false)}
            className="md:hidden fixed inset-0 z-30 bg-black/50"
          />
        ) : null}
        {/* left rail — logo + collapse + ⌘K search + 新建/工作流 + sessions + 设置 */}
        <aside
          className={`${
            sideCollapsed ? "md:w-16" : "md:w-[296px]"
          } w-[296px] shrink-0 border-r border-surface-700/60 flex flex-col bg-surface-950 transition-[width] duration-150 ${
            sidebarOpen ? "fixed inset-y-0 left-0 z-40 md:static" : "hidden md:flex"
          }`}
        >
          {/* logo row */}
          <div
            className={`h-12 shrink-0 px-3 flex items-center border-b border-surface-700/30 ${
              sideCollapsed ? "justify-center" : "justify-between gap-2"
            }`}
          >
            {sideCollapsed ? (
              <button
                type="button"
                onClick={toggleSideCollapsed}
                className="h-8 w-8 grid place-items-center rounded-md text-brand-400 hover:bg-surface-800"
                title={tr(lang, "展开侧栏", "Expand sidebar")}
                aria-label={tr(lang, "展开侧栏", "Expand sidebar")}
              >
                <MessageSquare className="h-4 w-4" />
              </button>
            ) : (
              <>
                <div className="flex items-center gap-2 min-w-0">
                  <MessageSquare className="h-4 w-4 text-brand-400 shrink-0" />
                  <span className="text-sm font-semibold truncate">
                    ccteam <span className="text-brand-400">chat</span>
                  </span>
                  {totalAttention > 0 ? (
                    <span
                      className="shrink-0 min-w-[16px] h-4 px-1 rounded-full bg-status-error/15 text-status-error text-[10px] font-mono leading-4 text-center"
                      title={`${totalAttention} 个 session 需要关注`}
                    >
                      {totalAttention}
                    </span>
                  ) : null}
                </div>
                <div className="flex items-center gap-0.5">
                  <button
                    type="button"
                    onClick={toggleSideCollapsed}
                    className="hidden md:grid h-7 w-7 place-items-center rounded-md text-text-muted hover:text-text-primary hover:bg-surface-800"
                    title={tr(lang, "折叠侧栏", "Collapse sidebar")}
                    aria-label={tr(lang, "折叠侧栏", "Collapse sidebar")}
                  >
                    <ChevronLeft className="h-4 w-4" />
                  </button>
                  <button
                    type="button"
                    onClick={() => setSidebarOpen(false)}
                    aria-label="关闭"
                    className="md:hidden h-7 w-7 grid place-items-center rounded-md text-text-muted hover:text-text-primary hover:bg-surface-800"
                  >
                    <X className="h-3.5 w-3.5" />
                  </button>
                </div>
              </>
            )}
          </div>

          {/* search + primary actions (hidden labels when collapsed) */}
          <div className={`shrink-0 border-b border-surface-700/30 ${sideCollapsed ? "p-1.5" : "p-2"} space-y-1.5`}>
            {sideCollapsed ? (
              <button
                type="button"
                onClick={() => {
                  setSideCollapsed(false);
                  try {
                    localStorage.setItem("ccteam-side-collapsed", "0");
                  } catch {
                    /* ignore */
                  }
                  window.setTimeout(() => sideSearchRef.current?.focus(), 50);
                }}
                className="w-full h-9 grid place-items-center rounded-md text-text-secondary hover:bg-surface-800"
                title={`${tr(lang, "搜索", "Search")} (⌘K)`}
              >
                <Search className="h-4 w-4" />
              </button>
            ) : (
              <div className="relative">
                <Search className="absolute left-2.5 top-1/2 -translate-y-1/2 h-3.5 w-3.5 text-text-muted pointer-events-none" />
                <input
                  id="side-search"
                  ref={sideSearchRef}
                  value={sideSearch}
                  onChange={(e) => setSideSearch(e.target.value)}
                  placeholder={tr(lang, "搜索会话 / 项目", "Search sessions / projects")}
                  className="w-full h-9 pl-8 pr-12 rounded-md bg-surface-900 border border-surface-700/40 text-xs text-text-primary placeholder:text-text-muted outline-none focus:ring-1 focus:ring-brand-500/50"
                />
                <kbd className="absolute right-2 top-1/2 -translate-y-1/2 text-[10px] font-mono text-text-muted border border-surface-700/50 rounded px-1 py-0.5">
                  ⌘K
                </kbd>
              </div>
            )}
            <button
              type="button"
              disabled={atSessionCap}
              onClick={() => {
                void refreshSessions();
                // Home = empty shell path; modal still creates immediately.
                navigate("/chat");
                setModalProject(null);
                setModalOpen(true);
                setSidebarOpen(false);
              }}
              className={`w-full rounded-md bg-brand-500/90 text-surface-950 hover:bg-brand-400 text-xs font-medium flex items-center gap-2 disabled:opacity-50 disabled:cursor-not-allowed ${
                sideCollapsed ? "h-9 justify-center" : "h-9 px-3"
              }`}
              title={
                atSessionCap
                  ? tr(
                      lang,
                      "最多 10 个活跃 session,请先结束其他 session",
                      "Max 10 active sessions — please end others first",
                    )
                  : tr(lang, "新建会话", "New session")
              }
            >
              <Plus className="h-3.5 w-3.5 shrink-0" />
              {sideCollapsed ? null : <span>{tr(lang, "新建会话", "New session")}</span>}
            </button>
            <SidebarNavLink
              to="/workflow"
              icon={<LayoutGrid className="h-4 w-4" />}
              label={sideCollapsed ? "" : tr(lang, "工作流", "Workflow")}
              onNavigate={() => setSidebarOpen(false)}
              collapsed={sideCollapsed}
              title={tr(lang, "工作流", "Workflow")}
            />
          </div>

          <div className="flex-1 overflow-y-auto p-2 space-y-2">
            {sideCollapsed ? (
              <div className="flex flex-col items-center gap-1 py-1">
                {filteredRailSessions.slice(0, 12).map((s) => (
                  <button
                    key={s.sid}
                    type="button"
                    onClick={() => switchTo(s)}
                    title={`${railSessionLabel(s)} · ${s.sid}`}
                    className={`h-9 w-9 grid place-items-center rounded-md text-[10px] font-mono ${
                      s.sid === sid
                        ? "bg-surface-700 text-text-primary"
                        : "text-text-secondary hover:bg-surface-800/70"
                    } ${vendorBadgeClass(s.vendor)}`}
                  >
                    {(s.vendor || "?").slice(0, 1).toUpperCase()}
                  </button>
                ))}
                <button
                  type="button"
                  onClick={toggleSideCollapsed}
                  className="h-8 w-8 grid place-items-center rounded-md text-text-muted hover:bg-surface-800"
                  title={tr(lang, "展开侧栏", "Expand sidebar")}
                >
                  <ChevronRight className="h-4 w-4" />
                </button>
              </div>
            ) : (
              <>
            {filteredProjects.map((project) => {
              const items = filteredRailSessions.filter((s) => s.project === project);
              const projectPath = projectPaths[project];
              const broken = brokenProjects.has(project);
              return (
                <div key={project} className="group/proj">
                  <div
                    className="px-1.5 py-1 flex items-start justify-between gap-1"
                    title={projectPath ? `${project} — ${projectPath}` : project}
                  >
                    <div className="min-w-0 flex-1">
                      <div className="flex items-center gap-1">
                        {broken ? (
                          <span
                            className="shrink-0 text-[9px] font-mono px-1 rounded bg-status-error/15 text-status-error"
                            title="注册损坏:.ccteam/state.json 丢失,解除注册以清理"
                          >
                            ⚠
                          </span>
                        ) : null}
                        <div
                          className={`text-[11px] font-mono font-medium truncate ${
                            broken ? "text-text-muted" : "text-text-primary"
                          }`}
                        >
                          {project}
                        </div>
                      </div>
                      {projectPath ? (
                        <div className="text-[10px] font-mono text-text-muted truncate">
                          {projectPath}
                        </div>
                      ) : null}
                    </div>
                    {/* deregister — registry only; hover-revealed for healthy
                        projects, always-on for a broken one (it needs cleanup). */}
                    <button
                      type="button"
                      onClick={() =>
                        setDeleteTarget({ slug: project, broken, sessionCount: items.length })
                      }
                      title={tr(lang, "解除注册", "Deregister")}
                      aria-label={tr(lang, `解除注册 ${project}`, `Deregister ${project}`)}
                      className={`shrink-0 h-5 w-5 grid place-items-center rounded text-text-muted hover:text-status-error hover:bg-surface-800 transition-opacity focus-visible:opacity-100 ${
                        broken ? "opacity-100" : "opacity-0 group-hover/proj:opacity-100"
                      }`}
                    >
                      <Trash2 className="h-3 w-3" />
                    </button>
                  </div>
                  <div className="space-y-0.5">
                    {items.map((s) => {
                      const active = s.sid === sid;
                      const editing = renamingSid === s.sid;
                      // Item 9/10 — attention badge + real-status dot. The
                      // dot no longer encodes "selected" (the row background
                      // above already does that); it encodes the session's
                      // actual liveness/business (`s.status`).
                      const attention = sessionAttention(s, getLastViewedTurnCount(s.sid));
                      const attentionInfo = attentionMeta(attention);
                      const commitRename = () => {
                        const title = renameDraft.trim();
                        setRenamingSid(null);
                        if (!title || title === (s.title ?? "")) return;
                        renameSession(s.sid, title)
                          .then(() => refreshSessions())
                          .catch((e) =>
                            toastBus.handler?.error(`Rename failed: ${String(e)}`),
                          );
                      };
                      return (
                        <div
                          key={s.sid}
                          className={`group/sess w-full flex items-center gap-1 rounded-md text-xs ${
                            active
                              ? "bg-surface-700 text-text-primary"
                              : "text-text-secondary hover:bg-surface-800/70"
                          }`}
                        >
                          {editing ? (
                            <input
                              autoFocus
                              value={renameDraft}
                              onChange={(e) => setRenameDraft(e.target.value)}
                              onKeyDown={(e) => {
                                if (e.key === "Enter") commitRename();
                                if (e.key === "Escape") {
                                  // Reset the draft to the pristine value FIRST:
                                  // removing this input from the DOM (via the
                                  // renamingSid state flip below) may itself
                                  // trigger a native blur — resetting the draft
                                  // makes `commitRename`'s no-op-when-unchanged
                                  // guard cover that case too, so Escape can
                                  // never save a discarded edit either way.
                                  setRenameDraft(s.title || s.role || "");
                                  setRenamingSid(null);
                                }
                              }}
                              onBlur={commitRename}
                              className="flex-1 min-w-0 mx-1.5 my-1 px-1.5 py-0.5 rounded bg-surface-900 text-text-primary text-xs outline-none ring-1 ring-brand-500/60"
                            />
                          ) : (
                            <button
                              type="button"
                              onClick={() => switchTo(s)}
                              className="flex-1 min-w-0 text-left px-2 py-1.5 rounded-md flex items-center gap-2"
                            >
                              <span
                                title={`状态: ${s.status || "live"}`}
                                className={`h-1.5 w-1.5 rounded-full shrink-0 ${sessionDotClass(s.status)}`}
                              />
                              <span
                                className={`font-mono px-1 rounded text-[10px] ${vendorBadgeClass(s.vendor)}`}
                              >
                                {s.vendor}
                              </span>
                              <span className="truncate flex-1">{railSessionLabel(s)}</span>
                              {s.permission_mode === "hitl" ? (
                                <span
                                  className="font-mono text-[9px] text-brand-400/90"
                                  title="HITL: 非 allowlist 工具需批准"
                                >
                                  hitl
                                </span>
                              ) : null}
                              {attentionInfo ? (
                                <span
                                  className={`shrink-0 font-mono text-[9px] px-1 rounded ${attentionInfo.className}`}
                                  title={attentionInfo.label}
                                >
                                  {attentionInfo.label}
                                </span>
                              ) : null}
                              <span className="text-text-muted font-mono">{s.sid}</span>
                            </button>
                          )}
                          {editing ? null : (
                            <button
                              type="button"
                              onClick={(e) => {
                                e.stopPropagation();
                                setRenamingSid(s.sid);
                                setRenameDraft(s.title || s.role || "");
                              }}
                              title={tr(lang, "重命名", "Rename")}
                              aria-label={tr(lang, `重命名 ${s.sid}`, `Rename ${s.sid}`)}
                              className="shrink-0 h-5 w-5 mr-1 grid place-items-center rounded text-text-muted opacity-0 group-hover/sess:opacity-100 hover:text-brand-400 hover:bg-surface-800 transition-opacity focus-visible:opacity-100"
                            >
                              <Pencil className="h-3 w-3" />
                            </button>
                          )}
                        </div>
                      );
                    })}
                    {items.length === 0 ? (
                      // A registered project with no session yet (e.g. just
                      // `ccteam init`-ed). Render an inviting hint that opens
                      // the new-session modal pre-selected to this project so
                      // the user can create its FIRST session in one click.
                      <button
                        type="button"
                        onClick={() => {
                          void refreshSessions();
                          setModalProject(project);
                          setModalOpen(true);
                        }}
                        className="w-full text-left px-2 py-1 rounded-md flex items-center gap-1 text-[10px] text-text-muted hover:text-brand-400 hover:bg-surface-800/70"
                        title={`为 ${project} 创建第一个 session`}
                      >
                        <Plus className="h-3 w-3" /> 还没有 session — 新建
                      </button>
                    ) : null}
                    {/* v0.8.22 P0-3/P0-4 review — history expand/collapse is a
                        tenant-visible feature (a stopped session's owner can
                        see + resume it), NOT an admin-only beta surface: the
                        backend already scopes `.../sessions/history` and
                        `.../resume` by project ownership (`can_see_project`
                        via the shared `project_acl_layer`/`gate_sid` chokepoint
                        — see `sessions_api.rs`), so dropping the UI gate here
                        exposes nothing a tenant couldn't already reach. */}
                    <HistorySection
                      project={project}
                      expanded={expandedHistory.has(project)}
                      loading={historyLoading.has(project)}
                      history={historyByProject[project] ?? []}
                      activeSid={sid}
                      onToggle={() => {
                        const next = new Set(expandedHistory);
                        if (next.has(project)) {
                          next.delete(project);
                          setExpandedHistory(next);
                        } else {
                          next.add(project);
                          setExpandedHistory(next);
                          if (!historyByProject[project]) {
                            setHistoryLoading((s) => new Set(s).add(project));
                            listHistorySessions(project)
                              .then((rows) => {
                                setHistoryByProject((prev) => ({
                                  ...prev,
                                  [project]: rows,
                                }));
                              })
                              .catch(() => {})
                              .finally(() => {
                                setHistoryLoading((s) => {
                                  const next2 = new Set(s);
                                  next2.delete(project);
                                  return next2;
                                });
                              });
                          }
                        }
                      }}
                      onResume={(hsid) => {
                        resumeSession(project, hsid)
                          .then(({ sid: newSid }) => {
                            void refreshSessions();
                            navigate(`/chat/s/${encodeURIComponent(newSid)}`);
                            setSidebarOpen(false);
                          })
                          .catch((e) =>
                            toastBus.handler?.error(`Resume failed: ${String(e)}`),
                          );
                      }}
                      onOpenImport={() => {
                        setImportSlug(project);
                        setExternalSessions([]);
                        setExternalLoading(true);
                        listExternalSessions(project)
                          .then(setExternalSessions)
                          .catch(() => {})
                          .finally(() => setExternalLoading(false));
                      }}
                    />
                  </div>
                </div>
              );
            })}
            {filteredProjects.length === 0 ? (
              <div className="px-2 py-3 text-xs text-text-secondary leading-5">
                {railError
                  ? `加载失败: ${railError}`
                  : q
                    ? tr(lang, "没有匹配的会话", "No matching sessions")
                    : tr(lang, "还没有 session。点「新建会话」创建。", "No sessions yet — create one.")}
              </div>
            ) : null}
              </>
            )}
          </div>

          {/* bottom: CostPill + Settings (+ admin Status/主机 until Settings tabs land) */}
          <nav className="border-t border-surface-700/40 p-2 space-y-1">
            {sideCollapsed ? null : (
              <div className="px-1 pb-1">
                <CostPill />
              </div>
            )}
            <div className="space-y-0.5">
              <SidebarNavLink
                to="/marketplace"
                icon={<Puzzle className="h-4 w-4" />}
                label={sideCollapsed ? "" : navLabel("marketplace", lang)}
                onNavigate={() => setSidebarOpen(false)}
                collapsed={sideCollapsed}
                title={navLabel("marketplace", lang)}
              />
              {isAdmin && (
                <>
                  <SidebarNavLink
                    to="/status"
                    icon={<Activity className="h-4 w-4" />}
                    label={sideCollapsed ? "" : navLabel("status", lang)}
                    onNavigate={() => setSidebarOpen(false)}
                    collapsed={sideCollapsed}
                    title={navLabel("status", lang)}
                  />
                  <SidebarNavLink
                    to="/hosts"
                    icon={<Server className="h-4 w-4" />}
                    label={sideCollapsed ? "" : navLabel("hosts", lang)}
                    onNavigate={() => setSidebarOpen(false)}
                    collapsed={sideCollapsed}
                    title={navLabel("hosts", lang)}
                  />
                </>
              )}
              <SidebarNavLink
                to="/settings"
                icon={<Settings className="h-4 w-4" />}
                label={sideCollapsed ? "" : navLabel("settings", lang)}
                onNavigate={() => setSidebarOpen(false)}
                collapsed={sideCollapsed}
                title={navLabel("settings", lang)}
              />
            </div>
            {sideCollapsed ? null : (
              <div className="pt-1 border-t border-surface-700/30">
                <AvatarMenu />
              </div>
            )}
          </nav>
        </aside>

        {/* main area: a global view (插件市场/Status/Settings), else the per-sid
            SessionView (keyed by sid → remounts on every switch, so all per-sid
            state resets atomically), else the no-session empty state. */}
        <main className="flex-1 min-w-0 min-h-0 flex flex-col">
          {globalView ? (
            <>
              <div className="h-10 shrink-0 px-4 flex items-center gap-3 border-b border-surface-700/30">
                <span className="text-xs font-semibold text-text-primary">
                  {globalView === "workflow"
                    ? tr(lang, "工作流", "Workflow")
                    : navLabel(globalView, lang)}
                </span>
              </div>
              <div className="flex-1 min-h-0 overflow-y-auto">
                {globalView === "settings" ? (
                  <SettingsPage />
                ) : globalView === "marketplace" ? (
                  <MarketplaceView />
                ) : globalView === "hosts" ? (
                  <HostsView />
                ) : globalView === "workflow" ? (
                  <WorkflowView />
                ) : (
                  <StatusView rail={railSessions} />
                )}
              </div>
            </>
          ) : sid ? (
            // KEY={sid}: a fresh SessionView mounts on every session switch, so
            // its per-sid state (rows / SSE buffer / draft / chat|terminal view
            // / scroll / HITL) resets atomically — no state survives a switch.
            <SessionView key={sid} sid={sid} session={activeView} onSessionChanged={refreshSessions} />
          ) : (
            /* v0.8.24 A1 Home — empty landing; first message path still uses modal for now. */
            <div className="flex-1 min-h-0 flex flex-col items-center justify-center px-6 text-center">
              <h1 className="text-2xl font-semibold text-text-primary tracking-tight">
                {tr(lang, "开工吧!", "Let's go!")}
              </h1>
              <p className="mt-2 max-w-md text-sm text-text-secondary leading-6">
                {tr(
                  lang,
                  "从左侧选会话，或点「新建会话」开始 —— 会话在创建时绑定项目与模型。",
                  "Pick a session on the left, or New session — project and model bind at create time.",
                )}
              </p>
              <button
                type="button"
                disabled={atSessionCap}
                onClick={() => {
                  void refreshSessions();
                  setModalProject(null);
                  setModalOpen(true);
                }}
                className="mt-6 h-10 px-5 rounded-lg bg-brand-500 text-surface-950 text-sm font-medium hover:bg-brand-400 disabled:opacity-50"
              >
                {tr(lang, "新建会话", "New session")}
              </button>
              <p className="mt-4 text-[11px] text-text-muted font-mono">
                {tr(lang, "提示: ⌘K 搜索会话", "Tip: ⌘K to search sessions")}
              </p>
            </div>
          )}
        </main>
      </div>

      {modalOpen ? (
        <NewSessionModal
          projects={projects}
          projectPaths={projectPaths}
          fallbackRoles={roleOptions}
          defaultProject={modalProject ?? activeView?.project ?? projects[0] ?? ""}
          isAdmin={isAdmin}
          onCancel={() => {
            setModalOpen(false);
            setModalProject(null);
          }}
          onCreate={createSession}
        />
      ) : null}

      {/* Deregister-project confirm — registry-only, honest about the boundary
          (the project dir + its .ccteam are NOT deleted) and the side effect
          (live sessions stop). Broken (orphan) targets get a tailored line. */}
      {deleteTarget ? (
        <div
          className="fixed inset-0 z-50 grid place-items-center bg-black/50 p-4 animate-fade-in"
          role="dialog"
          aria-modal="true"
          aria-label={tr(lang, "解除注册项目", "Deregister project")}
          onClick={() => {
            if (!deleting) setDeleteTarget(null);
          }}
        >
          <div
            className="w-full max-w-[400px] rounded-lg border border-surface-700 bg-surface-900 p-4 shadow-xl animate-slide-up"
            onClick={(e) => e.stopPropagation()}
          >
            <h2 className="text-sm font-semibold text-text-primary">
              {tr(lang, "解除注册「", "Deregister ")}
              <span className="font-mono">{deleteTarget.slug}</span>
              {tr(lang, "」?", "?")}
            </h2>
            <p className="mt-2 text-xs leading-5 text-text-secondary">
              {tr(
                lang,
                "仅从 ccteam 注册表移除。",
                "Removes it from ccteam's registry only. ",
              )}
              <span className="text-text-primary">
                {tr(
                  lang,
                  "项目目录和其中的 .ccteam 都不会被删除",
                  "the project directory and its .ccteam are NOT deleted",
                )}
              </span>
              {tr(lang, ",随时可 ", " — re-add anytime with ")}
              <span className="font-mono">ccteam init</span>
              {tr(lang, " 重新注册。", ".")}
              {deleteTarget.broken
                ? tr(
                    lang,
                    " (该注册已损坏:state.json 丢失。)",
                    " (This registration is broken: its state.json is missing.)",
                  )
                : deleteTarget.sessionCount > 0
                  ? tr(
                      lang,
                      ` 该项目下进行中的 ${deleteTarget.sessionCount} 个 session 会被停止。`,
                      ` Its ${deleteTarget.sessionCount} live session(s) will be stopped.`,
                    )
                  : ""}
            </p>
            <div className="mt-4 flex justify-end gap-2">
              <button
                type="button"
                onClick={() => setDeleteTarget(null)}
                disabled={deleting}
                className="h-8 px-3 rounded-md text-xs text-text-secondary hover:bg-surface-800 hover:text-text-primary disabled:opacity-50"
              >
                {tr(lang, "取消", "Cancel")}
              </button>
              <button
                type="button"
                onClick={() => void confirmDelete()}
                disabled={deleting}
                className="h-8 px-3 rounded-md text-xs bg-status-error/90 text-surface-950 hover:bg-status-error disabled:opacity-50 disabled:cursor-not-allowed"
              >
                {deleting
                  ? tr(lang, "解除中…", "Removing…")
                  : tr(lang, "解除注册", "Deregister")}
              </button>
            </div>
          </div>
        </div>
      ) : null}

      {/* v0.8.21 — Import external session dialog */}
      {importSlug ? (
        <div
          className="fixed inset-0 z-50 grid place-items-center bg-black/50 p-4 animate-fade-in"
          role="dialog"
          aria-modal="true"
          aria-label="导入历史会话"
          onClick={() => setImportSlug(null)}
        >
          <div
            className="w-full max-w-[480px] rounded-lg border border-surface-700 bg-surface-900 p-4 shadow-xl animate-slide-up"
            onClick={(e) => e.stopPropagation()}
          >
            <h2 className="text-sm font-semibold text-text-primary mb-3">
              导入历史会话 —{" "}
              <span className="font-mono text-brand-400">{importSlug}</span>
            </h2>
            {externalLoading ? (
              <p className="text-xs text-text-muted">扫描中…</p>
            ) : externalSessions.length === 0 ? (
              <p className="text-xs text-text-muted">未发现可收编的外部 Claude 会话。</p>
            ) : (
              <div className="space-y-1 max-h-64 overflow-y-auto">
                {externalSessions.map((es) => (
                  <div
                    key={es.vendor_uuid}
                    className="flex items-center gap-2 px-2 py-1.5 rounded-md hover:bg-surface-800"
                  >
                    <div className="flex-1 min-w-0">
                      <div className="text-xs truncate text-text-primary">
                        {es.title || es.vendor_uuid.slice(0, 8) + "…"}
                      </div>
                      <div className="text-[10px] font-mono text-text-muted truncate">
                        {es.last_active ? es.last_active.slice(0, 16).replace("T", " ") : ""}
                      </div>
                    </div>
                    <button
                      type="button"
                      disabled={importing === es.vendor_uuid}
                      onClick={() => {
                        setImporting(es.vendor_uuid);
                        importExternalSession(importSlug, es.vendor_uuid)
                          .then(({ sid: newSid }) => {
                            void refreshSessions();
                            setImportSlug(null);
                            navigate(`/chat/s/${encodeURIComponent(newSid)}`);
                            setSidebarOpen(false);
                          })
                          .catch((e) => toastBus.handler?.error(`Import failed: ${String(e)}`))
                          .finally(() => setImporting(null));
                      }}
                      className="shrink-0 px-2 py-1 rounded text-[10px] bg-brand-600/80 hover:bg-brand-500 text-white disabled:opacity-50"
                    >
                      {importing === es.vendor_uuid ? "收编中…" : "收编"}
                    </button>
                  </div>
                ))}
              </div>
            )}
            <div className="mt-3 flex justify-end">
              <button
                type="button"
                onClick={() => setImportSlug(null)}
                className="px-3 py-1.5 rounded text-xs text-text-muted hover:text-text-primary hover:bg-surface-800"
              >
                关闭
              </button>
            </div>
          </div>
        </div>
      ) : null}
    </div>
  );
}

// New-session modal: pick a project (an existing one, OR scaffold a brand-new
// one inline — B2), code agent, role, and permission mode.
//   - B2: selecting the "＋ 新建项目…" sentinel reveals slug(name)+path fields;
//     submit first POSTs /api/v1/projects (createProject) then feeds the
//     returned slug into the existing create-session flow. Slug/path are
//     validated client-side mirroring the backend (validate_slug_format +
//     expand_project_path) so the user gets inline feedback before the round
//     trip.
//   - B3 / BUG-4: the role field is now a real dropdown sourced from
//     `GET /api/v1/projects/{slug}/roles` (the project's `.claude/agents/`)
//     for an EXISTING project, with the static ROLE_SUGGESTIONS/DEFAULT_ROLE
//     as the fallback/seed (FIX-2). A brand-new project has no roles yet, so
//     that branch uses the static fallback and does NOT fetch (would 404).
//
//   - F2-web: the role dropdown offers an explicit "(无角色 / 裸 claude)" choice
//     (the ROLELESS sentinel). Picking it sends an empty role (a bare-claude
//     session that self-reads the project CLAUDE.md); `resolveRole`
//     (chatDefaults) maps the sentinel → "".
//   - v0.8.20 F4: the modal now DEFAULTS to ROLELESS (no role unless one is
//     deliberately picked), and the role picker itself is admin-only — a tenant
//     always creates a roleless session. The terminal/rmux runtimes are
//     likewise admin-only; tenants see only claude/codex on stream-json.

/** Sentinel project value selecting the "create a new project" branch. */
const NEW_PROJECT = "__new";

/** Mirror of the backend slug grammar (`ccteam_core::validate_slug_format`):
 *  `[a-z0-9-]+`, ≤60, no leading/trailing `-`. */
function slugError(slug: string): string | null {
  if (slug.length === 0) return "项目名不能为空";
  if (slug.length > 60) return "项目名最多 60 字符";
  if (!/^[a-z0-9-]+$/.test(slug)) return "只允许小写字母、数字、连字符";
  if (slug.startsWith("-") || slug.endsWith("-")) return "不能以连字符开头或结尾";
  return null;
}

/** Mirror of the backend path rule (`expand_project_path`): non-empty after
 *  trim and starting with `/` or `~`. */
function pathError(path: string): string | null {
  const p = path.trim();
  if (p.length === 0) return "路径不能为空";
  if (!p.startsWith("/") && !p.startsWith("~")) return "路径需以 / 或 ~ 开头";
  return null;
}

type RoleFetchState =
  | { kind: "idle" }
  | { kind: "loading" }
  | { kind: "ready"; roles: RoleSummary[] }
  | { kind: "error" };

const RUNTIME_OPTIONS = [
  {
    id: "claude-stream-json",
    label: "Claude · stream-json",
    hint: "轻量聊天",
    vendor: "claude",
    protocol: "stream-json",
  },
  {
    id: "claude-terminal",
    label: "Claude · terminal",
    hint: "终端镜像 / 截图",
    vendor: "claude",
    protocol: "terminal",
  },
  {
    id: "codex-app-server",
    label: "Codex · app-server",
    hint: "Codex JSON-RPC",
    vendor: "codex",
    protocol: "stream-json",
  },
  {
    id: "codex-terminal",
    label: "Codex · terminal",
    hint: "终端模式",
    vendor: "codex",
    protocol: "terminal",
  },
  {
    id: "grok-acp",
    label: "Grok · ACP",
    hint: "Grok Build agent stdio",
    vendor: "grok",
    protocol: "acp",
  },
  {
    id: "opencode-acp",
    label: "OpenCode · ACP",
    hint: "OpenCode agent stdio",
    vendor: "opencode",
    protocol: "acp",
  },
] as const satisfies readonly {
  id: string;
  label: string;
  hint: string;
  vendor: "claude" | "codex" | "grok" | "opencode";
  protocol: "stream-json" | "terminal" | "acp";
}[];

/** Effort levels offered at spawn (advisory UI; OpenCode set_config later). */
const EFFORT_OPTIONS = [
  { id: "low", label: "低" },
  { id: "medium", label: "中" },
  { id: "high", label: "高" },
  { id: "max", label: "极高" },
] as const;

/** Per-vendor badge classes (4-way; never collapse opencode into codex/grok). */
function vendorBadgeClass(vendor: string): string {
  if (vendor === "claude") return "bg-vendor-claude/15 text-vendor-claude";
  if (vendor === "codex") return "bg-vendor-codex/15 text-vendor-codex";
  if (vendor === "grok") return "bg-vendor-grok/15 text-vendor-grok";
  if (vendor === "opencode") return "bg-vendor-opencode/15 text-vendor-opencode";
  return "bg-surface-700 text-text-secondary";
}

type RuntimeId = (typeof RUNTIME_OPTIONS)[number]["id"];

export function NewSessionModal({
  projects,
  projectPaths,
  fallbackRoles,
  defaultProject,
  isAdmin,
  onCancel,
  onCreate,
}: {
  projects: string[];
  /** slug → real working-tree path. Optional (tests pass only `projects`);
   *  when present, each existing project's option shows its directory as the
   *  Combobox `hint` line so demo / demo2 / demo3 are distinguishable at pick
   *  time. */
  projectPaths?: Record<string, string>;
  /** Static role hints (ROLE_SUGGESTIONS ∪ live session roles) — the seed/
   *  fallback when a project's real roles can't be / aren't fetched. */
  fallbackRoles: string[];
  defaultProject: string;
  /** v0.8.20 F4 — beta-gating (UI ONLY): the admin sees every runtime + the
   *  role picker; a tenant sees only the production-stable claude/codex
   *  stream-json runtimes and creates roleless sessions. Not a security
   *  boundary — the backend create route is unchanged. */
  isAdmin: boolean;
  onCancel: () => void;
  onCreate: (
    slug: string,
    role: string,
    vendor: string,
    permissionMode: "skip" | "hitl",
    protocol: "stream-json" | "terminal" | "acp",
    newProjectPath?: string,
    host?: string,
  ) => Promise<boolean>;
}) {
  const [project, setProject] = useState(defaultProject);
  const [newSlug, setNewSlug] = useState("");
  const [newPath, setNewPath] = useState("");
  const [runtimeId, setRuntimeId] = useState<RuntimeId>("claude-stream-json");
  const [role, setRole] = useState("");
  const [hitl, setHitl] = useState(false);
  const [effort, setEffort] = useState<(typeof EFFORT_OPTIONS)[number]["id"]>("medium");
  const [host, setHost] = useState("local");
  const [pending, setPending] = useState(false);
  const [roleState, setRoleState] = useState<RoleFetchState>({ kind: "idle" });
  const runtime = RUNTIME_OPTIONS.find((item) => item.id === runtimeId) ?? RUNTIME_OPTIONS[0];
  const vendor = runtime.vendor;
  const protocol = runtime.protocol;

  // v0.8.20 F4 + v0.8.24 — stable: claude/codex stream-json + grok/opencode acp;
  // terminal is admin-only (frozen). NOT a security boundary.
  const runtimeOptions = isAdmin
    ? RUNTIME_OPTIONS
    : RUNTIME_OPTIONS.filter(
        (option) => option.protocol === "stream-json" || option.protocol === "acp",
      );

  const isNew = project === NEW_PROJECT;

  // ---- B3: fetch the selected (existing) project's real roles ------------
  // A brand-new project has no roles on disk yet, so we skip the fetch (it
  // would 404) and lean on the static fallback below — `roleChoices`/
  // `roleLoading` ignore `roleState` while `isNew`, so a stale value here is
  // inert and we don't need a synchronous reset (which would trip
  // react-hooks/set-state-in-effect). The async transitions (loading→ready/
  // error) follow the same data-fetch pattern as the per-sid history seed.
  useEffect(() => {
    if (isNew || !project) return;
    let cancelled = false;
    queueMicrotask(() => {
      if (!cancelled) setRoleState({ kind: "loading" });
    });
    listProjectRoles(project)
      .then((roles) => {
        if (!cancelled) setRoleState({ kind: "ready", roles });
      })
      .catch((e) => {
        if (cancelled) return;
        if (e instanceof Error && e.message === "UNAUTHENTICATED") {
          // global gate re-auths; just fall back silently.
          setRoleState({ kind: "error" });
          return;
        }
        toastBus.handler?.error(
          `加载角色失败: ${e instanceof Error ? e.message : "unknown"}（回退默认角色）`,
        );
        setRoleState({ kind: "error" });
      });
    return () => {
      cancelled = true;
    };
  }, [project, isNew]);

  // The role <select> options. For an EXISTING project with a successful,
  // non-empty fetch we show its real roles ("role — description"); otherwise
  // (new project / empty / error / still-loading) we fall back to the static
  // ROLE_SUGGESTIONS-seeded list so the user always has cto + the usual hints.
  // `isNew` shortcuts to the fallback regardless of any stale `roleState`.
  // F2-web — the explicit "no role / bare claude" choice leads every option
  // set (existing + new project), so a roleless session is always reachable
  // and never the silent default (resolveRole keeps `cto` as the no-pick
  // fallback).
  // Project picker options: the registered ∪ session projects, then the
  // "＋ new project…" sentinel. The "(no existing projects)" empty marker is
  // only offered when the list is truly empty (and not already creating one).
  const projectOptions: ComboboxOption[] = useMemo(() => {
    const opts: ComboboxOption[] = projects.map((p) => ({
      value: p,
      label: p,
      // Show the real working-tree path under the slug so a collision-suffixed
      // slug (demo / demo2 / demo3) is disambiguated at selection time.
      hint: projectPaths?.[p],
    }));
    if (projects.length === 0 && !isNew) opts.push({ value: "", label: "（暂无已有项目）" });
    opts.push({ value: NEW_PROJECT, label: "＋ 新建项目…" });
    return opts;
  }, [projects, projectPaths, isNew]);

  const roleChoices: { value: string; label: string }[] = useMemo(() => {
    const roleless = { value: ROLELESS, label: "(无角色 / 裸 claude)" };
    if (!isNew && roleState.kind === "ready" && roleState.roles.length > 0) {
      return [
        roleless,
        ...roleState.roles.map((r) => ({
          value: r.role,
          label: r.description ? `${r.role} — ${r.description}` : r.role,
        })),
      ];
    }
    return [roleless, ...fallbackRoles.map((r) => ({ value: r, label: r }))];
  }, [isNew, roleState, fallbackRoles]);

  const roleLoading = !isNew && roleState.kind === "loading";

  // The role <select>'s controlled value, DERIVED (not effect-synced) so the
  // option set changing (project switch / fetch resolve) can't desync state:
  // honor the user's explicit pick while it's still on offer, otherwise fall
  // back to the default. `role===""` means "no explicit pick yet".
  //
  // v0.8.20 F4: the new-session form now defaults to ROLELESS (a bare-claude
  // session) for everyone — the owner's call that web sessions start with no
  // role unless one is deliberately chosen. The admin can still pick a concrete
  // role from the picker below; a tenant has no picker, so it stays roleless.
  // (ROLELESS always leads `roleChoices`, so it is always on offer.)
  const selectedRole =
    role && roleChoices.some((c) => c.value === role) ? role : ROLELESS;

  // The wire role: ROLELESS → "" (roleless passthrough), a concrete pick →
  // that role, blank → DEFAULT_ROLE. See chatDefaults.resolveRole (pure +
  // unit-tested for the sentinel semantics).
  const effectiveRole = resolveRole(selectedRole);

  // ---- submit gating -----------------------------------------------------
  const newSlugErr = isNew ? slugError(newSlug.trim()) : null;
  const newPathErr = isNew ? pathError(newPath) : null;
  const ready = isNew
    ? !pending && newSlugErr === null && newPathErr === null
    : !pending && project.length > 0;

  const submit = () => {
    if (!ready) return;
    setPending(true);
    const targetSlug = isNew ? newSlug.trim() : project;
    void onCreate(
      targetSlug,
      effectiveRole,
      vendor,
      hitl ? "hitl" : "skip",
      protocol,
      isNew ? newPath.trim() : undefined,
      host,
    )
      .then((ok) => {
        // On success the parent navigates away & unmounts us. On failure it
        // toasted the (human-readable) error; we re-enable so the user can
        // fix the input and retry without losing what they typed.
        if (ok) onCancel();
        else setPending(false);
      })
      .catch(() => setPending(false));
  };

  // Esc closes, Enter submits (when not composing in the textarea-less modal).
  const onKeyDown = (event: React.KeyboardEvent) => {
    if (event.key === "Escape") {
      event.preventDefault();
      if (!pending) onCancel();
    } else if (event.key === "Enter") {
      event.preventDefault();
      submit();
    }
  };

  return (
    <div className="fixed inset-0 z-50 bg-black/50 grid place-items-center p-4" onClick={onCancel}>
      <div
        className="w-full max-w-md rounded-lg bg-surface-900 border border-surface-700 shadow-xl"
        onClick={(event) => event.stopPropagation()}
        onKeyDown={onKeyDown}
      >
        <div className="px-4 h-11 flex items-center justify-between border-b border-surface-700/50">
          <span className="text-sm font-semibold">新建 session</span>
          <button type="button" onClick={onCancel} className="text-text-dim hover:text-text-primary">
            ✕
          </button>
        </div>
        <div className="p-4 space-y-3">
          <label className="block text-xs text-text-dim">项目</label>
          <Combobox
            value={project}
            onChange={setProject}
            options={projectOptions}
            disabled={pending}
            searchable={projects.length > 8}
            searchPlaceholder="搜索项目…"
            ariaLabel="项目"
          />

          {isNew ? (
            <div className="space-y-3 rounded-md border border-brand-500/30 bg-brand-500/5 p-3">
              <div>
                <label className="block text-xs text-text-dim mb-1">项目名（slug）</label>
                <input
                  value={newSlug}
                  onChange={(event) => setNewSlug(event.target.value)}
                  disabled={pending}
                  autoFocus
                  placeholder="my-project"
                  className="w-full h-9 rounded-md bg-surface-800 border border-surface-700 px-3 text-sm outline-none focus:border-brand-500 disabled:opacity-40"
                />
                {newSlug.length > 0 && newSlugErr ? (
                  <div className="mt-1 text-[11px] text-status-error">{newSlugErr}</div>
                ) : null}
              </div>
              <div>
                <label className="block text-xs text-text-dim mb-1">工作目录</label>
                <input
                  value={newPath}
                  onChange={(event) => setNewPath(event.target.value)}
                  disabled={pending}
                  placeholder="~/code/my-project 或 /abs/path"
                  className="w-full h-9 rounded-md bg-surface-800 border border-surface-700 px-3 text-sm font-mono outline-none focus:border-brand-500 disabled:opacity-40"
                />
                {newPath.length > 0 && newPathErr ? (
                  <div className="mt-1 text-[11px] text-status-error">{newPathErr}</div>
                ) : null}
              </div>
            </div>
          ) : null}

          {/* v0.8.24 A2 — model + protocol (grouped by vendor) */}
          <label className="block text-xs text-text-dim">模型 · 协议</label>
          <div className="grid grid-cols-2 gap-1 rounded-md bg-surface-800 p-0.5">
            {runtimeOptions.map((option) => (
              <button
                key={option.id}
                type="button"
                disabled={pending}
                onClick={() => setRuntimeId(option.id)}
                data-testid={`runtime-${option.id}`}
                className={`min-h-10 rounded px-2 py-1 text-left disabled:opacity-40 ${
                  runtimeId === option.id ? "bg-surface-700 text-text-primary" : "text-text-dim"
                }`}
              >
                <span className="flex items-center gap-1.5">
                  <span
                    className={`h-1.5 w-1.5 rounded-full shrink-0 ${vendorBadgeClass(option.vendor).split(" ")[0].replace("/15", "")}`}
                    style={{
                      background:
                        option.vendor === "claude"
                          ? "var(--color-vendor-claude)"
                          : option.vendor === "codex"
                            ? "var(--color-vendor-codex)"
                            : option.vendor === "grok"
                              ? "var(--color-vendor-grok)"
                              : "var(--color-vendor-opencode)",
                    }}
                  />
                  <span className="block text-xs font-medium leading-4">{option.label}</span>
                </span>
                <span className="block text-[10px] leading-3 text-text-dim pl-3">{option.hint}</span>
              </button>
            ))}
          </div>

          <div className="grid grid-cols-2 gap-2">
            <div>
              <label className="block text-xs text-text-dim mb-1">力度 (effort)</label>
              <select
                value={effort}
                disabled={pending}
                onChange={(e) =>
                  setEffort(e.target.value as (typeof EFFORT_OPTIONS)[number]["id"])
                }
                data-testid="effort-select"
                className="w-full h-9 rounded-md bg-surface-800 border border-surface-700 px-2 text-xs outline-none focus:border-brand-500 disabled:opacity-40"
              >
                {EFFORT_OPTIONS.map((o) => (
                  <option key={o.id} value={o.id}>
                    {o.label}
                  </option>
                ))}
              </select>
            </div>
            <div>
              <label className="block text-xs text-text-dim mb-1">主机</label>
              <select
                value={host}
                disabled={pending}
                onChange={(e) => setHost(e.target.value)}
                data-testid="host-select"
                className="w-full h-9 rounded-md bg-surface-800 border border-surface-700 px-2 text-xs outline-none focus:border-brand-500 disabled:opacity-40"
              >
                <option value="local">local（本机）</option>
              </select>
              <p className="mt-0.5 text-[10px] text-text-muted">
                分支：显示用（本版不切换 worktree）
              </p>
            </div>
          </div>

          {/* v0.8.20 F4 — role picker is an admin-only (beta) surface; a tenant
              always creates a roleless session (selectedRole stays ROLELESS). */}
          {isAdmin ? (
            <>
              <div className="flex items-center justify-between">
                <label className="block text-xs text-text-dim">Role</label>
                {roleLoading ? (
                  <span className="text-[10px] text-text-dim">加载角色中…</span>
                ) : null}
              </div>
              <Combobox
                value={selectedRole}
                onChange={setRole}
                options={roleChoices}
                disabled={pending || roleLoading}
                searchable={roleChoices.length > 8}
                searchPlaceholder="搜索角色…"
                ariaLabel="Role"
              />
            </>
          ) : null}

          {/* HITL pre-spawn toggle — spawn-time only (mid-session deferred). */}
          <button
            type="button"
            disabled={pending}
            data-testid="hitl-toggle"
            onClick={() => setHitl((v) => !v)}
            className={`w-full h-9 rounded-md border text-xs font-medium flex items-center justify-center gap-2 transition-colors disabled:opacity-40 ${
              hitl
                ? "border-brand-500/60 bg-brand-500/15 text-brand-400"
                : "border-surface-700 bg-surface-800 text-text-secondary hover:border-surface-600"
            }`}
          >
            <span
              className={`h-2 w-2 rounded-full ${hitl ? "bg-brand-400" : "bg-surface-600"}`}
            />
            {hitl ? "请求批准 · 已开" : "请求批准 · 关闭（skip）"}
          </button>
          <p className="text-[10px] text-text-muted leading-4 -mt-1">
            影响 spawn 参数（skip vs default）。会话中途切换本版不支持。
          </p>

          <div className="text-[11px] font-mono text-text-dim leading-5">
            vendor=
            <span className={`text-text-secondary ${vendorBadgeClass(vendor)}`}>{vendor}</span>{" "}
            protocol=
            <span className="text-text-secondary">{protocol}</span> effort=
            <span className="text-text-secondary">{effort}</span> host=
            <span className="text-text-secondary">{host}</span> permission=
            <span className="text-text-secondary">{hitl ? "hitl" : "skip"}</span>
            {effectiveRole ? (
              <>
                {" "}
                role=<span className="text-text-secondary">{effectiveRole}</span>
              </>
            ) : (
              <> role=<span className="text-text-secondary">(roleless)</span></>
            )}
          </div>
        </div>
        <div className="px-4 py-3 flex justify-end gap-2 border-t border-surface-700/50">
          <button
            type="button"
            onClick={onCancel}
            disabled={pending}
            className="h-9 px-3 rounded-md text-sm text-text-dim hover:text-text-primary disabled:opacity-40"
          >
            取消
          </button>
          <button
            type="button"
            disabled={!ready}
            onClick={submit}
            className="h-9 px-3 rounded-md text-sm bg-brand-500 text-surface-950 hover:bg-brand-400 disabled:opacity-40 flex items-center gap-1.5"
          >
            {pending ? (
              <>
                <span className="h-3 w-3 rounded-full border-2 border-surface-950/40 border-t-surface-950 animate-spin" />
                {isNew ? "创建中…" : "切换中…"}
              </>
            ) : isNew ? (
              "创建项目并开始"
            ) : (
              "创建并切过去"
            )}
          </button>
        </div>
      </div>
    </div>
  );
}

// ── v0.8.21 HistorySection ────────────────────────────────────────────────────

/** v0.8.22 P0-3 review — Chinese relative-time phrase for an RFC3339
 *  timestamp, matching the surrounding hardcoded-Chinese copy ("3分钟前"/
 *  "昨天"/"3天前"). Mirrors `ccteam-im::gateway::relative_time_zh` so the IM
 *  `/sessions` history section and the SPA history rail read the same way.
 *  Unparseable/empty input renders as `"—"`. Exported for a small unit test. */
export function relativeTimeZh(iso: string | null | undefined): string {
  if (!iso) return "—";
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return "—";
  const secs = Math.max(0, Math.floor((Date.now() - then) / 1000));
  if (secs < 60) return "刚刚";
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins}分钟前`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}小时前`;
  const days = Math.floor(hours / 24);
  if (days === 1) return "昨天";
  if (days < 7) return `${days}天前`;
  const weeks = Math.floor(days / 7);
  if (weeks < 5) return `${weeks}周前`;
  return new Date(then).toISOString().slice(0, 10);
}

interface HistorySectionProps {
  project: string;
  expanded: boolean;
  loading: boolean;
  history: HistorySessionView[];
  activeSid: string | null | undefined;
  onToggle: () => void;
  onResume: (sid: string) => void;
  onOpenImport: () => void;
}

function HistorySection({
  project,
  expanded,
  loading,
  history,
  activeSid,
  onToggle,
  onResume,
  onOpenImport,
}: HistorySectionProps) {
  return (
    <div className="mt-0.5">
      <button
        type="button"
        onClick={onToggle}
        className="w-full text-left px-2 py-0.5 flex items-center gap-1 text-[10px] text-text-muted hover:text-text-secondary"
        title={expanded ? "折叠历史会话" : "展开历史会话"}
      >
        <span className="font-mono">{expanded ? "▾" : "▸"}</span>
        {loading ? (
          <span>历史加载中…</span>
        ) : (
          <span>
            {expanded && history.length > 0
              ? `历史 (${history.length})`
              : "更多历史"}
          </span>
        )}
      </button>
      {expanded && !loading && (
        <div className="space-y-0.5 mt-0.5">
          {history.length === 0 ? (
            <div className="px-3 text-[10px] text-text-muted italic">暂无历史会话</div>
          ) : null}
          {history.map((h) => {
            const isActive = h.sid === activeSid;
            // Item 9 — a stopped session can never be waiting-approval or
            // "stuck" (both are live-only concepts), so history rows only
            // ever resolve to "unread" or nothing.
            const attentionInfo = attentionMeta(
              sessionAttention(h, getLastViewedTurnCount(h.sid)),
            );
            return (
              <button
                key={h.sid}
                type="button"
                onClick={() => onResume(h.sid)}
                title={
                  h.transcript_present
                    ? "精确恢复 (vendor transcript 存在)"
                    : "按记录重放 (transcript 已清理)"
                }
                className={`w-full text-left px-2 py-1.5 rounded-md flex items-center gap-2 text-xs opacity-70 hover:opacity-100 ${
                  isActive
                    ? "bg-surface-700 text-text-primary opacity-100"
                    : "text-text-secondary hover:bg-surface-800/70"
                }`}
              >
                <span className="h-1.5 w-1.5 rounded-full shrink-0 bg-surface-600" />
                <span
                  className={`font-mono px-1 rounded text-[10px] ${vendorBadgeClass(h.vendor)}`}
                >
                  {h.vendor}
                </span>
                <span className="truncate flex-1">{railSessionLabel(h)}</span>
                {attentionInfo ? (
                  <span
                    className={`shrink-0 font-mono text-[9px] px-1 rounded ${attentionInfo.className}`}
                    title={attentionInfo.label}
                  >
                    {attentionInfo.label}
                  </span>
                ) : null}
                <span
                  className="text-[9px] text-text-muted shrink-0"
                  title={h.last_active || h.created_at}
                >
                  {relativeTimeZh(h.last_active || h.created_at)}
                </span>
                {h.transcript_present ? (
                  <span className="text-[9px] text-brand-400/80 font-mono shrink-0">精确</span>
                ) : (
                  <span className="text-[9px] text-text-muted font-mono shrink-0">重放</span>
                )}
                <span className="text-text-muted font-mono shrink-0">{h.sid}</span>
              </button>
            );
          })}
          {/* Import external session entry point */}
          <button
            type="button"
            onClick={() => onOpenImport()}
            className="w-full text-left px-2 py-0.5 flex items-center gap-1 text-[10px] text-text-muted hover:text-brand-400 hover:bg-surface-800/70 rounded-md"
            title={`导入 ${project} 的外部 Claude 会话`}
          >
            <Plus className="h-3 w-3 shrink-0" /> 导入历史会话
          </button>
        </div>
      )}
    </div>
  );
}
