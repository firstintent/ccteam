// v0.8.24 Track A — the SPA shell, rebuilt to the prototype
// (docs-local/versions/v0-8-24/ui-prototype.html): `.app` = sidebar + main,
// NO full-width top bar, four mutually-exclusive views:
//
//   Home (`/`)                — landing page; the session is lazy-created on
//                               the first message (HomeView).
//   Conversation (`/chat/s/:sid`) — the per-sid chat/terminal (SessionView,
//                               keyed by sid → atomic per-sid state reset).
//   工作流 (`/flow/:tab?`)     — Skills / Roles / MCP / 自进化
//                               (WorkflowView, set-nav layout).
//   设置 (`/settings/:tab?`)   — 运维总览 / 接入 / 通用 / 账号 / 管理员
//                               (SettingsView, set-nav layout; only user
//                               management stays fail-closed via useMe).
//
// The sidebar (Sidebar.tsx) is the single navigation axis: ⌘K search,
// 「新建会话」→ Home, 「工作流」, per-project session groups (live + stopped
// history rows — clicking a stopped row RESUMES it), bottom 「设置」 + user.
// Collapsible to a 64px icon rail on desktop; a fixed drawer + backdrop +
// floating hamburger ≤820px (prototype breakpoints, CSS-driven).

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useLocation, useNavigate, useParams } from "react-router-dom";
import { Menu } from "lucide-react";
import HomeView from "./HomeView";
import SessionView from "./SessionView";
import WorkflowView from "./WorkflowView";
import SettingsView from "./SettingsView";
import AgentsView from "./AgentsView";
import DshView from "./DshView";
import { Sidebar, type RailRow } from "../components/Sidebar";
import { deleteProject } from "../lib/dashboardApi";
import {
  listHistorySessions,
  listSessions,
  renameSession as apiRenameSession,
  resumeSession,
  stopSession as apiStopSession,
  type HistorySessionView,
  type SessionView as SessionSummary,
} from "../lib/sessionsApi";
import { toastBus } from "../lib/toastBus";
import { t, tStopped } from "../lib/i18n";
import { useWebSettings } from "../hooks/useWebSettings";
import { useAgentsEvents } from "../hooks/useAgentsEvents";
import { useMe } from "../hooks/useMe";
import { projectsStore, useProjectsStore } from "../hooks/useProjectsStore";
import {
  createLifecycleReconciler,
  enqueueUnseenLifecycleEvents,
  type LifecycleReconciler,
} from "../lib/lifecycleReconciler";
import { railSessionLabel, renameToastText } from "./railHelpers";
import { mergeProjectSlugs } from "./projectList";

type ShellView = "home" | "conv" | "flow" | "settings" | "agents" | "dsh";

// eslint-disable-next-line react-refresh/only-export-components -- pure helper co-located for unit tests.
export function shellViewFor(pathname: string): ShellView {
  if (pathname.startsWith("/chat/s/")) return "conv";
  if (pathname.startsWith("/flow")) return "flow";
  if (pathname.startsWith("/settings")) return "settings";
  // v0.9.0 W4 — 团队/Team view; backend graph/SSE responses are identity-filtered.
  if (pathname.startsWith("/agents")) return "agents";
  // v0.9.15 — DSH embedded page (per-identity instance via companion proxy).
  if (pathname.startsWith("/dsh")) return "dsh";
  return "home";
}

/** How many stopped (history) sessions each project contributes to the rail
 *  — enough to resume recent work without drowning the live rows. */
const HISTORY_PER_PROJECT = 6;

/** Synthesize a live-shaped `SessionSummary` from a stopped/history row so a
 *  directly-navigated session that is no longer live still renders its REAL
 *  vendor / protocol / role / title (not the "claude" default a `null` session
 *  falls back to). The server cold-resumes it on the next send (resume-by-sid),
 *  after which the live row takes over. */
function historyToSummary(h: HistorySessionView): SessionSummary {
  return {
    sid: h.sid,
    project: h.slug,
    role: h.role,
    vendor: h.vendor,
    permission_mode: h.permission_mode,
    protocol: h.protocol,
    host: "local",
    current: false,
    status: "off",
    created_at: h.created_at,
    last_active: h.last_active,
    title: h.title ?? null,
    turn_count: h.turn_count,
    cost_usd: h.cost_usd ?? null,
    waiting_approval: false,
  };
}

export default function ChatConsole() {
  const { sid: routeSid, tab: routeTab } = useParams<{ sid: string; tab: string }>();
  const sid = routeSid ?? null;
  const navigate = useNavigate();
  const location = useLocation();
  const view = shellViewFor(location.pathname);
  const { settings } = useWebSettings();
  const lang = settings.language;
  const { me } = useMe();
  const { projects: projectRows } = useProjectsStore();

  // ---- cross-project session data (live + stopped history) -----------------
  const [sessionsByProject, setSessionsByProject] = useState<Record<string, SessionSummary[]>>({});
  const [historyByProject, setHistoryByProject] = useState<Record<string, HistorySessionView[]>>(
    {},
  );
  const registeredProjects = useMemo(() => (projectRows ?? []).map((project) => project.slug), [projectRows]);
  const registeredProjectSet = useMemo(() => new Set(registeredProjects), [registeredProjects]);
  const railSessions = useMemo(
    () => registeredProjects.flatMap((slug) => sessionsByProject[slug] ?? []),
    [registeredProjects, sessionsByProject],
  );
  const visibleHistoryByProject = useMemo(
    () => Object.fromEntries(Object.entries(historyByProject).filter(([slug]) => registeredProjectSet.has(slug))),
    [historyByProject, registeredProjectSet],
  );
  const projectPaths = useMemo(
    () => Object.fromEntries((projectRows ?? []).map((project) => [project.slug, project.path])),
    [projectRows],
  );
  const projectHosts = useMemo(
    () =>
      Object.fromEntries(
        (projectRows ?? []).map((project) => [
          project.slug,
          { host: project.host || "local", online: project.host_online },
        ]),
      ),
    [projectRows],
  );
  const projectBranches = useMemo(
    () =>
      Object.fromEntries(
        (projectRows ?? [])
          .filter((project) => project.current_branch)
          .map((project) => [project.slug, project.current_branch as string]),
      ),
    [projectRows],
  );

  const sessionRequests = useRef(new Map<string, Promise<void>>());
  const reconcileProject = useCallback((slug: string): Promise<void> => {
    const current = sessionRequests.current.get(slug);
    if (current) return current;
    const request = Promise.allSettled([
      listSessions(slug, { background: true }),
      listHistorySessions(slug, { background: true }),
    ])
      .then(([live, history]) => {
        if (live.status === "fulfilled") {
          setSessionsByProject((previous) => ({ ...previous, [slug]: live.value }));
        }
        if (history.status === "fulfilled") {
          setHistoryByProject((previous) => ({
            ...previous,
            [slug]: history.value.slice(0, HISTORY_PER_PROJECT),
          }));
        }
        if (live.status === "rejected") throw live.reason;
        if (history.status === "rejected") throw history.reason;
      })
      .finally(() => {
        sessionRequests.current.delete(slug);
      });
    sessionRequests.current.set(slug, request);
    return request;
  }, []);

  const refreshSessions = useCallback(
    () => Promise.allSettled(registeredProjects.map(reconcileProject)).then(() => undefined),
    [registeredProjects, reconcileProject],
  );

  // A newly-visible project gets exactly one live/history pair. Existing rows
  // are refreshed by lifecycle frames, not by every `/projects` refresh.
  const initializedProjects = useRef(new Set<string>());
  useEffect(() => {
    for (const slug of initializedProjects.current) {
      if (!registeredProjectSet.has(slug)) initializedProjects.current.delete(slug);
    }
    for (const slug of registeredProjects) {
      if (initializedProjects.current.has(slug)) continue;
      initializedProjects.current.add(slug);
      queueMicrotask(() => void reconcileProject(slug).catch(() => {}));
    }
  }, [registeredProjects, registeredProjectSet, reconcileProject]);

  // Capacity eviction is out-of-band from the session the user is currently
  // viewing. Listen to the daemon-wide lifecycle stream so the live/history
  // rail refreshes immediately even when a different sid was evicted.
  const { events: globalEvents } = useAgentsEvents(true, "session_lifecycle");
  const lifecycleReconciler = useRef<LifecycleReconciler | null>(null);
  useEffect(() => {
    const reconciler = createLifecycleReconciler(reconcileProject);
    lifecycleReconciler.current = reconciler;
    return () => {
      reconciler.stop();
      if (lifecycleReconciler.current === reconciler) lifecycleReconciler.current = null;
    };
  }, [reconcileProject]);

  useEffect(() => {
    lifecycleReconciler.current?.setVisibilityRefreshSlugs(registeredProjects);
  }, [registeredProjects]);

  // React may batch many SSE frames into one render. Walk every newly-appended
  // ring entry so a two-project burst cannot lose the earlier slug. The
  // numeric watermark survives ring eviction/reconstruction; object identity
  // cannot provide that guarantee.
  const lastLifecycleSeq = useRef(0);
  useEffect(() => {
    lastLifecycleSeq.current = enqueueUnseenLifecycleEvents(
      globalEvents,
      lastLifecycleSeq.current,
      (slug) => lifecycleReconciler.current?.enqueue(slug),
    );
  }, [globalEvents]);

  const projects = useMemo(
    () => mergeProjectSlugs(registeredProjects, railSessions),
    [registeredProjects, railSessions],
  );

  // ---- sidebar rows: live sessions + resumable history ---------------------
  const liveSids = useMemo(() => new Set(railSessions.map((s) => s.sid)), [railSessions]);
  const rows: RailRow[] = useMemo(() => {
    const live: RailRow[] = railSessions.map((s) => ({
      sid: s.sid,
      project: s.project,
      label: railSessionLabel(s),
      vendor: s.vendor,
      model: undefined,
      status: s.status,
    }));
    const hist: RailRow[] = Object.values(visibleHistoryByProject)
      .flat()
      .filter((h) => !liveSids.has(h.sid))
      .map((h) => ({
        sid: h.sid,
        project: h.slug,
        label: railSessionLabel(h),
        vendor: h.vendor,
        status: "off",
        history: true,
      }));
    return [...live, ...hist];
  }, [railSessions, visibleHistoryByProject, liveSids]);

  // ---- sidebar chrome state -------------------------------------------------
  const [collapsed, setCollapsed] = useState(() => {
    try {
      return localStorage.getItem("ccteam-side-collapsed") === "1";
    } catch {
      return false;
    }
  });
  const [mobileOpen, setMobileOpen] = useState(false);
  const [query, setQuery] = useState("");
  const [homeProject, setHomeProject] = useState<string | null>(null);
  const searchRef = useRef<HTMLInputElement | null>(null);

  const setSideCollapsed = useCallback((c: boolean) => {
    setCollapsed(c);
    try {
      localStorage.setItem("ccteam-side-collapsed", c ? "1" : "0");
    } catch {
      /* ignore */
    }
  }, []);

  const isMobile = () =>
    typeof window !== "undefined" &&
    typeof window.matchMedia === "function" &&
    window.matchMedia("(max-width: 820px)").matches;

  // ⌘K / Ctrl+K focuses the sidebar search (expands / opens the drawer first).
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "k") {
        e.preventDefault();
        setSideCollapsed(false);
        if (isMobile()) setMobileOpen(true);
        window.setTimeout(() => {
          searchRef.current?.focus();
          searchRef.current?.select();
        }, 60);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [setSideCollapsed]);

  const closeMobile = useCallback(() => setMobileOpen(false), []);

  // ---- navigation actions ----------------------------------------------------
  const goHome = useCallback(
    (project?: string | null) => {
      setHomeProject(project ?? null);
      navigate("/");
      closeMobile();
    },
    [navigate, closeMobile],
  );

  const openRow = useCallback(
    (row: { sid: string; project: string; history?: boolean }) => {
      if (row.history) {
        resumeSession(row.project, row.sid)
          .then(({ sid: newSid }) => {
            void reconcileProject(row.project).catch(() => {});
            navigate(`/chat/s/${encodeURIComponent(newSid)}`);
          })
          .catch((e) => {
            toastBus.handler?.error(`Resume failed: ${e instanceof Error ? e.message : e}`);
          });
      } else {
        navigate(`/chat/s/${encodeURIComponent(row.sid)}`);
      }
      closeMobile();
    },
    [navigate, reconcileProject, closeMobile],
  );

  const stopRow = useCallback(
    (row: { sid: string; project?: string }) => {
      apiStopSession(row.sid)
        .then(() => {
          toastBus.handler?.info(tStopped(lang, row.sid));
          if (row.project) void reconcileProject(row.project).catch(() => {});
          else void refreshSessions();
          if (row.sid === sid) navigate("/");
        })
        .catch((e) => {
          toastBus.handler?.error(`Stop failed: ${e instanceof Error ? e.message : e}`);
        });
    },
    [refreshSessions, reconcileProject, lang, sid, navigate],
  );

  // One rename action for every surface that offers it (rail rows + the
  // conversation header), so the toast, the refresh and the failure handling
  // are identical wherever the user renames from. Live and stopped sessions
  // take the same path — the server renames either.
  const renameRow = useCallback(
    async (targetSid: string, title: string) => {
      try {
        const result = await apiRenameSession(targetSid, title);
        toastBus.handler?.info(renameToastText(lang, result));
        // Awaited (not fire-and-forget) so the caller's optimistic title only
        // clears once the rail carries the server's own cleaned title.
        const project =
          railSessions.find((session) => session.sid === targetSid)?.project ??
          Object.values(visibleHistoryByProject)
            .flat()
            .find((session) => session.sid === targetSid)?.slug;
        if (project) await reconcileProject(project);
        else await refreshSessions();
      } catch (e) {
        toastBus.handler?.error(
          `${t(lang, "renameFailed")}: ${e instanceof Error ? e.message : e}`,
        );
      }
    },
    [railSessions, visibleHistoryByProject, reconcileProject, refreshSessions, lang],
  );

  const activeSession = useMemo(() => {
    const live = railSessions.find((s) => s.sid === sid);
    if (live) return live;
    if (!sid) return null;
    // Not in the live list → fall back to the stopped/history row (when loaded)
    // so the head + composer reflect the session's real vendor/protocol instead
    // of the claude default. The next send cold-resumes it server-side.
    for (const list of Object.values(visibleHistoryByProject)) {
      const h = list.find((x) => x.sid === sid);
      if (h) return historyToSummary(h);
    }
    return null;
  }, [railSessions, visibleHistoryByProject, sid]);

  const displayName = (settings.displayName || "").trim() || me?.handle || "user";
  const initial = displayName.slice(0, 1).toUpperCase() || "C";

  // Sidebar ⋯ menu → remove a project FROM CCTEAM (deregister + stop its live
  // sessions; disk untouched). Resolves true on success so the type-to-confirm
  // dialog closes; errors toast and keep it open. The rail refresh also drops
  // any of the project's rows the stop just ended.
  const removeProject = useCallback(
    async (slug: string): Promise<boolean> => {
      try {
        const res = await deleteProject(slug);
        const stopped = res.sessions_stopped.length;
        toastBus.handler?.info(
          lang === "en"
            ? `Removed ${slug} from ccteam (${stopped} live session${stopped === 1 ? "" : "s"} stopped) — files on disk untouched.`
            : `已从 ccteam 移除 ${slug}(停止 ${stopped} 个 live 会话)—— 磁盘文件未动。`,
        );
        projectsStore.refresh();
        return true;
      } catch (e) {
        if (!(e instanceof Error && e.message === "UNAUTHENTICATED")) {
          toastBus.handler?.error(
            `${lang === "en" ? "Remove failed" : "移除失败"}: ${e instanceof Error ? e.message : "unknown"}`,
          );
        }
        return false;
      }
    },
    [lang],
  );

  return (
    <div className="app" data-testid="app-shell">
      <Sidebar
        lang={lang}
        collapsed={collapsed}
        mobileOpen={mobileOpen}
        activeSid={view === "conv" ? sid : null}
        projects={projects}
        projectHosts={projectHosts}
        projectPaths={projectPaths}
        rows={rows}
        query={query}
        flowActive={view === "flow"}
        settingsActive={view === "settings"}
        teamActive={view === "agents"}
        dshActive={view === "dsh"}
        userName={displayName}
        userInitial={initial}
        avatarColor={settings.avatar}
        searchRef={searchRef}
        onQuery={setQuery}
        onCollapse={setSideCollapsed}
        onNewSession={() => goHome(null)}
        onNewInProject={(p) => goHome(p)}
        onOpenFlow={() => {
          navigate("/flow");
          closeMobile();
        }}
        onOpenSettings={() => {
          navigate("/settings");
          closeMobile();
        }}
        onOpenTeam={() => {
          navigate("/agents");
          closeMobile();
        }}
        onOpenDsh={() => {
          navigate("/dsh");
          closeMobile();
        }}
        onOpenRow={openRow}
        onStopRow={stopRow}
        onRenameRow={renameRow}
        onRemoveProject={removeProject}
      />

      {/* 移动端:抽屉入口 + 遮罩 (prototype .hamb / .side-backdrop) */}
      <button
        type="button"
        className="hamb"
        aria-label="menu"
        data-testid="hamb"
        onClick={() => setMobileOpen(true)}
      >
        <Menu />
      </button>
      <button
        type="button"
        className={`side-backdrop ${mobileOpen ? "show" : ""}`}
        aria-label="close menu"
        data-testid="side-backdrop"
        onClick={closeMobile}
      />

      <main className="main">
        {view === "conv" && sid ? (
          // KEY={sid}: fresh SessionView per switch — per-sid state resets atomically.
          <SessionView
            key={sid}
            sid={sid}
            session={activeSession}
            lang={lang}
            onRename={renameRow}
          />
        ) : view === "flow" ? (
          <WorkflowView
            tab={routeTab}
            onNav={(t) => navigate(`/flow/${t}`)}
            onOpenMarket={() => navigate("/flow/market")}
            lang={lang}
          />
        ) : view === "settings" ? (
          <SettingsView
            tab={routeTab}
            onNav={(t) => navigate(`/settings/${t}`)}
          />
        ) : view === "agents" ? (
          <AgentsView lang={lang} />
        ) : view === "dsh" ? (
          <DshView lang={lang} />
        ) : (
          <HomeView
            lang={lang}
            projects={projects}
            projectPaths={projectPaths}
            projectHosts={projectHosts}
            projectBranches={projectBranches}
            initialProject={homeProject}
            onLaunched={(newSid) => {
              projectsStore.refresh();
              void refreshSessions();
              navigate(`/chat/s/${encodeURIComponent(newSid)}`);
            }}
            onOpenSettings={(t) => navigate(`/settings/${t}`)}
          />
        )}
      </main>
    </div>
  );
}
