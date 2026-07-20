// v0.8.24 Track A — the Home landing page (prototype `#view-home`), replacing
// the retired NewSessionModal.
//
// 「开工吧!」 + ctx-bar (项目 · 主机 · 分支(只读, v0.8.24 Q7 — hidden for
// non-git projects, never mocked) · 角色) sitting flush on the composer, and
// the 快速开始 two-column template grid (picking a card prefills the
// composer — recents live in the sidebar rail, not here).
//
// LAZY-CREATE: the session is created when the FIRST message is sent —
// POST /projects (only for an inline 「＋ 新建项目…」 path) → POST
// /projects/{slug}/sessions (vendor/protocol/host/hitl/role + v0.8.24 A-U3
// model/effort — the create form now carries them vendor-natively, replacing
// the old post-spawn `/model` control turn) → POST the user's text as the
// first turn → navigate to the Conversation view.

import { useEffect, useRef, useState } from "react";
import {
  Code,
  Folder,
  GitBranch,
  Globe,
  Layers,
  Scale,
  ShieldCheck,
  Users,
  Zap,
} from "lucide-react";
import { ChatComposer } from "../components/ChatComposer";
import type { TurnAttachment } from "../lib/attachmentsApi";
import { VendorChip } from "../components/VendorChip";
import { toastBus } from "../lib/toastBus";
import { makeT, tRemoteProjectPath, type Lang } from "../lib/i18n";
import {
  defaultDraft,
  modelSwitchFor,
  normalizeDraft,
  slugFromPath,
  wireEffort,
  wireProtocol,
  type ComposerDraft,
  type VendorId,
} from "../lib/vendors";
import { createProject as apiCreateProject } from "../lib/dashboardApi";
import {
  createSession as apiCreateSession,
  listProjectRoles,
  submitTurn,
} from "../lib/sessionsApi";
import { getHostDetail, getHosts, type HostDetail, type HostSummary } from "../lib/hostsApi";
import { allowedVendorsFor, eligibleHosts } from "../lib/hostFilter";

/** 快速开始 template cards (the grid replacing the old 最近会话 recents —
 *  those live in the sidebar rail). The set showcases ccteam's headline
 *  feature — multi-vendor delegation: the 协作 flagship, the face-off and
 *  cross-review moves, plus each harness on its strength (codex codes
 *  steady / grok is fastest / kimi is cost-effective):
 *  a click prefills the composer with `<key>P` from the i18n table (`<key>T`
 *  = title, `<key>D` = description) AND switches the model draft to
 *  `vendors[0]` so the session really spawns on that harness (lazy-create
 *  untouched — the session is born on send). `vendors` also renders as
 *  brand chips on the card; the 协作 flagship wears all four and leaves the
 *  actual fan-out to session_spawn/dispatch from the claude brain. */
const TEMPLATES: ReadonlyArray<{
  id: string;
  key: string;
  Icon: typeof Users;
  vendors: readonly VendorId[];
}> = [
  { id: "team", key: "tplTeam", Icon: Users, vendors: ["claude", "codex", "grok", "kimi"] },
  { id: "compare", key: "tplCompare", Icon: Scale, vendors: ["claude", "codex", "grok"] },
  { id: "review", key: "tplReview", Icon: ShieldCheck, vendors: ["claude", "codex"] },
  { id: "code", key: "tplCode", Icon: Code, vendors: ["codex"] },
  { id: "fast", key: "tplFast", Icon: Zap, vendors: ["grok"] },
  { id: "bulk", key: "tplBulk", Icon: Layers, vendors: ["kimi"] },
];

export interface ProjectHostIdentity {
  host: string;
  online: boolean;
}

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

/** SSR-safe new-project path + host controls. Host options are already
 * filtered by `eligibleHosts`; this component only renders the choice. */
export function NewProjectFields({
  lang,
  open,
  hosts,
  host,
  inputRef,
  onHostChange,
  onPathChange,
  onCancel,
}: {
  lang: Lang;
  open: boolean;
  hosts: HostSummary[];
  host: string;
  inputRef?: React.Ref<HTMLInputElement>;
  onHostChange: (host: string) => void;
  onPathChange: (path: string) => void;
  onCancel: () => void;
}) {
  const t = makeT(lang);
  const remote = host !== "local";
  return (
    <div className={`newproj ${open ? "show" : ""}`} data-testid="newproj">
      <label htmlFor="newproj-path">{t("newProjLabel")}</label>
      <input
        id="newproj-path"
        ref={inputRef}
        placeholder={remote ? tRemoteProjectPath(lang, host) : "~/work/my-app"}
        spellCheck={false}
        onChange={(event) => onPathChange(event.target.value.trim())}
      />
      <select
        className="newproj-host"
        data-testid="newproj-host"
        title={t("host")}
        value={host}
        onChange={(event) => onHostChange(event.target.value)}
      >
        {hosts.map((option) => (
          <option key={option.host} value={option.host}>
            {option.host}{option.is_local ? ` · ${t("localTag")}` : ""}
          </option>
        ))}
      </select>
      <button type="button" className="x" onClick={onCancel} aria-label={t("cancel")}>
        ✕
      </button>
    </div>
  );
}

export default function HomeView({
  lang,
  isAdmin,
  projects,
  projectPaths,
  projectHosts = {},
  projectBranches = {},
  initialProject,
  onLaunched,
  onOpenSettings,
}: {
  lang: Lang;
  isAdmin: boolean;
  projects: string[];
  projectPaths: Record<string, string>;
  projectHosts?: Record<string, ProjectHostIdentity>;
  /** v0.8.24 Q7 — current git branch per slug (absent ⇒ hide the dimension). */
  projectBranches?: Record<string, string>;
  /** Pre-picked project (sidebar 「在此工作区新建」). */
  initialProject?: string | null;
  onLaunched: (sid: string) => void;
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
  const [hostDetails, setHostDetails] = useState<Record<string, HostDetail | null>>({});
  const [host, setHost] = useState<string>("local");
  const [draft, setDraft] = useState<ComposerDraft>(() => loadModelDraft());
  const [pending, setPending] = useState(false);
  // 快速开始 template pick → composer draft text (bump-nonce channel).
  const [prefill, setPrefill] = useState({ text: "", nonce: 0 });

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

  // Hosts (admin data): tenants/errors gracefully HIDE the dimension. Details
  // (per-host agent probe + registered projects) drive the project→host and
  // host→vendor binding below; a failed detail marks the host not spawnable.
  useEffect(() => {
    let cancelled = false;
    getHosts()
      .then(async (res) => {
        if (cancelled) return;
        setHosts(res.hosts);
        const pairs = await Promise.all(
          res.hosts.map((h) =>
            getHostDetail(h.host)
              .then((d) => [h.host, d] as const)
              .catch(() => [h.host, null] as const),
          ),
        );
        if (!cancelled) setHostDetails(Object.fromEntries(pairs));
      })
      .catch(() => {
        if (!cancelled) setHosts(null);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const isNewProject = newProjOpen;
  const boundHost = projectHosts[project]?.host ?? "local";

  // Existing projects inherit exactly their bound host. A new project can be
  // created on any online host with at least one installed harness.
  const spawnableHosts = hosts
    ? eligibleHosts(hosts, hostDetails, boundHost, isNewProject)
    : null;
  const effectiveHost = !isNewProject
    ? boundHost
    : !spawnableHosts || spawnableHosts.some((candidate) => candidate.host === host)
      ? host
      : (spawnableHosts[0]?.host ?? "local");
  const newProjectHosts = spawnableHosts ?? [{
    host: "local",
    hostname: "local",
    is_local: true,
    status: "online",
    agent_count: 0,
    agents_ready: 0,
  }];

  // 主机绑定 vendor: the composer only offers harnesses installed on the
  // effective host (null = unknown → don't filter); a pick the host can't
  // run is normalized to the host's first installed vendor, derived too.
  const hostVendors = allowedVendorsFor(hostDetails[effectiveHost]);
  const effectiveDraft =
    hostVendors && !hostVendors.includes(draft.vendor)
      ? normalizeDraft({ ...draft, vendor: hostVendors[0]! })
      : draft;

  const openNewProject = () => {
    setHost("local");
    setRole("");
    setNewProjOpen(true);
    window.setTimeout(() => newProjRef.current?.focus(), 30);
  };

  const cancelNewProject = () => {
    setNewProjOpen(false);
    setNewProjectPath(null);
    if (newProjRef.current) newProjRef.current.value = "";
  };

  // ---- lazy-create funnel ---------------------------------------------------
  const launch = (text: string, attachments: TurnAttachment[] = []): boolean => {
    if (pending) return false;
    if (!project && !isNewProject) {
      toastBus.handler?.error(
        lang === "en" ? "Pick a project first (＋ New project…)" : "先选一个项目(＋ 新建项目…)",
      );
      return false;
    }
    setPending(true);
    const run = async () => {
      let slug = project;
      if (isNewProject) {
        if (!newProjectPath) {
          throw new Error(t("newProjPathRequired"));
        }
        const derived = slugFromPath(newProjectPath);
        if (!derived) {
          throw new Error(lang === "en" ? "invalid project path" : "项目路径无效");
        }
        const created = await apiCreateProject(derived, newProjectPath.trim(), { host: effectiveHost });
        slug = created.slug;
      }
      // v0.8.24 A-U3 — an explicit model/effort pick rides the create form
      // (vendor-native spawn seam), replacing the old post-spawn `/model`
      // control turn.
      const { sid } = await apiCreateSession(slug, {
        role,
        vendor: effectiveDraft.vendor,
        permission_mode: effectiveDraft.hitl ? "hitl" : "skip",
        protocol: wireProtocol(effectiveDraft),
        model: modelSwitchFor(effectiveDraft) ?? undefined,
        effort: wireEffort(effectiveDraft) ?? undefined,
      });
      await submitTurn(sid, text, attachments);
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

  const projLabel = isNewProject ? (
    <span style={{ color: "#0E7490" }}>{slugFromPath(newProjectPath ?? "") || "…"} (new)</span>
  ) : (
    project || t("newProject")
  );

  const pickedHost = hosts?.find((x) => x.host === effectiveHost);
  const hostOnline = isNewProject
    ? effectiveHost === "local" || pickedHost?.status === "online"
    : (projectHosts[project]?.online ?? effectiveHost === "local");
  const hostLabel = !pickedHost
    ? effectiveHost
    : pickedHost.is_local
      ? `${pickedHost.hostname} · ${t("localTag")}`
      : `${pickedHost.hostname} @ ${pickedHost.host}`;
  const hostLabelWithStatus = hostOnline ? hostLabel : `${hostLabel} · ${t("offline")}`;

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
                  {projects.map((p) => {
                    const identity = projectHosts[p] ?? { host: "local", online: true };
                    return (
                      <button
                        key={p}
                        type="button"
                        disabled={!identity.online}
                        className={`sel-item ${!isNewProject && project === p ? "selected" : ""} ${identity.online ? "" : "offline"}`}
                        title={identity.online ? projectPaths[p] : `${projectPaths[p] ?? p} · ${t("offline")}`}
                        onClick={() => {
                          setPicked(p);
                          setNewProjectPath(null);
                          setNewProjOpen(false);
                          close();
                        }}
                      >
                        <span>{p}</span>
                        {identity.host !== "local" ? (
                          <span className="project-option-host">@ {identity.host}</span>
                        ) : null}
                        {projectPaths[p] || !identity.online ? (
                          <span className="sub">{identity.online ? projectPaths[p] : t("offline")}</span>
                        ) : null}
                        <span className="check">✓</span>
                      </button>
                    );
                  })}
                  <button
                    type="button"
                    className={`sel-item new ${isNewProject ? "selected" : ""}`}
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

            {/* Existing project host is read-only: project identity owns the
                execution location. New-project host choice lives with path. */}
            {!isNewProject && project ? (
              <span className="ctx-btn" data-testid="ctx-host" title={t("host")} style={{ cursor: "default" }}>
                <Globe />
                <span className={`dot ${hostOnline ? "on" : "off"}`} />
                <span className="v">{hostLabelWithStatus}</span>
              </span>
            ) : null}

            {/* v0.8.24 Q7 — 分支 dimension: READ-ONLY display of the project's
                current git branch (.git/HEAD, server-side best-effort); hidden
                for non-git projects and for a not-yet-created project. */}
            {!isNewProject && project && projectBranches[project] ? (
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
            {isAdmin && !(isNewProject && effectiveHost !== "local") ? (
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
            draft={effectiveDraft}
            onDraftChange={setDraft}
            allowedVendors={hostVendors ?? undefined}
            onSend={launch}
            sendTestId="home-send"
            uploadSlug={isNewProject ? undefined : project || undefined}
            prefill={prefill}
            topSlot={
              <NewProjectFields
                lang={lang}
                open={newProjOpen}
                hosts={newProjectHosts}
                host={effectiveHost}
                inputRef={newProjRef}
                onHostChange={setHost}
                onPathChange={(path) => setNewProjectPath(path || null)}
                onCancel={cancelNewProject}
              />
            }
          />
          {pending ? (
            <p style={{ textAlign: "center", marginTop: 10, fontSize: 12.5, color: "var(--text-faint)" }}>
              {t("starting")}
            </p>
          ) : null}
        </div>

        <div className="quickstart">
          <h3>{t("quickStart")}</h3>
          <div className="tpl-grid" data-testid="template-grid">
            {TEMPLATES.map(({ id, key, Icon, vendors }) => (
              <button
                key={id}
                type="button"
                className="tpl-card"
                data-testid={`tpl-${id}`}
                title={t(`${key}P`)}
                onClick={() => {
                  setPrefill((cur) => ({ text: t(`${key}P`), nonce: cur.nonce + 1 }));
                  // Vendor-strength cards also aim the spawn at their harness
                  // (host binding may still normalize, same as a manual pick).
                  const vendor = vendors[0];
                  if (vendor) setDraft((cur) => normalizeDraft({ ...cur, vendor }));
                }}
              >
                <div className="t">
                  <Icon />
                  <span className="name">{t(`${key}T`)}</span>
                  {vendors.length > 0 ? (
                    <span className="vs">
                      {vendors.map((v) => (
                        <VendorChip key={v} vendor={v} />
                      ))}
                    </span>
                  ) : null}
                </div>
                <div className="d">{t(`${key}D`)}</div>
              </button>
            ))}
          </div>
        </div>
      </div>
    </section>
  );
}
