// v0.8.24 Track A — the SPA shell, rebuilt to the prototype
// (docs-local/versions/v0-8-24/ui-prototype.html): `.app` = sidebar + main,
// NO full-width top bar, four mutually-exclusive views:
//
//   Home (`/`)                — landing page; the session is lazy-created on
//                               the first message (HomeView).
//   Conversation (`/chat/s/:sid`) — the per-sid chat/terminal (SessionView,
//                               keyed by sid → atomic per-sid state reset).
//   工作流 (`/flow/:tab?`)     — Skills / Roles / MCP / 自进化 / Compare
//                               (WorkflowView, set-nav layout).
//   设置 (`/settings/:tab?`)   — 主机 / 插件市场 / Status / IM / 通用 / 账号
//                               (SettingsView, set-nav layout; admin-gated
//                               panels stay fail-closed via useMe).
//
// The sidebar (Sidebar.tsx) is the single navigation axis: ⌘K search,
// 「新建会话」→ Home, 「工作流」, per-project session groups (live + stopped
// history rows — clicking a stopped row RESUMES it), bottom 「设置」 + user.
// Collapsible to a 64px icon rail on desktop; a fixed drawer + backdrop +
// floating hamburger ≤820px (prototype breakpoints, CSS-driven).

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useLocation, useNavigate, useParams } from "react-router-dom";
import { Menu } from "lucide-react";
import HomeView, { type RecentEntry } from "./HomeView";
import SessionView from "./SessionView";
import WorkflowView from "./WorkflowView";
import SettingsView from "./SettingsView";
import { Sidebar, type RailRow } from "../components/Sidebar";
import { fetchDashboard } from "../lib/dashboardApi";
import {
  listHistorySessions,
  listSessions,
  resumeSession,
  stopSession as apiStopSession,
  type HistorySessionView,
  type SessionView as SessionSummary,
} from "../lib/sessionsApi";
import { toastBus } from "../lib/toastBus";
import { tStopped } from "../lib/i18n";
import { useWebSettings } from "../hooks/useWebSettings";
import { useMe } from "../hooks/useMe";
import { railSessionLabel } from "./railHelpers";
import { mergeProjectSlugs } from "./projectList";

type ShellView = "home" | "conv" | "flow" | "settings";

// eslint-disable-next-line react-refresh/only-export-components -- pure helper co-located for unit tests.
export function shellViewFor(pathname: string): ShellView {
  if (pathname.startsWith("/chat/s/")) return "conv";
  if (pathname.startsWith("/flow")) return "flow";
  if (pathname.startsWith("/settings")) return "settings";
  return "home";
}

/** How many stopped (history) sessions each project contributes to the rail
 *  + recents — enough to resume recent work without drowning the live rows. */
const HISTORY_PER_PROJECT = 6;

export default function ChatConsole() {
  const { sid: routeSid, tab: routeTab } = useParams<{ sid: string; tab: string }>();
  const sid = routeSid ?? null;
  const navigate = useNavigate();
  const location = useLocation();
  const view = shellViewFor(location.pathname);
  const { settings } = useWebSettings();
  const lang = settings.language;
  const { me, isAdmin } = useMe();

  // ---- cross-project session data (live + stopped history) -----------------
  const [railSessions, setRailSessions] = useState<SessionSummary[]>([]);
  const [historyByProject, setHistoryByProject] = useState<Record<string, HistorySessionView[]>>(
    {},
  );
  const [registeredProjects, setRegisteredProjects] = useState<string[]>([]);
  const [projectPaths, setProjectPaths] = useState<Record<string, string>>({});
  // v0.8.24 Q7 — read-only branch per project (absent = not a git repo).
  const [projectBranches, setProjectBranches] = useState<Record<string, string>>({});

  const refreshSessions = useCallback(async () => {
    try {
      const projects = await fetchDashboard();
      const slugs = projects.map((p) => p.slug);
      setRegisteredProjects(slugs);
      setProjectPaths(Object.fromEntries(projects.map((p) => [p.slug, p.path])));
      setProjectBranches(
        Object.fromEntries(
          projects
            .filter((p) => p.current_branch)
            .map((p) => [p.slug, p.current_branch as string]),
        ),
      );
      const lists = await Promise.all(
        slugs.map((slug) => listSessions(slug).catch(() => [] as SessionSummary[])),
      );
      setRailSessions(lists.flat());
      const historyLists = await Promise.all(
        slugs.map((slug) =>
          listHistorySessions(slug)
            .then((rows) => [slug, rows.slice(0, HISTORY_PER_PROJECT)] as const)
            .catch(() => [slug, [] as HistorySessionView[]] as const),
        ),
      );
      setHistoryByProject(Object.fromEntries(historyLists));
    } catch (e) {
      if (e instanceof Error && e.message === "UNAUTHENTICATED") return;
      // Best-effort: the shell renders with whatever resolved.
    }
  }, []);

  useEffect(() => {
    queueMicrotask(() => {
      void refreshSessions();
    });
  }, [refreshSessions]);

  // Pick up out-of-band projects/sessions (CLI `ccteam init`) on tab focus.
  useEffect(() => {
    const onFocus = () => {
      void refreshSessions();
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [refreshSessions]);

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
      host: s.host,
      status: s.status,
    }));
    const hist: RailRow[] = Object.values(historyByProject)
      .flat()
      .filter((h) => !liveSids.has(h.sid))
      .map((h) => ({
        sid: h.sid,
        project: h.slug,
        label: railSessionLabel(h),
        vendor: h.vendor,
        host: undefined,
        status: "off",
        history: true,
      }));
    return [...live, ...hist];
  }, [railSessions, historyByProject, liveSids]);

  // ---- Home recents: live + stopped, most recent first ---------------------
  const recents: RecentEntry[] = useMemo(() => {
    const live: RecentEntry[] = railSessions.map((s) => ({
      sid: s.sid,
      label: railSessionLabel(s),
      project: s.project,
      vendor: s.vendor,
      host: s.host,
      status: s.status,
      lastActive: s.last_active ?? s.created_at,
    }));
    const hist: RecentEntry[] = Object.values(historyByProject)
      .flat()
      .filter((h) => !liveSids.has(h.sid))
      .map((h) => ({
        sid: h.sid,
        label: railSessionLabel(h),
        project: h.slug,
        vendor: h.vendor,
        history: true,
        lastActive: h.last_active || h.created_at,
      }));
    const ts = (e: RecentEntry) => (e.lastActive ? Date.parse(e.lastActive) || 0 : 0);
    return [...live, ...hist].sort((a, b) => ts(b) - ts(a));
  }, [railSessions, historyByProject, liveSids]);

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
            void refreshSessions();
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
    [navigate, refreshSessions, closeMobile],
  );

  const stopRow = useCallback(
    (row: { sid: string }) => {
      apiStopSession(row.sid)
        .then(() => {
          toastBus.handler?.info(tStopped(lang, row.sid));
          void refreshSessions();
          if (row.sid === sid) navigate("/");
        })
        .catch((e) => {
          toastBus.handler?.error(`Stop failed: ${e instanceof Error ? e.message : e}`);
        });
    },
    [refreshSessions, lang, sid, navigate],
  );

  const activeSession = useMemo(
    () => railSessions.find((s) => s.sid === sid) ?? null,
    [railSessions, sid],
  );

  const displayName = (settings.displayName || "").trim() || me?.handle || "user";
  const initial = displayName.slice(0, 1).toUpperCase() || "C";

  return (
    <div className="app" data-testid="app-shell">
      <Sidebar
        lang={lang}
        collapsed={collapsed}
        mobileOpen={mobileOpen}
        activeSid={view === "conv" ? sid : null}
        projects={projects}
        rows={rows}
        query={query}
        flowActive={view === "flow"}
        settingsActive={view === "settings"}
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
        onOpenRow={openRow}
        onStopRow={stopRow}
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
          <SessionView key={sid} sid={sid} session={activeSession} lang={lang} isAdmin={isAdmin} />
        ) : view === "flow" ? (
          <WorkflowView
            tab={routeTab}
            onNav={(t) => navigate(`/flow/${t}`)}
            onOpenMarket={() => navigate("/settings/market")}
            lang={lang}
            isAdmin={isAdmin}
          />
        ) : view === "settings" ? (
          <SettingsView
            tab={routeTab}
            onNav={(t) => navigate(`/settings/${t}`)}
            rail={railSessions}
          />
        ) : (
          <HomeView
            lang={lang}
            isAdmin={isAdmin}
            projects={projects}
            projectPaths={projectPaths}
            projectBranches={projectBranches}
            liveCount={railSessions.length}
            recents={recents}
            initialProject={homeProject}
            onLaunched={(newSid) => {
              void refreshSessions();
              navigate(`/chat/s/${encodeURIComponent(newSid)}`);
            }}
            onOpenRecent={(entry) => openRow(entry)}
            onOpenSettings={(t) => navigate(`/settings/${t}`)}
          />
        )}
      </main>
    </div>
  );
}
