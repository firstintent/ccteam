// v0.8.24 Track A — the Home landing page (prototype `#view-home`), replacing
// the retired NewSessionModal.
//
// 「开工吧!」 + ctx-bar (项目 · 主机 · 分支(只读, v0.8.24 Q7 — hidden for
// non-git projects, never mocked) · 角色) sitting flush on the composer, and
// the 最近会话 two-column card grid.
//
// LAZY-CREATE: the session is created when the FIRST message is sent —
// POST /projects (only for an inline 「＋ 新建项目…」 path) → POST
// /projects/{slug}/sessions (vendor/protocol/host/hitl/role + v0.8.24 A-U3
// model/effort — the create form now carries them vendor-natively, replacing
// the old post-spawn `/model` control turn) → POST the user's text as the
// first turn → navigate to the Conversation view.

import { useEffect, useRef, useState } from "react";
import { Folder, GitBranch, Globe } from "lucide-react";
import { ChatComposer } from "../components/ChatComposer";
import { toastBus } from "../lib/toastBus";
import { makeT, type Lang } from "../lib/i18n";
import {
  defaultDraft,
  modelSwitchFor,
  normalizeDraft,
  slugFromPath,
  statusDotClass,
  vendorChipClass,
  wireEffort,
  wireProtocol,
  type ComposerDraft,
} from "../lib/vendors";
import { createProject as apiCreateProject } from "../lib/dashboardApi";
import {
  createSession as apiCreateSession,
  listProjectRoles,
  submitTurn,
} from "../lib/sessionsApi";
import { getHosts, type HostSummary } from "../lib/hostsApi";
import { relativeTime } from "./railHelpers";

/** One 最近会话 card (live or resumable history) the shell feeds in. */
export interface RecentEntry {
  sid: string;
  label: string;
  project: string;
  vendor: string;
  host?: string;
  status?: string | null;
  history?: boolean;
  lastActive?: string;
}

/** Frontend-only soft cap on concurrently-live sessions (UX guard). */
export const MAX_ACTIVE_SESSIONS = 10;

const MODEL_DRAFT_KEY = "ccteam.home.model.v1";

function loadModelDraft(): ComposerDraft {
  try {
    const raw = localStorage.getItem(MODEL_DRAFT_KEY);
    if (raw) return normalizeDraft({ ...defaultDraft(), ...JSON.parse(raw) });
  } catch {
    /* fall through */
  }
  return defaultDraft();
}

/** ctx-bar dropdown built on the prototype `.sel` pattern. */
function CtxSelect({
  icon,
  value,
  title,
  right,
  children,
  testId,
}: {
  icon?: React.ReactNode;
  value: React.ReactNode;
  title: string;
  right?: boolean;
  children: (close: () => void) => React.ReactNode;
  testId?: string;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement | null>(null);
  useEffect(() => {
    if (!open) return;
    const close = (e: MouseEvent) => {
      if (ref.current && e.target instanceof Node && ref.current.contains(e.target)) return;
      setOpen(false);
    };
    document.addEventListener("click", close);
    return () => document.removeEventListener("click", close);
  }, [open]);
  const body = (
    <div className={`sel ${open ? "open" : ""}`} ref={ref} data-testid={testId}>
      <button
        type="button"
        className="ctx-btn"
        title={title}
        onClick={(e) => {
          e.stopPropagation();
          setOpen((o) => !o);
        }}
      >
        {icon}
        <span className="v">{value}</span>
      </button>
      <div className="sel-menu">{children(() => setOpen(false))}</div>
    </div>
  );
  return right ? <div className="right">{body}</div> : body;
}

export default function HomeView({
  lang,
  isAdmin,
  projects,
  projectPaths,
  projectBranches = {},
  liveCount,
  recents,
  initialProject,
  onLaunched,
  onOpenRecent,
  onOpenSettings,
}: {
  lang: Lang;
  isAdmin: boolean;
  projects: string[];
  projectPaths: Record<string, string>;
  /** v0.8.24 Q7 — current git branch per slug (absent ⇒ hide the dimension). */
  projectBranches?: Record<string, string>;
  /** Caller's live session count (soft cap gate). */
  liveCount: number;
  recents: RecentEntry[];
  /** Pre-picked project (sidebar 「在此工作区新建」). */
  initialProject?: string | null;
  onLaunched: (sid: string) => void;
  onOpenRecent: (entry: RecentEntry) => void;
  onOpenSettings: (tab: string) => void;
}) {
  const t = makeT(lang);
  // The picked project is DERIVED against the (async-resolving) project list:
  // an explicit pick wins while valid; else the sidebar's 「在此工作区新建」
  // pre-pick; else the first project. No validity-sync effect needed.
  const [picked, setPicked] = useState<string | null>(initialProject ?? null);
  const project =
    picked && projects.includes(picked)
      ? picked
      : initialProject && projects.includes(initialProject)
        ? initialProject
        : (projects[0] ?? "");
  const [newProjectPath, setNewProjectPath] = useState<string | null>(null);
  const [newProjOpen, setNewProjOpen] = useState(false);
  const newProjRef = useRef<HTMLInputElement | null>(null);
  const [role, setRole] = useState<string>("");
  // Roles keyed by project so a project switch needs no synchronous reset.
  const [rolesByProject, setRolesByProject] = useState<Record<string, string[]>>({});
  const roles = (project && !newProjectPath ? rolesByProject[project] : undefined) ?? [];
  const [hosts, setHosts] = useState<HostSummary[] | null>(null);
  const [host, setHost] = useState<string>("local");
  const [draft, setDraft] = useState<ComposerDraft>(() => loadModelDraft());
  const [pending, setPending] = useState(false);

  // Persist the model/effort/protocol/hitl draft.
  useEffect(() => {
    try {
      localStorage.setItem(MODEL_DRAFT_KEY, JSON.stringify(draft));
    } catch {
      /* ignore */
    }
  }, [draft]);

  // Roles of the selected (existing) project — the ctx-bar 角色 menu.
  useEffect(() => {
    if (!project) return;
    let cancelled = false;
    listProjectRoles(project)
      .then((rs) => {
        if (!cancelled)
          setRolesByProject((cur) => ({ ...cur, [project]: rs.map((r) => r.role) }));
      })
      .catch(() => {
        /* best-effort — the 角色 menu just shows 无 role + market entry */
      });
    return () => {
      cancelled = true;
    };
  }, [project]);

  // Hosts (admin data): tenants/errors gracefully HIDE the dimension.
  useEffect(() => {
    let cancelled = false;
    getHosts()
      .then((res) => {
        if (!cancelled) setHosts(res.hosts);
      })
      .catch(() => {
        if (!cancelled) setHosts(null);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const openNewProject = () => {
    setNewProjOpen(true);
    window.setTimeout(() => newProjRef.current?.focus(), 30);
  };

  const cancelNewProject = () => {
    setNewProjOpen(false);
    setNewProjectPath(null);
    if (newProjRef.current) newProjRef.current.value = "";
  };

  // ---- lazy-create funnel ---------------------------------------------------
  const launch = (text: string): boolean => {
    if (pending) return false;
    if (!project && !newProjectPath) {
      toastBus.handler?.error(
        lang === "en" ? "Pick a project first (＋ New project…)" : "先选一个项目(＋ 新建项目…)",
      );
      return false;
    }
    if (liveCount >= MAX_ACTIVE_SESSIONS) {
      toastBus.handler?.error(
        lang === "en"
          ? "Max 10 active sessions — stop others first"
          : "最多 10 个活跃 session,请先停掉其他",
      );
      return false;
    }
    setPending(true);
    const run = async () => {
      let slug = project;
      if (newProjectPath) {
        const derived = slugFromPath(newProjectPath);
        if (!derived) {
          throw new Error(lang === "en" ? "invalid project path" : "项目路径无效");
        }
        const created = await apiCreateProject(derived, newProjectPath.trim());
        slug = created.slug;
      }
      // v0.8.24 A-U3 — an explicit model/effort pick rides the create form
      // (vendor-native spawn seam), replacing the old post-spawn `/model`
      // control turn.
      const { sid, model_warning: warning } = await apiCreateSession(slug, {
        role,
        vendor: draft.vendor,
        permission_mode: draft.hitl ? "hitl" : "skip",
        protocol: wireProtocol(draft),
        host,
        model: modelSwitchFor(draft) ?? undefined,
        effort: wireEffort(draft) ?? undefined,
      });
      if (warning) toastBus.handler?.info(warning);
      await submitTurn(sid, text);
      return sid;
    };
    run()
      .then((sid) => {
        setPending(false);
        cancelNewProject();
        onLaunched(sid);
      })
      .catch((e) => {
        setPending(false);
        if (e instanceof Error && e.message === "UNAUTHENTICATED") return;
        toastBus.handler?.error(
          `${lang === "en" ? "Launch failed" : "启动失败"}: ${e instanceof Error ? e.message : "unknown"}`,
        );
      });
    return true;
  };

  const projLabel = newProjectPath ? (
    <span style={{ color: "#0E7490" }}>{slugFromPath(newProjectPath) || "…"} (new)</span>
  ) : (
    project || t("newProject")
  );

  const pickedHost = hosts?.find((x) => x.host === host);
  const hostLabel = !pickedHost
    ? `local · ${t("localTag")}`
    : pickedHost.is_local
      ? `${pickedHost.hostname} · ${t("localTag")}`
      : pickedHost.hostname;

  return (
    <section className="view active home-view" data-testid="home-view">
      <div className="home-inner fade-in">
        <div className="home-title">
          <h1>{t("homeTitle")}</h1>
          <p>{t("homeSub")}</p>
        </div>

        <div className="composer-group">
          <div className="ctx-bar" data-testid="ctx-bar">
            <CtxSelect
              icon={<Folder />}
              value={projLabel}
              title={t("project")}
              testId="ctx-project"
            >
              {(close) => (
                <>
                  {projects.map((p) => (
                    <button
                      key={p}
                      type="button"
                      className={`sel-item ${!newProjectPath && project === p ? "selected" : ""}`}
                      title={projectPaths[p]}
                      onClick={() => {
                        setPicked(p);
                        setNewProjectPath(null);
                        setNewProjOpen(false);
                        close();
                      }}
                    >
                      {p}
                      {projectPaths[p] ? <span className="sub">{projectPaths[p]}</span> : null}
                      <span className="check">✓</span>
                    </button>
                  ))}
                  <button
                    type="button"
                    className={`sel-item new ${newProjectPath ? "selected" : ""}`}
                    onClick={() => {
                      openNewProject();
                      close();
                    }}
                  >
                    {t("newProject")}
                    <span className="check">✓</span>
                  </button>
                </>
              )}
            </CtxSelect>

            {hosts && hosts.length > 0 ? (
              <CtxSelect icon={<Globe />} value={hostLabel} title={t("host")} testId="ctx-host">
                {(close) => (
                  <>
                    {hosts.map((h) => (
                      <button
                        key={h.host}
                        type="button"
                        className={`sel-item ${host === h.host ? "selected" : ""}`}
                        onClick={() => {
                          setHost(h.host);
                          close();
                        }}
                      >
                        <span className="dot on" />
                        {h.hostname}
                        {h.is_local ? ` · ${t("localTag")}` : ""}
                        <span className="check">✓</span>
                      </button>
                    ))}
                    <button
                      type="button"
                      className="sel-item new"
                      onClick={() => {
                        onOpenSettings("hosts");
                        close();
                      }}
                    >
                      {t("connectHost")}
                      <span className="check">✓</span>
                    </button>
                  </>
                )}
              </CtxSelect>
            ) : null}

            {/* v0.8.24 Q7 — 分支 dimension: READ-ONLY display of the project's
                current git branch (.git/HEAD, server-side best-effort); hidden
                for non-git projects and for a not-yet-created project. */}
            {!newProjectPath && project && projectBranches[project] ? (
              <span
                className="ctx-btn"
                data-testid="ctx-branch"
                title={t("branch")}
                style={{ cursor: "default" }}
              >
                <GitBranch />
                <span className="v">{projectBranches[project]}</span>
              </span>
            ) : null}

            {/* 角色 — admin-only beta surface (v0.8.20 F4, AGENTS.md §五.8):
                a tenant always launches roleless. */}
            {isAdmin ? (
            <CtxSelect
              value={
                <>
                  {role || t("noRole")}
                  <span className="dot on" style={{ marginLeft: 7 }} />
                </>
              }
              title={t("role")}
              right
              testId="ctx-role"
            >
              {(close) => (
                <>
                  <button
                    type="button"
                    className={`sel-item ${role === "" ? "selected" : ""}`}
                    onClick={() => {
                      setRole("");
                      close();
                    }}
                  >
                    {t("noRole")}
                    <span className="check">✓</span>
                  </button>
                  {roles.map((r) => (
                    <button
                      key={r}
                      type="button"
                      className={`sel-item ${role === r ? "selected" : ""}`}
                      onClick={() => {
                        setRole(r);
                        close();
                      }}
                    >
                      {r}
                      {r === "cto" ? <span className="sub">{t("ctoSub")}</span> : null}
                      <span className="check">✓</span>
                    </button>
                  ))}
                  <button
                    type="button"
                    className="sel-item new"
                    onClick={() => {
                      onOpenSettings("market");
                      close();
                    }}
                  >
                    {t("installFromMarket")}
                    <span className="check">✓</span>
                  </button>
                </>
              )}
            </CtxSelect>
            ) : null}
          </div>

          <ChatComposer
            draftKey="home"
            lang={lang}
            placeholderKey="inputPh"
            disabled={pending}
            isAdmin={isAdmin}
            draft={draft}
            onDraftChange={setDraft}
            onSend={launch}
            sendTestId="home-send"
            topSlot={
              <div className={`newproj ${newProjOpen ? "show" : ""}`} data-testid="newproj">
                <label htmlFor="newproj-path">{t("newProjLabel")}</label>
                <input
                  id="newproj-path"
                  ref={newProjRef}
                  placeholder="~/work/my-app"
                  spellCheck={false}
                  onChange={(e) => setNewProjectPath(e.target.value.trim() || null)}
                />
                <button type="button" className="x" onClick={cancelNewProject} aria-label="cancel">
                  ✕
                </button>
              </div>
            }
          />
          {pending ? (
            <p style={{ textAlign: "center", marginTop: 10, fontSize: 12.5, color: "var(--text-faint)" }}>
              {t("starting")}
            </p>
          ) : null}
        </div>

        <div className="recent">
          <h3>{t("recent")}</h3>
          <div className="recent-grid" data-testid="recent-grid">
            {recents.slice(0, 4).map((c) => (
              <button
                key={c.sid}
                type="button"
                className="conv-card"
                onClick={() => onOpenRecent(c)}
                title={c.history ? t("resumeTip") : c.sid}
              >
                <div className="t">
                  <span className={statusDotClass(c.status, { off: c.history })} />
                  <span className="name">{c.label}</span>
                </div>
                <div className="m">
                  <span>{c.project}</span>
                  <span className={vendorChipClass(c.vendor)}>{c.vendor}</span>
                  <span className="chip sid">{c.sid}</span>
                  <span style={{ marginLeft: "auto" }} className="mono">
                    {c.host ?? "local"} · {relativeTime(lang, c.lastActive)}
                  </span>
                </div>
              </button>
            ))}
          </div>
        </div>
      </div>
    </section>
  );
}
