// v0.8.9 Phase 4 — plugin-marketplace browser (the global view that replaces
// the retired read-only Roles page; prototype `#v-market`).
//
// Browse + one-click install role/agent · skill · workflow from ccteam-hub
// (curated + ingested agency-agents etc.). Flow (marketplace-design §五):
//   底部点市场 → 选类目/来源/搜 → 点卡看正文 → 安装到当前项目 → 立刻能在新建
//   session 里选用.
//
// v0.9.9 — a `type=skill` entry installs into the user-level GLOBAL LIBRARY
// (`~/.ccteam/skills`), never the project: its CTA copy names the library
// (安装到库 / 已在库 / 更新库内版本, zh+en via the zh/en switch) and no
// "install to project" affordance exists for skills. agent/plugin CTAs and
// the install POST route are unchanged — the library semantics live in the
// backend; `installed_status` for skills is library-relative.
//
// Install target: project is picked AT INSTALL TIME (detail drawer), not in
// the filter bar — switching Skills ↔ Roles must not reflow the chrome.
// Internally we still keep a default/last project so the decorated
// `GET /api/v1/projects/{slug}/marketplace` can show 已装/更新 on cards, and
// so skill installs (library-write, project-scoped REST path) have a slug.
// With no projects registered we fall back to the UNDECORATED global catalog
// (browse-only); install stays disabled until a project exists.
//
// Four states per the v0.8.8 web UI quality baseline: loading / error /
// empty (no plugins in this category) / success. Install → POST → toast →
// re-fetch the decorated catalog → card flips. The detail drawer fetches the
// body lazily (marked-rendered markdown = review-before-install) + upstream +
// license + an install button (+ project picker for agent/plugin).
//
// Theme discipline: surface-*/brand-*/text-*/status-* + the vendor-* tokens
// only — NO bare Tailwind color literals (mirrors SettingsPage).

import { useCallback, useEffect, useMemo, useState } from "react";
import { marked } from "marked";
import DOMPurify from "dompurify";
import { ExternalLink, PackageOpen, RefreshCw, X } from "lucide-react";
import {
  Button,
  Combobox,
  Dialog,
  EmptyState,
  Skeleton,
  type ComboboxOption,
} from "../components/ui";
import { useProjectsStore } from "../hooks/useProjectsStore";
import {
  getMarketplace,
  getPluginBody,
  getProjectMarketplace,
  installPlugin,
  type HubPlugin,
  type InstalledStatus,
} from "../lib/marketplaceApi";
import {
  CATEGORIES,
  cardInstallNeedsPreview,
  distinctSources,
  filterPlugins,
  installable,
  installedStatusLabel,
  needsProjectTarget,
  skillLibraryStatusLabel,
} from "../lib/marketplaceFormat";
import { makeT, type Lang } from "../lib/i18n";
import { useWebSettings } from "../hooks/useWebSettings";
import { toastBus } from "../lib/toastBus";

/** A catalog plugin enriched with an (optional) per-project install status —
 *  the global catalog has none (browse-only), the decorated one does. */
type CatalogPlugin = HubPlugin & { installed_status?: InstalledStatus };

type LoadState =
  | { kind: "loading" }
  | { kind: "error"; message: string }
  | { kind: "ready"; plugins: CatalogPlugin[]; generatedAt: string };

const SRC_ALL = "__all";

export default function MarketplaceView({ embedded = false }: { embedded?: boolean } = {}) {
  // v0.9.9 — the zh/en switch drives the skill install-to-library CTA copy
  // (the rest of this panel's long-tail strings are still zh-only).
  const { settings } = useWebSettings();
  const lang = settings.language;
  const t = makeT(lang);
  // ---- install-target project picker -------------------------------------
  const { projects: projectRows, loading: projectsLoading } = useProjectsStore();
  const projects = useMemo(
    () => (projectRows ?? []).map((row) => row.slug).sort(),
    [projectRows],
  );
  const [selectedProject, setSelectedProject] = useState("");
  const project = projects.includes(selectedProject) ? selectedProject : (projects[0] ?? "");
  const projectsLoaded = !projectsLoading;

  // ---- catalog -----------------------------------------------------------
  const [state, setState] = useState<LoadState>({ kind: "loading" });
  const [refreshing, setRefreshing] = useState(false);

  // ---- filters -----------------------------------------------------------
  const [category, setCategory] = useState<HubPlugin["type"]>("skill");
  const [source, setSource] = useState<string>(SRC_ALL);
  const [query, setQuery] = useState("");

  // ---- detail drawer -----------------------------------------------------
  const [detailId, setDetailId] = useState<string | null>(null);

  // Fetch the catalog — decorated when a project is selected, global otherwise.
  // The actual network + state-transition core; state is only ever set inside
  // the async `.then`/`.catch` (never synchronously in the effect body) so the
  // react-hooks/set-state-in-effect rule stays clean. `refresh` bypasses the
  // hub cache (the "刷新目录" button + a post-install re-fetch).
  const fetchCatalog = useCallback(
    (refresh: boolean) => {
      const decorate = project.length > 0;
      const req = decorate ? getProjectMarketplace(project, refresh) : getMarketplace(refresh);
      return req
        .then((index) => {
          setState({
            kind: "ready",
            plugins: index.plugins as CatalogPlugin[],
            generatedAt: index.generated_at,
          });
        })
        .catch((e) => {
          if (e instanceof Error && e.message === "UNAUTHENTICATED") return;
          const message = e instanceof Error ? e.message : "加载市场失败";
          setState({ kind: "error", message });
        });
    },
    [project],
  );

  // Event-handler reload (refresh button / post-install): we CAN reset to a
  // loading/refreshing state synchronously here because this runs from a user
  // gesture, not an effect body.
  const reload = useCallback(
    (refresh: boolean) => {
      if (!projectsLoaded) return;
      if (refresh) setRefreshing(true);
      else setState({ kind: "loading" });
      void fetchCatalog(refresh).finally(() => setRefreshing(false));
    },
    [fetchCatalog, projectsLoaded],
  );

  // Initial / dependency-driven load. We fetch directly (no synchronous
  // setState in the effect body); the initial `state` is already `loading`, so
  // there's nothing to reset on the first run. On a later `project` switch
  // (drawer install-target change → setSelectedProject) this re-fires to re-decorate
  // installed_status quietly — no loading flash, drawer stays open (it only
  // unmounts when `state.kind !== "ready"`, which we avoid on project switch).
  useEffect(() => {
    if (!projectsLoaded) return;
    void fetchCatalog(false);
  }, [fetchCatalog, projectsLoaded]);

  // Distinct sources for the filter — derived from whatever the catalog holds.
  const sources = useMemo(
    () => (state.kind === "ready" ? distinctSources(state.plugins) : []),
    [state],
  );

  // Combobox option lists (project target + source filter).
  const projectOptions: ComboboxOption[] = useMemo(
    () => projects.map((p) => ({ value: p, label: p })),
    [projects],
  );
  const sourceOptions: ComboboxOption[] = useMemo(
    () => [{ value: SRC_ALL, label: "全部来源" }, ...sources.map((s) => ({ value: s, label: s }))],
    [sources],
  );

  // The cards to show = category + source + search filtered.
  const visible = useMemo(() => {
    if (state.kind !== "ready") return [];
    return filterPlugins(state.plugins, {
      type: category,
      source: source === SRC_ALL ? null : source,
      query,
    });
  }, [state, category, source, query]);

  // Counts per category (so the seg tabs can show non-empty categories even
  // when the active one is empty) + to drive the empty-state copy.
  const countByType = useMemo(() => {
    const counts: Record<string, number> = {};
    if (state.kind === "ready") {
      for (const p of state.plugins) counts[p.type] = (counts[p.type] ?? 0) + 1;
    }
    return counts;
  }, [state]);

  const detailPlugin = useMemo(
    () =>
      state.kind === "ready" ? state.plugins.find((p) => p.id === detailId) ?? null : null,
    [state, detailId],
  );

  // ---- install -----------------------------------------------------------
  const [installing, setInstalling] = useState<string | null>(null);
  const doInstall = useCallback(
    (plugin: CatalogPlugin) => {
      if (!project) {
        toastBus.handler?.error("先选一个安装目标项目");
        return;
      }
      // An update (sha differs) overwrites → pass force; a fresh install does
      // not (a 409 then means a stray file we shouldn't clobber silently).
      const force = plugin.installed_status === "update_available";
      setInstalling(plugin.id);
      installPlugin(project, plugin.id, force)
        .then((res) => {
          // A skill install lands in the user-level global library (never
          // the project) — its toast names the library, not `→ project`.
          toastBus.handler?.info(
            plugin.type === "skill"
              ? `${t("skillInstalledToast")}: ${res.id}${res.overwrote ? (lang === "en" ? " (updated)" : "（已更新）") : ""}`
              : `已安装 ${res.id} → ${project}${res.overwrote ? "（已更新）" : ""}`,
          );
          // Re-fetch the decorated catalog so the card flips to 已装 — quietly
          // (no loading flash; only the resolved result updates the grid).
          void fetchCatalog(false);
          setDetailId(null);
        })
        .catch((e) => {
          if (e instanceof Error && e.message === "UNAUTHENTICATED") return;
          toastBus.handler?.error(
            `安装失败: ${e instanceof Error ? e.message : "unknown"}`,
          );
        })
        .finally(() => setInstalling(null));
    },
    [project, fetchCatalog, t, lang],
  );

  return (
    <div data-testid="marketplace-view" className={embedded ? "" : "p-6 max-w-[1000px] mx-auto"}>
      <div className="flex items-start gap-3">
        {embedded ? (
          <div className="flex-1 min-w-0" />
        ) : (
          <div className="flex-1 min-w-0">
            <h2 className="text-base font-semibold text-text-primary">插件市场</h2>
            <p className="mt-1 text-sm text-text-secondary">
              浏览 + 一键装 role/agent、skill、workflow。来源 ccteam-hub（自建）+ agency-agents
              等开源。skill 装入全局库,agent/plugin 按所选项目安装。
            </p>
          </div>
        )}
        <button
          type="button"
          onClick={() => reload(true)}
          disabled={refreshing || state.kind === "loading"}
          title="刷新目录（重新拉 hub index）"
          className="shrink-0 h-8 px-2.5 rounded-md text-xs flex items-center gap-1.5 border border-surface-700/60 text-text-secondary hover:text-text-primary hover:bg-surface-800 disabled:opacity-40"
        >
          <RefreshCw className={`h-3.5 w-3.5 ${refreshing ? "animate-spin" : ""}`} />
          刷新目录
        </button>
      </div>

      {/* filter bar: category seg · source · search (stable layout — install
          target lives in the drawer so tab switches never reflow this row). */}
      <div className="mt-4 flex flex-wrap items-center gap-2.5">
        <div className="flex items-center gap-0.5 rounded-md bg-surface-800 p-0.5" role="tablist">
          {CATEGORIES.map((c) => (
            <button
              key={c.type}
              type="button"
              role="tab"
              aria-selected={category === c.type}
              onClick={() => setCategory(c.type)}
              className={`h-7 px-3 rounded text-xs ${
                category === c.type
                  ? "bg-surface-700 text-text-primary"
                  : "text-text-dim hover:text-text-secondary"
              }`}
            >
              {c.label}
              {countByType[c.type] ? (
                <span className="ml-1.5 text-[10px] text-text-dim">{countByType[c.type]}</span>
              ) : null}
            </button>
          ))}
        </div>

        <Combobox
          value={source}
          onChange={setSource}
          options={sourceOptions}
          searchable={sources.length > 8}
          searchPlaceholder="搜索来源…"
          ariaLabel="来源筛选"
          className="min-w-[140px]"
          buttonClassName="h-8 text-xs"
        />

        <input
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          placeholder="搜索插件（id / 名称 / 描述 / tag）…"
          aria-label="搜索插件"
          className="flex-1 min-w-[140px] h-8 rounded-md bg-surface-800 border border-surface-700 px-3 text-xs text-text-primary placeholder:text-text-dim outline-none focus:border-brand-500"
        />
      </div>

      {/* four states */}
      <div className="mt-4">
        {state.kind === "loading" ? (
          <div
            data-testid="marketplace-loading"
            className="grid gap-3"
            style={{ gridTemplateColumns: "repeat(auto-fill, minmax(260px, 1fr))" }}
          >
            {Array.from({ length: 6 }, (_, i) => (
              <div
                key={i}
                className="flex flex-col gap-2 rounded-lg bg-surface-900 border border-surface-700/60 p-3.5"
              >
                <Skeleton className="h-4 w-2/3" />
                <Skeleton className="h-3 w-full" />
                <Skeleton className="h-3 w-5/6" />
                <div className="mt-1 flex items-center gap-2">
                  <Skeleton className="h-3 w-10" />
                  <Skeleton className="ml-auto h-6 w-16" />
                </div>
              </div>
            ))}
          </div>
        ) : state.kind === "error" ? (
          <div
            data-testid="marketplace-error"
            role="alert"
            className="rounded-lg border border-status-error/40 bg-status-error/10 px-4 py-4 text-sm text-status-error"
          >
            <div>加载市场失败: {state.message}</div>
            <button
              type="button"
              onClick={() => reload(true)}
              className="mt-2 h-7 px-2.5 rounded-md text-xs border border-surface-700/60 text-text-secondary hover:text-text-primary hover:bg-surface-800"
            >
              重试
            </button>
          </div>
        ) : visible.length === 0 ? (
          <EmptyState
            data-testid="marketplace-empty"
            icon={PackageOpen}
            title={query.trim() ? `没有匹配「${query.trim()}」的插件` : "该类目暂无插件"}
            description={
              query.trim()
                ? "换个关键词，或清空搜索看全部。"
                : "切换上方类目/来源，或刷新目录重新拉取 hub。"
            }
            action={
              <Button variant="outline" size="sm" onClick={() => reload(true)} disabled={refreshing}>
                <RefreshCw className={refreshing ? "animate-spin" : ""} />
                刷新目录
              </Button>
            }
          />
        ) : (
          <div
            data-testid="marketplace-grid"
            className="grid gap-3"
            style={{ gridTemplateColumns: "repeat(auto-fill, minmax(260px, 1fr))" }}
          >
            {visible.map((plugin) => (
              <PluginCard
                key={plugin.id}
                plugin={plugin}
                lang={lang}
                installing={installing === plugin.id}
                canInstall={project.length > 0}
                onOpen={() => setDetailId(plugin.id)}
                // Card CTA routes into the drawer when:
                //   - agent/plugin → always (install-time project picker lives
                //     there; never install-to-wrong-project from the card)
                //   - skill first-install → review-before-install (body preview)
                // Skill `update_available` was already reviewed → one-click
                // install-to-library from the card (no project to pick).
                onInstall={() =>
                  needsProjectTarget(plugin.type) ||
                  cardInstallNeedsPreview(plugin.installed_status)
                    ? setDetailId(plugin.id)
                    : doInstall(plugin)
                }
              />
            ))}
          </div>
        )}
      </div>

      {detailPlugin ? (
        <PluginDrawer
          key={detailPlugin.id}
          plugin={detailPlugin}
          project={project}
          projects={projects}
          projectOptions={projectOptions}
          lang={lang}
          installing={installing === detailPlugin.id}
          onClose={() => setDetailId(null)}
          onProjectChange={setSelectedProject}
          onInstall={() => doInstall(detailPlugin)}
        />
      ) : null}
    </div>
  );
}

// --------------------------------------------------------------------------
// Card
// --------------------------------------------------------------------------

/** Vendor-neutral source badge. `builtin` = the muted "none" badge; anything
 *  else (an ingested open-source source) = the accent badge. Theme tokens
 *  only. */
function SourceBadge({ source }: { source: string }) {
  const builtin = source === "builtin" || source === "";
  return (
    <span
      className={`text-[10px] font-medium px-1.5 py-0.5 rounded ${
        builtin
          ? "bg-surface-700 text-text-dim"
          : "bg-accent-500/15 text-accent-500"
      }`}
    >
      {source || "builtin"}
    </span>
  );
}

/** The install button / state pill driven by `installed_status`. When no
 *  install target is selected the action is disabled with a hint. SKILL
 *  entries (`skill`) install into the user-level global library — their copy
 *  names the library (i18n); agent/plugin entries keep the project copy. */
export function InstallButton({
  status,
  installing,
  canInstall,
  onInstall,
  skill = false,
  lang = "zh",
}: {
  status: InstalledStatus | undefined;
  installing: boolean;
  canInstall: boolean;
  onInstall: () => void;
  /** true for `type === "skill"` entries (install-to-library semantics). */
  skill?: boolean;
  lang?: Lang;
}) {
  // No per-project decoration (global browse) → treat as not_installed.
  const st: InstalledStatus = status ?? "not_installed";
  if (st === "installed") {
    return (
      <span className="text-[11px] font-medium px-2 py-1 rounded-md bg-status-running/15 text-status-running">
        {skill ? skillLibraryStatusLabel("installed", lang) : "已装"}
      </span>
    );
  }
  const label = skill ? skillLibraryStatusLabel(st, lang) : installedStatusLabel(st);
  return (
    <button
      type="button"
      disabled={installing || !canInstall}
      onClick={(e) => {
        e.stopPropagation();
        onInstall();
      }}
      title={canInstall ? undefined : "先选一个安装目标项目"}
      className={`text-xs font-medium px-3 py-1 rounded-md disabled:opacity-40 disabled:cursor-not-allowed ${
        st === "update_available"
          ? "bg-brand-500/15 text-brand-400 hover:bg-brand-500/25"
          : "bg-brand-500 text-surface-950 hover:bg-brand-400"
      }`}
    >
      {installing ? "安装中…" : label}
    </button>
  );
}

export function PluginCard({
  plugin,
  lang = "zh",
  installing,
  canInstall,
  onOpen,
  onInstall,
}: {
  plugin: CatalogPlugin;
  lang?: Lang;
  installing: boolean;
  canInstall: boolean;
  onOpen: () => void;
  onInstall: () => void;
}) {
  return (
    <div
      role="button"
      tabIndex={0}
      onClick={onOpen}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") {
          e.preventDefault();
          onOpen();
        }
      }}
      className="text-left flex flex-col gap-2 rounded-lg bg-surface-900 border border-surface-700/60 hover:border-brand-500/50 p-3.5 cursor-pointer transition-colors"
    >
      <div className="flex items-center gap-2">
        <b className="text-sm text-text-primary truncate">{plugin.name || plugin.id}</b>
        <SourceBadge source={plugin.source} />
      </div>
      <p className="text-xs text-text-secondary flex-1 line-clamp-3">{plugin.description}</p>
      <div className="flex items-center gap-2">
        {plugin.license ? (
          <span className="text-[10px] font-mono text-text-dim">{plugin.license}</span>
        ) : null}
        {plugin.tags.slice(0, 2).map((t) => (
          <span key={t} className="text-[10px] font-mono text-text-dim/80">
            {`#${t}`}
          </span>
        ))}
        <span className="ml-auto">
          <InstallButton
            status={plugin.installed_status}
            installing={installing}
            canInstall={canInstall}
            onInstall={onInstall}
            skill={plugin.type === "skill"}
            lang={lang}
          />
        </span>
      </div>
    </div>
  );
}

// --------------------------------------------------------------------------
// Detail drawer (review-before-install)
// --------------------------------------------------------------------------

type BodyState =
  | { kind: "loading" }
  | { kind: "error"; message: string }
  | { kind: "ready"; html: string };

/** Render hub markdown to SANITIZED HTML for the body preview.
 *
 *  The hub ingests THIRD-PARTY open-source `.md` (agency-agents etc.) verbatim
 *  and `marked@18` does NOT sanitize, so a malicious/compromised upstream body
 *  could smuggle `<img onerror=…>` / `javascript:` as stored-XSS at full
 *  same-origin (web-token) privilege. We run the rendered HTML through
 *  DOMPurify (defense-in-depth) before it ever reaches `dangerouslySetInnerHTML`.
 *
 *  DOMPurify needs a DOM: in the browser bundle its default export is a ready
 *  instance, so `.sanitize` exists and strips the dangerous markup. In a
 *  non-DOM env (e.g. the node-based vitest renderer importing this module) the
 *  default export is an uninstantiated factory with no `.sanitize`; we guard so
 *  the import path never throws — the un-sanitized fallback is harmless there
 *  because nothing renders it (the drawer is browser-only). */
function renderBody(markdown: string): string {
  const html = marked.parse(markdown, { async: false }) as string;
  return typeof DOMPurify.sanitize === "function" ? DOMPurify.sanitize(html) : html;
}

export function PluginDrawer({
  plugin,
  project,
  projects = [],
  projectOptions = [],
  lang = "zh",
  installing,
  onClose,
  onProjectChange,
  onInstall,
}: {
  plugin: CatalogPlugin;
  project: string;
  /** Registered project slugs — drives the install-time target picker. */
  projects?: string[];
  projectOptions?: ComboboxOption[];
  lang?: Lang;
  installing: boolean;
  onClose: () => void;
  /** Called when the operator picks a different install target (agent/plugin). */
  onProjectChange?: (slug: string) => void;
  onInstall: () => void;
}) {
  // Initial state is `loading`; the drawer is keyed on `plugin.id` at its
  // call-site, so opening a different plugin REMOUNTS this component and the
  // initializer is the reset — no synchronous setState in the effect body
  // (keeps react-hooks/set-state-in-effect clean).
  const [body, setBody] = useState<BodyState>({ kind: "loading" });
  const t = makeT(lang);
  const isSkill = plugin.type === "skill";
  // agent / plugin install into a project; skill → global library (no picker).
  const showProjectPicker = needsProjectTarget(plugin.type);

  useEffect(() => {
    let cancelled = false;
    getPluginBody(plugin.id)
      .then((res) => {
        if (cancelled) return;
        // marked is sync for a string input; DOMPurify then strips any XSS the
        // ingested third-party markdown could carry (see renderBody).
        setBody({ kind: "ready", html: renderBody(res.body) });
      })
      .catch((e) => {
        if (cancelled) return;
        if (e instanceof Error && e.message === "UNAUTHENTICATED") return;
        setBody({
          kind: "error",
          message: e instanceof Error ? e.message : "加载正文失败",
        });
      });
    return () => {
      cancelled = true;
    };
  }, [plugin.id]);

  return (
    <Dialog
      open
      onClose={onClose}
      placement="end"
      ariaLabel={`插件详情 ${plugin.name || plugin.id}`}
      header={
        <div className="h-12 shrink-0 px-4 flex items-center gap-2 border-b border-surface-700/50">
          <b className="text-sm text-text-primary truncate">{plugin.name || plugin.id}</b>
          <SourceBadge source={plugin.source} />
          <span className="flex-1" />
          <button
            type="button"
            onClick={onClose}
            aria-label="关闭"
            className="text-text-dim hover:text-text-primary p-1"
          >
            <X className="h-4 w-4" />
          </button>
        </div>
      }
    >
      <div className="flex-1 min-h-0 overflow-y-auto p-4 space-y-3">
          <p className="text-sm text-text-secondary">{plugin.description}</p>
          <div className="flex flex-wrap items-center gap-3 text-[11px] font-mono text-text-dim">
            <span>id: {plugin.id}</span>
            <span>type: {plugin.type}</span>
            {plugin.license ? <span>license: {plugin.license}</span> : null}
            {plugin.upstream ? (
              <a
                href={plugin.upstream}
                target="_blank"
                rel="noreferrer noopener"
                className="flex items-center gap-1 text-brand-400 hover:text-brand-500 underline"
              >
                <ExternalLink className="h-3 w-3" /> upstream
              </a>
            ) : null}
          </div>
          {plugin.tags.length > 0 ? (
            <div className="flex flex-wrap gap-1.5">
              {plugin.tags.map((tag) => (
                <span
                  key={tag}
                  className="text-[10px] font-mono text-text-dim bg-surface-800 px-1.5 py-0.5 rounded"
                >
                  {`#${tag}`}
                </span>
              ))}
            </div>
          ) : null}

          <div className="pt-1">
            <div className="text-[11px] uppercase tracking-wide text-text-dim mb-1.5">
              正文预览（装前 review）
            </div>
            {body.kind === "loading" ? (
              <div className="text-xs text-text-dim py-4">加载正文中…</div>
            ) : body.kind === "error" ? (
              <div role="alert" className="text-xs text-status-error py-2">
                加载正文失败: {body.message}
              </div>
            ) : (
              <div
                className="cockpit-markdown text-sm text-text-secondary rounded-md border border-surface-700/50 bg-surface-950/40 p-3"
                // body.html is DOMPurify-sanitized (renderBody) — third-party
                // hub markdown stripped of XSS before it reaches the DOM.
                dangerouslySetInnerHTML={{ __html: body.html }}
              />
            )}
          </div>
        </div>

        <div className="shrink-0 px-4 py-3 border-t border-surface-700/50 flex flex-wrap items-center gap-2">
          {isSkill ? (
            <span className="text-[11px] text-text-dim truncate">
              {t("skillLibraryTarget")}
            </span>
          ) : showProjectPicker ? (
            <label
              data-testid="market-project-picker"
              className="flex items-center gap-1.5 text-xs text-text-dim min-w-0"
            >
              安装到
              <Combobox
                value={project}
                onChange={(v) => onProjectChange?.(v)}
                options={projectOptions}
                searchable={projects.length > 8}
                placeholder="（无项目 · 仅浏览）"
                searchPlaceholder="搜索项目…"
                ariaLabel="安装目标项目"
                className="min-w-[140px]"
                buttonClassName="h-8 text-xs"
              />
            </label>
          ) : (
            <span className="text-[11px] text-text-dim truncate">
              {project ? (
                <>
                  安装到 <span className="font-mono text-text-secondary">{project}</span>
                </>
              ) : (
                "无可用项目"
              )}
            </span>
          )}
          <span className="flex-1" />
          {plugin.installed_status === "installed" ? (
            <span className="text-[11px] font-medium px-2 py-1 rounded-md bg-status-running/15 text-status-running">
              {isSkill ? skillLibraryStatusLabel("installed", lang) : "已装"}
            </span>
          ) : (
            <button
              type="button"
              disabled={installing || !project || !installable(plugin.installed_status ?? "not_installed")}
              onClick={onInstall}
              className="h-8 px-3 rounded-md text-sm bg-brand-500 text-surface-950 hover:bg-brand-400 disabled:opacity-40 disabled:cursor-not-allowed"
            >
              {installing
                ? "安装中…"
                : isSkill
                  ? skillLibraryStatusLabel(plugin.installed_status ?? "not_installed", lang)
                  : plugin.installed_status === "update_available"
                    ? `更新到 ${project || "项目"}`
                    : `安装到 ${project || "项目"}`}
            </button>
          )}
        </div>
    </Dialog>
  );
}
