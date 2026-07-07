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
//   - CHROME the app bar + CostPill + bottom global-nav + the NewSessionModal
//            (create a session / a brand-new project inline).
//
// What moved OUT to SessionView (per-sid): the transcript rows + localStorage
// seed/persist, `useSessionEvents(sid)` + its fold, the draft + submitTurn, the
// chat|terminal toggle + TerminalView, stopSession, and HITL approval resolve.
//
// Red lines: reads structured turn/SSE frames (never scrapes a pane); the
// new-session form defaults to ROLELESS (v0.8.20 F4 — a bare-claude session),
// and the role picker + terminal runtimes are admin-only (UI-level beta-gate).

import { useCallback, useEffect, useMemo, useState } from "react";
import { NavLink, useLocation, useNavigate, useParams } from "react-router-dom";
import {
  Activity,
  MessageSquare,
  Menu,
  Pencil,
  Plus,
  Puzzle,
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

/** Frontend-only soft cap on how many active sessions one user may hold at
 *  once. "Active" = the sessions in this caller's own cross-project list
 *  (`railSessions`), which the backend already ACL-scopes to the caller — so
 *  counting that list is counting the user's own live sessions. This is a UX
 *  guard (block the create + toast), NOT a security/backend limit. */
const MAX_ACTIVE_SESSIONS = 10;

/** The three bottom-nav global views — each is a full route the shell hosts
 *  in its main area (sidebar persists, Chat|终端 tabs hide). `null` = a
 *  session-chat surface (a selected session or the empty state). */
type GlobalView = "marketplace" | "status" | "hosts" | "settings" | null;

function globalViewFor(pathname: string): GlobalView {
  if (pathname.startsWith("/marketplace")) return "marketplace";
  if (pathname.startsWith("/status")) return "status";
  if (pathname.startsWith("/hosts")) return "hosts";
  if (pathname.startsWith("/settings")) return "settings";
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
}: {
  to: string;
  icon: React.ReactNode;
  label: string;
  onNavigate?: () => void;
}) {
  return (
    <NavLink
      to={to}
      onClick={onNavigate}
      className={({ isActive }) =>
        `flex items-center gap-2.5 px-2.5 py-2 rounded-md text-xs transition-colors ${
          isActive
            ? "bg-surface-800 text-brand-400"
            : "text-text-secondary hover:bg-surface-800/70 hover:text-text-primary"
        }`
      }
    >
      <span className="grid w-4 place-items-center" aria-hidden>
        {icon}
      </span>
      {label}
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
  // Mobile sidebar drawer (the fixed `w-60` rail is off-canvas under `md`; a
  // hamburger toggles it). Closed by default; auto-closed on a session switch
  // / global-nav pick so the chosen surface is visible without a manual close.
  const [sidebarOpen, setSidebarOpen] = useState(false);
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
      protocol: "stream-json" | "terminal",
      newProjectPath?: string,
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
  const roleOptions = useMemo(() => {
    const seen = new Set(ROLE_SUGGESTIONS);
    railSessions.forEach((s) => s.role && seen.add(s.role));
    return Array.from(seen);
  }, [railSessions]);
  // Frontend-only cap (see MAX_ACTIVE_SESSIONS) — disable the header "＋ 新建"
  // entry at the limit so the affordance reflects the block; the create
  // funnel (`createSession`) still hard-guards + toasts as the source of truth.
  const atSessionCap = railSessions.length >= MAX_ACTIVE_SESSIONS;

  const switchTo = useCallback(
    (s: RailSession) => {
      // Navigate only — the remounted <SessionView key={sid}> resets its own
      // view to "chat" (a fresh instance), so the shell no longer tracks it.
      navigate(`/chat/s/${encodeURIComponent(s.sid)}`);
      setSidebarOpen(false);
    },
    [navigate],
  );

  return (
    <div className="h-full min-h-0 flex flex-col bg-surface-900 text-text-primary">
      {/* standalone app bar */}
      <header className="h-12 shrink-0 border-b border-surface-700/40 px-3 sm:px-4 flex items-center gap-2 sm:gap-3">
        {/* mobile drawer toggle — hidden on md+ where the rail is always shown */}
        <button
          type="button"
          onClick={() => setSidebarOpen(true)}
          aria-label="打开会话列表"
          className="md:hidden h-8 w-8 -ml-1 grid place-items-center rounded-md text-text-secondary hover:text-text-primary hover:bg-surface-800"
        >
          <Menu className="h-4 w-4" />
        </button>
        <MessageSquare className="h-4 w-4 text-brand-400 shrink-0" />
        <span className="text-sm font-semibold">
          ccteam <span className="text-brand-400">chat</span>
        </span>
        <span className="flex-1" />
        {/* Cost pill — today's daily-spend / 24h-budget rollup; click → /status. */}
        <CostPill />
        <AvatarMenu />
      </header>

      <div className="flex flex-1 min-h-0">
        {/* mobile drawer backdrop — only when open + below md. Click to close. */}
        {sidebarOpen ? (
          <button
            type="button"
            aria-label="关闭会话列表"
            onClick={() => setSidebarOpen(false)}
            className="md:hidden fixed inset-0 top-12 z-30 bg-black/50"
          />
        ) : null}
        {/* left rail — every project's gateway sessions, grouped. On md+ it is
            a static `w-60` column; below md it is an off-canvas drawer toggled
            by the header hamburger (slides in over the backdrop). */}
        <aside
          className={`w-60 shrink-0 border-r border-surface-700/60 flex flex-col bg-surface-950 ${
            sidebarOpen
              ? "fixed inset-y-0 top-12 left-0 z-40 md:static md:top-0"
              : "hidden md:flex"
          }`}
        >
          <div className="h-10 shrink-0 px-3 flex items-center justify-between border-b border-surface-700/30">
            <span className="text-xs font-mono uppercase text-text-secondary">所有 session</span>
            <div className="flex items-center gap-1">
              <button
                type="button"
                disabled={atSessionCap}
                onClick={() => {
                  // bug5 — refetch so a project created out-of-band (CLI
                  // `ccteam init`) is in the list when the modal opens.
                  void refreshSessions();
                  setModalProject(null);
                  setModalOpen(true);
                }}
                className="h-6 px-2 rounded-md bg-brand-500/90 text-surface-950 hover:bg-brand-400 text-xs flex items-center gap-1 disabled:opacity-50 disabled:cursor-not-allowed disabled:hover:bg-brand-500/90"
                title={
                  atSessionCap
                    ? tr(
                        lang,
                        "最多 10 个活跃 session,请先结束其他 session",
                        "Max 10 active sessions — please end others first",
                      )
                    : "新建 session"
                }
              >
                <Plus className="h-3.5 w-3.5" /> 新建
              </button>
              {/* close affordance inside the drawer (mobile only) */}
              <button
                type="button"
                onClick={() => setSidebarOpen(false)}
                aria-label="关闭"
                className="md:hidden h-6 w-6 grid place-items-center rounded-md text-text-muted hover:text-text-primary hover:bg-surface-800"
              >
                <X className="h-3.5 w-3.5" />
              </button>
            </div>
          </div>
          <div className="flex-1 overflow-y-auto p-2 space-y-2">
            {projects.map((project) => {
              const items = railSessions.filter((s) => s.project === project);
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
                      const isClaude = s.vendor === "claude";
                      const editing = renamingSid === s.sid;
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
                                className={`h-1.5 w-1.5 rounded-full shrink-0 ${
                                  active ? "bg-status-running" : "bg-surface-700"
                                }`}
                              />
                              <span
                                className={`font-mono px-1 rounded text-[10px] ${
                                  isClaude
                                    ? "bg-vendor-claude/15 text-vendor-claude"
                                    : "bg-vendor-codex/15 text-vendor-codex"
                                }`}
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
            {projects.length === 0 ? (
              <div className="px-2 py-3 text-xs text-text-secondary leading-5">
                {railError ? `加载失败: ${railError}` : "还没有 session。点「＋ 新建」创建。"}
              </div>
            ) : null}
          </div>

          {/* bottom global-nav (prototype `.nav` / `.navhint`): the session
              list above IS the chat navigation (click a session = its chat),
              so there's NO "Chat" item — only the 3 global views. */}
          <nav className="border-t border-surface-700/40 p-2">
            <div className="space-y-0.5">
              <SidebarNavLink
                to="/marketplace"
                icon={<Puzzle className="h-4 w-4" />}
                label={navLabel("marketplace", lang)}
                onNavigate={() => setSidebarOpen(false)}
              />
              {/* v0.8.18 档1 — Status / 主机 are operator/admin surfaces. */}
              {isAdmin && (
                <>
                  <SidebarNavLink
                    to="/status"
                    icon={<Activity className="h-4 w-4" />}
                    label={navLabel("status", lang)}
                    onNavigate={() => setSidebarOpen(false)}
                  />
                  <SidebarNavLink
                    to="/hosts"
                    icon={<Server className="h-4 w-4" />}
                    label={navLabel("hosts", lang)}
                    onNavigate={() => setSidebarOpen(false)}
                  />
                </>
              )}
              {/* v0.8.20 F2 — Settings is visible to tenants too (their
                  self-serve "我的 IM bot"); the page itself shows admin-only
                  sections only to the admin. */}
              <SidebarNavLink
                to="/settings"
                icon={<Settings className="h-4 w-4" />}
                label={navLabel("settings", lang)}
                onNavigate={() => setSidebarOpen(false)}
              />
            </div>
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
                  {navLabel(globalView, lang)}
                </span>
              </div>
              <div className="flex-1 min-h-0 overflow-y-auto">
                {globalView === "settings" ? (
                  <SettingsPage />
                ) : globalView === "marketplace" ? (
                  <MarketplaceView />
                ) : globalView === "hosts" ? (
                  <HostsView />
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
            <>
              <div className="h-10 shrink-0 px-4 flex items-center gap-3 border-b border-surface-700/30">
                <span className="text-xs text-text-dim shrink-0">会话 →</span>
                <span className="text-xs text-text-dim">从左侧选一个 session</span>
              </div>
              <div className="flex-1 min-h-0 grid place-items-center text-xs text-text-dim">
                选一个 session 或点「＋ 新建」开始。
              </div>
            </>
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
] as const satisfies readonly {
  id: string;
  label: string;
  hint: string;
  vendor: "claude" | "codex";
  protocol: "stream-json" | "terminal";
}[];

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
    protocol: "stream-json" | "terminal",
    newProjectPath?: string,
  ) => Promise<boolean>;
}) {
  const [project, setProject] = useState(defaultProject);
  const [newSlug, setNewSlug] = useState("");
  const [newPath, setNewPath] = useState("");
  const [runtimeId, setRuntimeId] = useState<RuntimeId>("claude-stream-json");
  const [role, setRole] = useState("");
  const [hitl, setHitl] = useState(false);
  const [pending, setPending] = useState(false);
  const [roleState, setRoleState] = useState<RoleFetchState>({ kind: "idle" });
  const runtime = RUNTIME_OPTIONS.find((item) => item.id === runtimeId) ?? RUNTIME_OPTIONS[0];
  const vendor = runtime.vendor;
  const protocol = runtime.protocol;

  // v0.8.20 F4 — beta-gating (UI-level only): production-stable runtimes
  // (claude/codex on stream-json) for everyone; the terminal/rmux protocol is
  // an advanced surface shown ONLY to the admin. The default `runtimeId`
  // (claude-stream-json) is in both sets, so a tenant never lands on a hidden
  // option. NOT a security boundary — the backend create route is unchanged.
  const runtimeOptions = isAdmin
    ? RUNTIME_OPTIONS
    : RUNTIME_OPTIONS.filter((option) => option.protocol === "stream-json");

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

          <label className="block text-xs text-text-dim">运行时</label>
          <div className="grid grid-cols-2 gap-1 rounded-md bg-surface-800 p-0.5">
            {runtimeOptions.map((option) => (
              <button
                key={option.id}
                type="button"
                disabled={pending}
                onClick={() => setRuntimeId(option.id)}
                className={`min-h-10 rounded px-2 py-1 text-left disabled:opacity-40 ${
                  runtimeId === option.id ? "bg-surface-700 text-text-primary" : "text-text-dim"
                }`}
              >
                <span className="block text-xs font-medium leading-4">{option.label}</span>
                <span className="block text-[10px] leading-3 text-text-dim">{option.hint}</span>
              </button>
            ))}
          </div>
          <p className="text-[10px] text-text-dim leading-4">
            {runtime.label} → vendor={vendor} · protocol={protocol}
          </p>

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

          <label className="flex items-center gap-2 text-xs text-text-secondary">
            <input
              type="checkbox"
              checked={hitl}
              disabled={pending}
              onChange={(event) => setHitl(event.target.checked)}
              className="accent-brand-500"
            />
            HITL 批准（非 allowlist 工具需在此点同意/拒绝）
          </label>

          <div className="text-[11px] font-mono text-text-dim leading-5">
            {isNew ? (
              <>
                → <span className="text-text-secondary">POST /api/v1/projects</span> slug=
                <span className="text-text-secondary">{newSlug.trim() || "<slug>"}</span>
                <br />
              </>
            ) : null}
            → <span className="text-text-secondary">
              POST /api/v1/projects/{(isNew ? newSlug.trim() : project) || "<project>"}/sessions
            </span>
            <br />
            → role=
            <span className="text-text-secondary">
              {effectiveRole || "(无角色 / 裸 claude)"}
            </span>{" "}
            vendor=
            <span className="text-text-secondary">{vendor}</span> permission=
            <span className="text-text-secondary">{hitl ? "hitl" : "skip"}</span>{" "}
            protocol=
            <span className="text-text-secondary">{protocol}</span>
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
            const isClaude = h.vendor === "claude";
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
                  className={`font-mono px-1 rounded text-[10px] ${
                    isClaude
                      ? "bg-vendor-claude/15 text-vendor-claude"
                      : "bg-vendor-codex/15 text-vendor-codex"
                  }`}
                >
                  {h.vendor}
                </span>
                <span className="truncate flex-1">{railSessionLabel(h)}</span>
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
