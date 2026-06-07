// v0.8.8 F5 — read-only Roles browser.
//
// Three stacked, independently-fetched surfaces, each with the four-state
// shape (loading / error / empty / success) borrowed from SessionsListPage:
//   1. a project <select> (slugs from `fetchDashboard()`),
//   2. the selected project's role list (`listProjectRoles(slug)`) as cards,
//   3. a clicked role's detail (`getRoleDetail(slug, role)`): frontmatter as a
//      key/value table + the markdown body rendered via `marked`.
//
// READ-ONLY: no web editing (the PUT endpoint exists but we don't surface it),
// and no in-app catalog install — `ccteam role add` stays the install path.
//
// 红线(red lines):
//   - Theme tokens only: surface-*/brand-*/status-error (no bare amber-*/red-*).
//   - Frontmatter values may be non-scalar (a YAML list/map round-trips to a
//     JSON array/object): `renderFrontmatterValue` (rolesView.ts) renders
//     scalars verbatim, everything else via JSON.stringify so the table never
//     shows "[object Object]".
//   - The role `.md` is a LOCAL, trusted file (`.claude/agents/*.md`), so the
//     body is rendered through `marked` into a `.cockpit-markdown` container.
//     `marked.parse` is synchronous (string) in v18.
//   - Every fetch effect uses a `cancelled` guard; the list/detail views are
//     keyed (`slug` / `slug:role:nonce`) so a re-fetch REMOUNTS to the fresh
//     `loading` initial state instead of a synchronous in-effect setState
//     (react-hooks/set-state-in-effect), and a fast switch can't land a stale
//     response on the new view.

import { useEffect, useMemo, useState } from "react";
import { marked } from "marked";
import { fetchDashboard } from "../lib/dashboardApi";
import {
  getRoleDetail,
  listProjectRoles,
  type RoleDetail,
  type RoleSummary,
} from "../lib/sessionsApi";
import { humanError, renderFrontmatterValue } from "./rolesView";

export default function RolesPage() {
  // ---- project select ----------------------------------------------------
  const [slugs, setSlugs] = useState<string[] | null>(null);
  const [projectsError, setProjectsError] = useState<string | null>(null);
  const [slug, setSlug] = useState<string>("");
  // The role selected for the detail view (null = list view).
  const [activeRole, setActiveRole] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    fetchDashboard()
      .then((rows) => {
        if (!cancelled) setSlugs(rows.map((r) => r.slug));
      })
      .catch((err) => {
        if (cancelled) return;
        const msg = err instanceof Error ? err.message : String(err);
        if (msg !== "UNAUTHENTICATED") setProjectsError(humanError(msg));
      });
    return () => {
      cancelled = true;
    };
  }, []);

  return (
    <div data-testid="roles-page" className="p-4 flex flex-col gap-4">
      <header className="flex flex-col gap-1">
        <h1 className="text-sm font-medium text-text-primary">Roles</h1>
        <p className="text-xs text-text-dim font-mono">
          只读浏览每个项目 <code>.claude/agents/</code> 下的角色；用{" "}
          <code>ccteam role add</code> 安装新角色。
        </p>
      </header>

      <ProjectPicker
        slugs={slugs}
        error={projectsError}
        value={slug}
        onChange={(s) => {
          setSlug(s);
          setActiveRole(null);
        }}
      />

      {slug ? (
        activeRole ? (
          <RoleDetailView
            // Key on slug:role so switching role remounts to fresh loading.
            key={`${slug}:${activeRole}`}
            slug={slug}
            role={activeRole}
            onBack={() => setActiveRole(null)}
          />
        ) : (
          // Key on slug so switching project remounts to fresh loading.
          <RoleListView key={slug} slug={slug} onOpen={setActiveRole} />
        )
      ) : null}
    </div>
  );
}

// --------------------------------------------------------------------------
// 1) Project picker (loading / error / empty / success)
// --------------------------------------------------------------------------

function ProjectPicker({
  slugs,
  error,
  value,
  onChange,
}: {
  slugs: string[] | null;
  error: string | null;
  value: string;
  onChange: (slug: string) => void;
}) {
  if (error) {
    return (
      <div
        data-testid="roles-projects-error"
        className="text-xs text-status-error font-mono"
        role="alert"
      >
        加载项目失败：{error}
      </div>
    );
  }
  if (slugs === null) {
    return (
      <div
        data-testid="roles-projects-loading"
        className="text-xs text-text-dim font-mono"
      >
        加载项目中…
      </div>
    );
  }
  if (slugs.length === 0) {
    return (
      <div
        data-testid="roles-projects-empty"
        className="text-xs text-text-dim font-mono"
      >
        暂无项目。先 <code>ccteam init</code> 一个项目。
      </div>
    );
  }
  return (
    <div className="flex flex-col gap-1 max-w-md">
      <label
        htmlFor="roles-project-select"
        className="text-[11px] text-text-dim font-mono"
      >
        项目
      </label>
      <select
        id="roles-project-select"
        data-testid="roles-project-select"
        value={value}
        onChange={(e) => onChange(e.target.value)}
        className="w-full h-9 rounded-md bg-surface-800 border border-surface-700 px-3 text-sm outline-none focus:border-brand-600 transition-colors"
      >
        <option value="">（选择项目…）</option>
        {slugs.map((s) => (
          <option key={s} value={s}>
            {s}
          </option>
        ))}
      </select>
    </div>
  );
}

// --------------------------------------------------------------------------
// 2) Role list for the chosen project (loading / error / empty / success)
// --------------------------------------------------------------------------

type ListState =
  | { kind: "loading" }
  | { kind: "error"; message: string }
  | { kind: "ready"; roles: RoleSummary[] };

export function RoleListView({
  slug,
  onOpen,
}: {
  slug: string;
  onOpen: (role: string) => void;
}) {
  // `loading` is the INITIAL state; the component is keyed on `slug` by the
  // parent so a project switch remounts here (fresh loading) — no synchronous
  // in-effect setState. The "重试" button bumps a key-less remount via a local
  // nonce that re-runs the effect; we tolerate the brief stale view because the
  // `cancelled` guard already prevents a late response from landing.
  const [state, setState] = useState<ListState>({ kind: "loading" });
  const [reloadNonce, setReloadNonce] = useState(0);

  useEffect(() => {
    let cancelled = false;
    listProjectRoles(slug)
      .then((roles) => {
        if (!cancelled) setState({ kind: "ready", roles });
      })
      .catch((err) => {
        if (cancelled) return;
        const msg = err instanceof Error ? err.message : String(err);
        if (msg === "UNAUTHENTICATED") return; // global gate re-auths
        setState({ kind: "error", message: humanError(msg) });
      });
    return () => {
      cancelled = true;
    };
  }, [slug, reloadNonce]);

  if (state.kind === "loading") {
    return (
      <div
        data-testid="roles-list-loading"
        className="text-xs text-text-dim font-mono"
      >
        加载角色中…
      </div>
    );
  }
  if (state.kind === "error") {
    return (
      <div
        data-testid="roles-list-error"
        className="flex flex-col items-start gap-2 text-xs text-status-error font-mono"
        role="alert"
      >
        <span>加载角色失败：{state.message}</span>
        <RetryButton onClick={() => setReloadNonce((n) => n + 1)} />
      </div>
    );
  }
  if (state.roles.length === 0) {
    return (
      <div
        data-testid="roles-list-empty"
        className="text-xs text-text-dim font-mono"
      >
        该项目 <code>.claude/agents/</code> 下无 role，用{" "}
        <code>ccteam role add</code> 安装。
      </div>
    );
  }
  return (
    <div data-testid="roles-list" className="flex flex-col gap-2">
      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-3">
        {state.roles.map((r) => (
          <RoleCard key={r.role} role={r} onOpen={() => onOpen(r.role)} />
        ))}
      </div>
    </div>
  );
}

function RetryButton({ onClick }: { onClick: () => void }) {
  return (
    <button
      type="button"
      onClick={onClick}
      className="px-2 py-1 rounded border border-surface-700 text-text-secondary hover:text-text-primary hover:bg-surface-800 transition-colors"
    >
      重试
    </button>
  );
}

export function RoleCard({
  role,
  onOpen,
}: {
  role: RoleSummary;
  onOpen: () => void;
}) {
  return (
    <button
      type="button"
      onClick={onOpen}
      data-testid={`role-card-${role.role}`}
      className="text-left bg-surface-800/60 hover:bg-surface-800 border border-surface-700/40 rounded-lg p-3 transition-colors flex flex-col gap-2 min-w-0"
    >
      <div className="flex items-center gap-2 min-w-0">
        <span className="font-mono text-sm text-text-primary truncate flex-1">
          {role.role}
        </span>
        {role.model ? (
          <span className="shrink-0 px-1.5 py-0.5 rounded text-[10px] font-mono uppercase tracking-wider bg-brand-600/10 text-brand-400">
            {role.model}
          </span>
        ) : null}
      </div>
      {role.description ? (
        <p className="text-[11px] text-text-dim font-mono line-clamp-2">
          {role.description}
        </p>
      ) : (
        <p className="text-[11px] text-text-dim/60 font-mono italic">无描述</p>
      )}
    </button>
  );
}

// --------------------------------------------------------------------------
// 3) Single role detail (loading / error / empty / success)
// --------------------------------------------------------------------------

type DetailState =
  | { kind: "loading" }
  | { kind: "error"; message: string }
  | { kind: "ready"; detail: RoleDetail };

function RoleDetailView({
  slug,
  role,
  onBack,
}: {
  slug: string;
  role: string;
  onBack: () => void;
}) {
  // Keyed on slug:role by the parent → a role switch remounts to fresh loading.
  const [state, setState] = useState<DetailState>({ kind: "loading" });
  const [reloadNonce, setReloadNonce] = useState(0);

  useEffect(() => {
    let cancelled = false;
    getRoleDetail(slug, role)
      .then((detail) => {
        if (!cancelled) setState({ kind: "ready", detail });
      })
      .catch((err) => {
        if (cancelled) return;
        const msg = err instanceof Error ? err.message : String(err);
        if (msg === "UNAUTHENTICATED") return; // global gate re-auths
        setState({ kind: "error", message: humanError(msg) });
      });
    return () => {
      cancelled = true;
    };
  }, [slug, role, reloadNonce]);

  return (
    <div data-testid="role-detail" className="flex flex-col gap-3">
      <button
        type="button"
        onClick={onBack}
        data-testid="role-detail-back"
        className="self-start text-[11px] font-mono text-text-dim hover:text-text-primary transition-colors"
      >
        ← 返回角色列表
      </button>
      <h2 className="font-mono text-sm text-text-primary">{role}</h2>

      {state.kind === "loading" ? (
        <div
          data-testid="role-detail-loading"
          className="text-xs text-text-dim font-mono"
        >
          加载角色详情中…
        </div>
      ) : null}

      {state.kind === "error" ? (
        <div
          data-testid="role-detail-error"
          className="flex flex-col items-start gap-2 text-xs text-status-error font-mono"
          role="alert"
        >
          <span>加载角色详情失败：{state.message}</span>
          <RetryButton onClick={() => setReloadNonce((n) => n + 1)} />
        </div>
      ) : null}

      {state.kind === "ready" ? <RoleDetailBody detail={state.detail} /> : null}
    </div>
  );
}

export function RoleDetailBody({ detail }: { detail: RoleDetail }) {
  const entries = useMemo(
    () => Object.entries(detail.frontmatter ?? {}),
    [detail.frontmatter],
  );
  // The role .md is a local, trusted file; render its body through marked into
  // the shared .cockpit-markdown container. v18 `marked.parse` returns a string.
  const bodyHtml = useMemo(
    () => marked.parse(detail.body ?? "", { async: false }) as string,
    [detail.body],
  );

  return (
    <div className="flex flex-col gap-4">
      <section
        data-testid="role-frontmatter"
        className="bg-surface-800/60 border border-surface-700/40 rounded-lg p-3 flex flex-col gap-2"
      >
        <h3 className="text-[11px] font-mono uppercase tracking-wide text-text-dim">
          Frontmatter
        </h3>
        {entries.length === 0 ? (
          <p className="text-[11px] text-text-dim font-mono italic">
            无 frontmatter
          </p>
        ) : (
          <dl className="grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-[11px] font-mono">
            {entries.map(([key, value]) => (
              <div key={key} className="contents">
                <dt className="text-text-muted">{key}</dt>
                <dd className="text-text-secondary break-words min-w-0">
                  {renderFrontmatterValue(value)}
                </dd>
              </div>
            ))}
          </dl>
        )}
      </section>

      <section
        data-testid="role-body"
        className="bg-surface-800/60 border border-surface-700/40 rounded-lg p-4"
      >
        {detail.body && detail.body.trim().length > 0 ? (
          <div
            className="cockpit-markdown text-sm text-text-secondary"
            dangerouslySetInnerHTML={{ __html: bodyHtml }}
          />
        ) : (
          <p className="text-[11px] text-text-dim font-mono italic">空 body</p>
        )}
      </section>
    </div>
  );
}
