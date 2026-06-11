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
// new-session default role stays `cto` (chatDefaults.DEFAULT_ROLE, FIX-2).

import { useCallback, useEffect, useMemo, useState } from "react";
import { NavLink, useLocation, useNavigate, useParams } from "react-router-dom";
import { MessageSquare, Menu, Plus, X } from "lucide-react";
import CostPill from "../components/CostPill";
import MarketplaceView from "./MarketplaceView";
import StatusView from "./StatusView";
import SettingsPage from "./SettingsPage";
import SessionView from "./SessionView";
import { createProject as apiCreateProject, fetchDashboard } from "../lib/dashboardApi";
import {
  createSession as apiCreateSession,
  listProjectRoles,
  listSessions,
  type RoleSummary,
  type SessionView as SessionSummary,
} from "../lib/sessionsApi";
import { toastBus } from "../lib/toastBus";
import { DEFAULT_ROLE, ROLE_SUGGESTIONS, ROLELESS, resolveRole } from "./chatDefaults";
import { mergeProjectSlugs } from "./projectList";

/** A switcher entry — one live gateway session, grouped under its project. */
type RailSession = SessionSummary;

/** The three bottom-nav global views — each is a full route the shell hosts
 *  in its main area (sidebar persists, Chat|终端 tabs hide). `null` = a
 *  session-chat surface (a selected session or the empty state). */
type GlobalView = "marketplace" | "status" | "settings" | null;

function globalViewFor(pathname: string): GlobalView {
  if (pathname.startsWith("/marketplace")) return "marketplace";
  if (pathname.startsWith("/status")) return "status";
  if (pathname.startsWith("/settings")) return "settings";
  return null;
}

/** Crumb label for each global view (shown in the top bar in place of the
 *  session crumb). */
const GLOBAL_VIEW_LABEL: Record<NonNullable<GlobalView>, string> = {
  marketplace: "插件市场",
  status: "Status",
  settings: "Settings",
};

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
  icon: string;
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
      <span className="w-4 text-center" aria-hidden>
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
    [refreshSessions, navigate],
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
        <span className="hidden sm:inline text-[11px] font-mono text-text-dim px-1.5 py-0.5 rounded bg-surface-800">
          per-session · /api/v1
        </span>
        <span className="flex-1" />
        {/* Cost pill — today's daily-spend / 24h-budget rollup; click → /status. */}
        <CostPill />
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
          className={`w-60 shrink-0 border-r border-surface-700/40 flex flex-col bg-surface-900 ${
            sidebarOpen
              ? "fixed inset-y-0 top-12 left-0 z-40 md:static md:top-0"
              : "hidden md:flex"
          }`}
        >
          <div className="h-10 shrink-0 px-3 flex items-center justify-between border-b border-surface-700/30">
            <span className="text-xs font-mono uppercase text-text-dim">所有 session</span>
            <div className="flex items-center gap-1">
              <button
                type="button"
                onClick={() => {
                  // bug5 — refetch so a project created out-of-band (CLI
                  // `ccteam init`) is in the list when the modal opens.
                  void refreshSessions();
                  setModalProject(null);
                  setModalOpen(true);
                }}
                className="h-6 px-2 rounded-md bg-brand-500/90 text-surface-950 hover:bg-brand-400 text-xs flex items-center gap-1"
                title="新建 session"
              >
                <Plus className="h-3.5 w-3.5" /> 新建
              </button>
              {/* close affordance inside the drawer (mobile only) */}
              <button
                type="button"
                onClick={() => setSidebarOpen(false)}
                aria-label="关闭"
                className="md:hidden h-6 w-6 grid place-items-center rounded-md text-text-dim hover:text-text-primary hover:bg-surface-800"
              >
                <X className="h-3.5 w-3.5" />
              </button>
            </div>
          </div>
          <div className="flex-1 overflow-y-auto p-2 space-y-2">
            {projects.map((project) => {
              const items = railSessions.filter((s) => s.project === project);
              return (
                <div key={project}>
                  <div className="px-1.5 py-1 text-[11px] font-mono text-text-dim">{project}</div>
                  <div className="space-y-0.5">
                    {items.map((s) => {
                      const active = s.sid === sid;
                      const isClaude = s.vendor === "claude";
                      return (
                        <button
                          key={s.sid}
                          type="button"
                          onClick={() => switchTo(s)}
                          className={`w-full text-left px-2 py-1.5 rounded-md flex items-center gap-2 text-xs ${
                            active
                              ? "bg-surface-700 text-text-primary"
                              : "text-text-secondary hover:bg-surface-800/70"
                          }`}
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
                          <span className="truncate flex-1">{s.role || "(无 role)"}</span>
                          {s.permission_mode === "hitl" ? (
                            <span
                              className="font-mono text-[9px] text-brand-400/90"
                              title="HITL: 非 allowlist 工具需批准"
                            >
                              hitl
                            </span>
                          ) : null}
                          <span className="text-text-dim font-mono">{s.sid}</span>
                        </button>
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
                        className="w-full text-left px-2 py-1 rounded-md flex items-center gap-1 text-[10px] text-text-dim/70 hover:text-brand-400 hover:bg-surface-800/70"
                        title={`为 ${project} 创建第一个 session`}
                      >
                        <Plus className="h-3 w-3" /> 还没有 session — 新建
                      </button>
                    ) : null}
                  </div>
                </div>
              );
            })}
            {projects.length === 0 ? (
              <div className="px-2 py-3 text-xs text-text-dim leading-5">
                {railError ? `加载失败: ${railError}` : "还没有 session。点「＋ 新建」创建。"}
              </div>
            ) : null}
          </div>

          {/* bottom global-nav (prototype `.nav` / `.navhint`): the session
              list above IS the chat navigation (click a session = its chat),
              so there's NO "Chat" item — only the 3 global views. */}
          <nav className="border-t border-surface-700/40 p-2">
            <p className="px-2 pb-2 pt-1 text-[11px] leading-snug text-text-dim/80">
              ↑ 点上面的会话 = 进入它的聊天（每个 session 一个独立对话）。下面是全局页：
            </p>
            <div className="space-y-0.5">
              <SidebarNavLink
                to="/marketplace"
                icon="🧩"
                label="插件市场"
                onNavigate={() => setSidebarOpen(false)}
              />
              <SidebarNavLink
                to="/status"
                icon="📊"
                label="Status"
                onNavigate={() => setSidebarOpen(false)}
              />
              <SidebarNavLink
                to="/settings"
                icon="⚙︎"
                label="Settings"
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
                  {GLOBAL_VIEW_LABEL[globalView]}
                </span>
              </div>
              <div className="flex-1 min-h-0 overflow-y-auto">
                {globalView === "settings" ? (
                  <SettingsPage />
                ) : globalView === "marketplace" ? (
                  <MarketplaceView />
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
          fallbackRoles={roleOptions}
          defaultProject={modalProject ?? activeView?.project ?? projects[0] ?? ""}
          onCancel={() => {
            setModalOpen(false);
            setModalProject(null);
          }}
          onCreate={createSession}
        />
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
//   - F2-web: the role dropdown now offers an explicit "(无角色 / 裸 claude)"
//     choice (the ROLELESS sentinel). Picking it sends an empty role (a
//     bare-claude session that self-reads the project CLAUDE.md);
//     `resolveRole` (chatDefaults) maps the sentinel → "" while an
//     un-touched modal still falls back to DEFAULT_ROLE (cto), never roleless.

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
  fallbackRoles,
  defaultProject,
  onCancel,
  onCreate,
}: {
  projects: string[];
  /** Static role hints (ROLE_SUGGESTIONS ∪ live session roles) — the seed/
   *  fallback when a project's real roles can't be / aren't fetched. */
  fallbackRoles: string[];
  defaultProject: string;
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
  // back to a sensible default. `role===""` means "no explicit pick yet".
  //
  // F2-web: with ROLELESS now leading every option set, the no-pick fallback
  // must NOT be `roleChoices[0]` (that would silently default to roleless and
  // break FIX-2). Prefer DEFAULT_ROLE (`cto`) when it's on offer, else the
  // first concrete (non-roleless) option, else roleless as the last resort.
  const selectedRole = role && roleChoices.some((c) => c.value === role)
    ? role
    : roleChoices.find((c) => c.value === DEFAULT_ROLE)?.value ??
      roleChoices.find((c) => c.value !== ROLELESS)?.value ??
      roleChoices[0]?.value ??
      "";

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
          <select
            value={project}
            onChange={(event) => setProject(event.target.value)}
            disabled={pending}
            className="w-full h-9 rounded-md bg-surface-800 border border-surface-700 px-3 text-sm outline-none focus:border-brand-500 disabled:opacity-40"
          >
            {projects.length === 0 && !isNew ? (
              <option value="">（暂无已有项目）</option>
            ) : null}
            {projects.map((item) => (
              <option key={item} value={item}>
                {item}
              </option>
            ))}
            <option value={NEW_PROJECT}>＋ 新建项目…</option>
          </select>

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
            {RUNTIME_OPTIONS.map((option) => (
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

          <div className="flex items-center justify-between">
            <label className="block text-xs text-text-dim">Role</label>
            {roleLoading ? (
              <span className="text-[10px] text-text-dim">加载角色中…</span>
            ) : null}
          </div>
          <select
            value={selectedRole}
            onChange={(event) => setRole(event.target.value)}
            disabled={pending || roleLoading}
            className="w-full h-9 rounded-md bg-surface-800 border border-surface-700 px-3 text-sm outline-none focus:border-brand-500 disabled:opacity-40"
          >
            {roleChoices.map((c) => (
              <option key={c.value} value={c.value}>
                {c.label}
              </option>
            ))}
          </select>

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
