// v0.8.24 Track A — the Home landing page (prototype `#view-home`), replacing
// the retired NewSessionModal.
//
// 「开工吧!」 + ctx-bar (项目 · 主机 · 分支(只读, v0.8.24 Q7 — hidden for
// non-git projects, never mocked) · 角色) sitting flush on the composer, and
// the 快速开始 two-column template grid (picking a card prefills the
// composer — recents live in the sidebar rail, not here).
//
// v0.9.11 TEAM-3: the template cards are the shared 编队起手 formation
// playbooks (`lib/playbooks.ts`, also rendered on the Team page 分工 tab);
// the Team page 起手 CTA lands here as one-shot router state
// `{ playbook: id }` and applies the SAME composer patch as a card click.
//
// LAZY-CREATE: the session is created when the FIRST message is sent —
// POST /projects (only for an inline 「＋ 新建项目…」 path) → POST
// /projects/{slug}/sessions (vendor/protocol/host/hitl/role + v0.8.24 A-U3
// model/effort — the create form now carries them vendor-natively, replacing
// the old post-spawn `/model` control turn) → POST the user's text as the
// first turn → navigate to the Conversation view.

import { useEffect, useRef, useState } from "react";
import { Folder, GitBranch, Globe } from "lucide-react";
import { useLocation, useNavigate } from "react-router-dom";
import { ChatComposer } from "../components/ChatComposer";
import { useVendorCatalog } from "../hooks/useVendorCatalog";
import type { TurnAttachment } from "../lib/attachmentsApi";
import { VendorChip } from "../components/VendorChip";
import { toastBus } from "../lib/toastBus";
import { makeT, tRemoteProjectPath, type Lang } from "../lib/i18n";
import {
  defaultDraft,
  effortSwitchFor,
  modelSwitchFor,
  normalizeDraft,
  slugFromPath,
  wireProtocol,
  type ComposerDraft,
} from "../lib/vendors";
import { createProject as apiCreateProject } from "../lib/dashboardApi";
import {
  createSession as apiCreateSession,
  listProjectRoles,
  submitTurn,
} from "../lib/sessionsApi";
import { getHostDetail, getHosts, type HostDetail, type HostSummary } from "../lib/hostsApi";
import { allowedVendorsFor, eligibleHosts } from "../lib/hostFilter";
import { applyPlaybook, playbookFromState, PLAYBOOKS } from "../lib/playbooks";

export interface ProjectHostIdentity {
  host: string;
  online: boolean;
}

const MODEL_DRAFT_KEY = "ccteam.home.model.v1";

/** The persisted draft is deliberately NOT normalized here: the live vendor
 *  catalog (`GET /api/v1/models`) has not loaded at mount, so a static-only
 *  normalize would wipe a perfectly good `kimi-code/k3` for looking unknown.
 *  Render-time normalization below — which sees the catalog — is the single
 *  validity gate (it also drops the retired `effortKey` of an older SPA's
 *  draft, degrading it to the vendor default rather than crashing). */
function loadModelDraft(): ComposerDraft {
  try {
    const raw = localStorage.getItem(MODEL_DRAFT_KEY);
    if (raw) return { ...defaultDraft(), ...(JSON.parse(raw) as Partial<ComposerDraft>) };
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
  projects,
  projectPaths,
  projectHosts = {},
  projectBranches = {},
  initialProject,
  onLaunched,
  onOpenSettings,
}: {
  lang: Lang;
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
  // What each installed vendor actually declares (models + effort tokens);
  // `{}` until it loads / on an older daemon → the static registry answers.
  const catalog = useVendorCatalog();
  const [pending, setPending] = useState(false);
  // 快速开始 template pick → composer draft text (bump-nonce channel).
  const [prefill, setPrefill] = useState({ text: "", nonce: 0 });
  const location = useLocation();
  const navigate = useNavigate();

  // ONE playbook-application path for both entries (card click here, Team
  // page 起手 handoff below): prefill the composer AND aim the spawn at the
  // formation's lead harness (host binding may still normalize, same as a
  // manual pick). Lazy-create untouched — the session is born on send.
  const pickPlaybook = (id: string) => {
    const patch = applyPlaybook(id, lang);
    if (!patch) return;
    setPrefill((cur) => ({ text: patch.text, nonce: cur.nonce + 1 }));
    setDraft((cur) => normalizeDraft({ ...cur, vendor: patch.vendor }, catalog));
  };

  // Team page 起手 CTA lands `{ state: { playbook: id } }` on `/`: apply it
  // exactly like a card click. setState-during-render with a change guard
  // (the ChatComposer prefill pattern) keyed on location.key applies each
  // handoff once; the effect only replace-clears the history entry so a
  // refresh / back-forward doesn't re-apply (one-shot; a StrictMode re-run
  // is benign — the same patch just bumps the prefill nonce again).
  const routedPlaybook = playbookFromState(location.state);
  const [appliedRouteKey, setAppliedRouteKey] = useState<string | null>(null);
  if (routedPlaybook && location.key !== appliedRouteKey) {
    setAppliedRouteKey(location.key);
    pickPlaybook(routedPlaybook);
  }
  useEffect(() => {
    if (playbookFromState(location.state)) navigate(location.pathname, { replace: true });
  }, [location.state, location.pathname, navigate]);

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

  // Hosts: load shared operational data; errors gracefully HIDE the dimension. Details
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
  // Normalizing on EVERY render (not just on a host-forced vendor swap) is
  // what lets the persisted draft stay raw until the catalog lands.
  const hostVendors = allowedVendorsFor(hostDetails[effectiveHost]);
  const effectiveDraft = normalizeDraft(
    hostVendors && !hostVendors.includes(draft.vendor)
      ? { ...draft, vendor: hostVendors[0]! }
      : draft,
    catalog,
  );

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
      // control turn. Both are the vendor's OWN tokens and both go out for
      // EVERY vendor (omitted only on the default row): dropping grok's /
      // kimi's / opencode's effort client-side is what made the menu lie.
      const { sid } = await apiCreateSession(slug, {
        role,
        vendor: effectiveDraft.vendor,
        permission_mode: effectiveDraft.hitl ? "hitl" : "skip",
        protocol: wireProtocol(effectiveDraft),
        model: modelSwitchFor(effectiveDraft, catalog) ?? undefined,
        effort: effortSwitchFor(effectiveDraft, catalog) ?? undefined,
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

            {/* 角色 — v0.8.20 F4 graduated to stable. Remote new-project
                creation still cannot inspect roles before the project exists. */}
            {!(isNewProject && effectiveHost !== "local") ? (
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
            draft={effectiveDraft}
            onDraftChange={setDraft}
            catalog={catalog}
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
            {PLAYBOOKS.map(({ id, key, Icon, vendors }) => (
              <button
                key={id}
                type="button"
                className="tpl-card"
                data-testid={`tpl-${id}`}
                title={t(`${key}P`)}
                onClick={() => pickPlaybook(id)}
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
