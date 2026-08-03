// v0.9.11 TEAM-9 — 主机接入与注册: the ops ACTION panel (设置→运维总览).
//
// This panel carries actions ONLY. Fleet observation — per-host × per-vendor
// health, live session counts, spend, offline age, host removal — lives on the
// Team page's charter roster, which is strictly richer; the health grid that
// used to duplicate it here is deleted and the header links there instead.
// What stays is what the Team page deliberately does not do:
//   · register-mcp — write ccteam's own MCP server into a LOCAL vendor config
//     (never a vendor login, never a CLI install; the backend 404s non-local).
//   · import — adopt a satellite-reported project into the daemon catalog.
//   · JoinCard — the real `ccteam host join` command. Exported from here and
//     ALSO rendered by AccessView (设置·接入), where this panel's footer points
//     rather than embedding a second copy.
//
// Data: GET /api/v1/hosts (registry) fanned into GET /api/v1/hosts/{host}; a
// host whose detail probe fails renders offline (honest state — we then say we
// cannot see what it needs, not that there is nothing to do).

import { useCallback, useEffect, useState } from "react";
import { Link } from "react-router-dom";
import {
  getHostDetail,
  getHosts,
  getJoinToken,
  mintJoinToken,
  registerMcp,
  type HostDetail,
  type HostSummary,
  type JoinTokenInfo,
} from "../lib/hostsApi";
import { importProject } from "../lib/dashboardApi";
import { copyText } from "../lib/clipboard";
import { makeT, type Lang } from "../lib/i18n";
import { vendorDotClass } from "../lib/vendors";

type HostState =
  | { kind: "ready"; detail: HostDetail }
  | { kind: "offline"; summary: HostSummary };

type LoadState =
  | { kind: "loading" }
  | { kind: "error"; message: string }
  | { kind: "ready"; hosts: HostState[] };

const REFRESH = "__refresh__";

/** Busy-token formats — one home, shared by the handlers and the rows. */
const registerKey = (host: string, vendor: string) => `${host}:${vendor}`;
const importKey = (host: string, slug: string) => `import:${host}:${slug}`;

/** The only two things ops can DO to a host. */
export type PendingAction =
  | { kind: "register"; vendor: string }
  | { kind: "import"; slug: string; path: string };

/** Actionable items for one probed host, in render order.
 *
 *  Local: vendors installed on PATH whose config still lacks ccteam's MCP
 *  entry (`tool_surface` must be `native_mcp_config`, so a managed-bridge CTA
 *  can never become a no-op). Satellites: projects the
 *  satellite reports but the daemon catalog has not adopted. The split is
 *  hard — register-mcp 404s off-local, and a local project is cataloged by
 *  definition. */
// eslint-disable-next-line react-refresh/only-export-components -- pure helper co-located with its only consumer for unit tests.
export function pendingActionsFor(detail: HostDetail): PendingAction[] {
  if (detail.is_local) {
    return detail.agents
      .filter(
        (a) =>
          a.tool_surface === "native_mcp_config" && a.installed && !a.mcp_registered,
      )
      .map((a) => ({ kind: "register", vendor: a.vendor }) as PendingAction);
  }
  return (detail.projects ?? [])
    .filter((p) => !p.cataloged)
    .map((p) => ({ kind: "import", slug: p.slug, path: p.path }) as PendingAction);
}

/** Managed-session-only notices shown verbatim from the backend SoT. */
// eslint-disable-next-line react-refresh/only-export-components -- pure helper co-located with its only consumer for unit tests.
export function toolSurfaceNoticesFor(detail: HostDetail): string[] {
  return [
    ...new Set(
      detail.agents.flatMap((agent) =>
        agent.tool_surface_note ? [agent.tool_surface_note] : [],
      ),
    ),
  ];
}

async function probeAll(refresh: boolean): Promise<HostState[]> {
  const { hosts } = await getHosts();
  const summaries = hosts.length > 0 ? hosts : null;
  if (!summaries) {
    // No registry rows — probe the implicit local host directly.
    const detail = await getHostDetail("local", refresh);
    return [{ kind: "ready", detail }];
  }
  return Promise.all(
    summaries.map((summary) =>
      getHostDetail(summary.host, refresh)
        .then((detail) => ({ kind: "ready", detail }) as HostState)
        .catch(() => ({ kind: "offline", summary }) as HostState),
    ),
  );
}

export default function HostsView({
  lang = "zh",
  embedded = false,
}: { lang?: Lang; /** hide page title when nested under Ops panel */ embedded?: boolean } = {}) {
  const t = makeT(lang);
  const [state, setState] = useState<LoadState>({ kind: "loading" });
  /** vendor token currently registering (scoped per host:vendor), or REFRESH. */
  const [busy, setBusy] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);

  const load = useCallback(async (refresh: boolean) => {
    try {
      const hosts = await probeAll(refresh);
      setState({ kind: "ready", hosts });
    } catch (e) {
      if (e instanceof Error && e.message === "UNAUTHENTICATED") return;
      const message = e instanceof Error ? e.message : "加载失败";
      setState((prev) => (prev.kind === "ready" ? prev : { kind: "error", message }));
    }
  }, []);

  useEffect(() => {
    let cancelled = false;
    probeAll(false)
      .then((hosts) => {
        if (!cancelled) setState({ kind: "ready", hosts });
      })
      .catch((e) => {
        if (cancelled) return;
        if (e instanceof Error && e.message === "UNAUTHENTICATED") return;
        const message = e instanceof Error ? e.message : "加载失败";
        setState((prev) => (prev.kind === "ready" ? prev : { kind: "error", message }));
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const onRefresh = async () => {
    setActionError(null);
    setBusy(REFRESH);
    await load(true);
    setBusy(null);
  };

  const onRegister = async (host: string, vendor: string) => {
    setActionError(null);
    setBusy(registerKey(host, vendor));
    try {
      await registerMcp(host, vendor);
      await load(true);
    } catch (e) {
      if (!(e instanceof Error && e.message === "UNAUTHENTICATED")) {
        setActionError(
          `注册 MCP 失败（${vendor}）: ${e instanceof Error ? e.message : "未知错误"}`,
        );
      }
    } finally {
      setBusy(null);
    }
  };

  const onImport = async (host: string, remoteSlug: string) => {
    const key = importKey(host, remoteSlug);
    setActionError(null);
    setBusy(key);
    try {
      const created = await importProject(host, remoteSlug);
      setState((current) => {
        if (current.kind !== "ready") return current;
        return {
          ...current,
          hosts: current.hosts.map((entry) =>
            entry.kind !== "ready" || entry.detail.host !== host
              ? entry
              : {
                  ...entry,
                  detail: {
                    ...entry.detail,
                    projects: (entry.detail.projects ?? []).map((project) =>
                      project.slug === remoteSlug
                        ? { ...project, cataloged: true, catalog_slug: created.slug }
                        : project,
                    ),
                  },
                },
          ),
        };
      });
      await load(false);
    } catch (e) {
      if (!(e instanceof Error && e.message === "UNAUTHENTICATED")) {
        setActionError(`${t("importProjectFailed")}: ${e instanceof Error ? e.message : t("unknownError")}`);
      }
    } finally {
      setBusy(null);
    }
  };

  return (
    <div data-testid="hosts-view" className="hosts-stack">
      <header className="hosts-head-bar">
        <div className="hosts-head-copy">
          {embedded ? (
            <h2 className="hosts-section-title">{t("setHosts")}</h2>
          ) : (
            <h1>{t("setHosts")}</h1>
          )}
          {/* Shown in both modes: where the observation surface went is the
              one thing this panel must always say. */}
          <p className="hosts-head-desc">{t("hostsDesc")}</p>
        </div>
        <Link className="btn ghost" data-testid="hosts-team-link" to="/agents">
          {t("hostsTeamLink")}
        </Link>
        <button
          type="button"
          className="btn ghost"
          data-testid="hosts-refresh"
          onClick={() => void onRefresh()}
          disabled={busy !== null}
        >
          {busy === REFRESH ? t("probing") : t("reprobe")}
        </button>
      </header>

      {actionError ? (
        <div
          data-testid="hosts-action-error"
          role="alert"
          className="badge warn"
          style={{ padding: "8px 12px", borderRadius: 10, fontSize: 12.5 }}
        >
          {actionError}
        </div>
      ) : null}

      {state.kind === "loading" ? (
        <p data-testid="hosts-loading" style={{ fontSize: 13, color: "var(--text-faint)" }}>
          {t("probing")}
        </p>
      ) : state.kind === "error" ? (
        <div
          data-testid="hosts-error"
          role="alert"
          style={{
            border: "1px solid var(--red)",
            background: "var(--red-soft)",
            color: "var(--red-text)",
            borderRadius: "var(--radius-card)",
            padding: "14px 16px",
            fontSize: 13.5,
          }}
        >
          探测主机失败: {state.message}
        </div>
      ) : (
        state.hosts.map((h) =>
          h.kind === "ready" ? (
            <HostActionRow
              key={h.detail.host}
              hostId={h.detail.host}
              hostname={h.detail.hostname}
              online
              actions={pendingActionsFor(h.detail)}
              notices={toolSurfaceNoticesFor(h.detail)}
              busy={busy}
              lang={lang}
              onRegister={(vendor) => void onRegister(h.detail.host, vendor)}
              onImport={(remoteSlug) => void onImport(h.detail.host, remoteSlug)}
            />
          ) : (
            <HostActionRow
              key={h.summary.host}
              hostId={h.summary.host}
              hostname={h.summary.hostname || h.summary.host}
              online={false}
              actions={[]}
              notices={[]}
              busy={busy}
              lang={lang}
              onRegister={() => {}}
              onImport={() => {}}
            />
          ),
        )
      )}

      <p className="text-xs text-text-muted">
        <a className="text-brand-400 hover:underline" href="/settings/access">
          {t("hostsAccessPointer")}
        </a>
      </p>
    </div>
  );
}

/** The 「连接新主机(卫星节点)」 card: shows the REAL join command (daemon
 *  origin + newest valid join token from `GET /hosts/join-token`) with a
 *  copy button; offers minting when no valid token exists yet. Admin-only
 *  data — a 403 (tenant) keeps the placeholder command and hides actions. */
export function JoinCard({
  lang = "zh",
  bare = false,
}: { lang?: Lang; /** remove the standalone shell when nested in a shared Card */ bare?: boolean } = {}) {
  const t = makeT(lang);
  const [info, setInfo] = useState<JoinTokenInfo | null>(null);
  const [allowed, setAllowed] = useState(true);
  const [busy, setBusy] = useState(false);
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    getJoinToken()
      .then((i) => {
        if (!cancelled) setInfo(i);
      })
      .catch(() => {
        // Authentication or transient failure: keep the placeholder, no CTA.
        if (!cancelled) setAllowed(false);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const origin =
    typeof window !== "undefined" && window.location ? window.location.origin : "https://<daemon>";
  const token = info?.token ?? null;
  // Full flow: install → start (the unified process) → join. The satellite
  // dials OUT to this daemon (reverse connection — it exposes no port); a
  // running `ccteam start` picks the join up within 30s and comes online.
  const command = `curl -fsSL https://ccteam.dev/install.sh | sh
ccteam start
ccteam host join --daemon ${origin} --token ${token ?? "<join-token>"}`;

  const onMint = async () => {
    setBusy(true);
    setError(null);
    try {
      setInfo(await mintJoinToken());
    } catch (e) {
      if (!(e instanceof Error && e.message === "UNAUTHENTICATED")) {
        setError(e instanceof Error ? e.message : "mint failed");
      }
    } finally {
      setBusy(false);
    }
  };

  const onCopy = async () => {
    setError(null);
    // copyText falls back to execCommand — the daemon is usually plain http://
    // on a remote IP, where `navigator.clipboard` is undefined.
    if (await copyText(command)) {
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } else {
      setError(t("joinTokenCopyFailed"));
    }
  };

  return (
    <div className={`join-card${bare ? " bare" : ""}`} data-testid="join-card">
      <h4>{t("joinTitle")}</h4>
      <p>{t("joinDesc")}</p>
      <pre data-testid="join-command">{command}</pre>
      {allowed ? (
        <div style={{ display: "flex", alignItems: "center", gap: 10, marginTop: 8 }}>
          {token ? (
            <button
              type="button"
              className="btn ghost mini"
              data-testid="join-copy"
              onClick={() => void onCopy()}
            >
              {copied ? t("joinTokenCopied") : t("joinTokenCopy")}
            </button>
          ) : (
            <button
              type="button"
              className="btn primary mini"
              data-testid="join-mint"
              disabled={busy}
              onClick={() => void onMint()}
            >
              {busy ? t("joinTokenGenBusy") : t("joinTokenGen")}
            </button>
          )}
          <span style={{ fontSize: 12, color: "var(--text-faint)" }}>{t("joinTokenHint")}</span>
        </div>
      ) : null}
      {error ? (
        <div role="alert" data-testid="join-error" className="badge warn" style={{ marginTop: 8 }}>
          {error}
        </div>
      ) : null}
    </div>
  );
}

/** One compact row per machine: identity (dot · hostname · host id) on the
 *  left, its to-do list on the right. Hook-free so the node test suite can
 *  walk it and fire `onClick` without a DOM. */
export function HostActionRow({
  hostId,
  hostname,
  online,
  actions,
  notices = [],
  busy,
  lang = "zh",
  onRegister,
  onImport,
}: {
  hostId: string;
  hostname: string;
  online: boolean;
  actions: PendingAction[];
  notices?: string[];
  busy: string | null;
  lang?: Lang;
  onRegister: (vendor: string) => void;
  onImport: (remoteSlug: string) => void;
}) {
  const t = makeT(lang);
  return (
    <div
      className={`host-actions${online ? "" : " offline"}`}
      data-testid={`host-actions-${hostId}`}
    >
      <div className="host-actions-head">
        <span className={`dot ${online ? "on" : "off"}`} />
        <span className="host-actions-name">{hostname}</span>
        <span className="host-actions-id mono">{hostId}</span>
      </div>
      <div className="host-actions-items">
        {actions.length === 0 ? (
          // An unreachable host has no to-do list we can trust — say that
          // rather than claiming it is clean.
          <span className="host-actions-idle" data-testid={`host-idle-${hostId}`}>
            {online ? t("nothingToDo") : t("offlineRow")}
          </span>
        ) : (
          actions.map((action) =>
            action.kind === "register" ? (
              <span className="host-action" key={`register:${action.vendor}`}>
                <span className={vendorDotClass(action.vendor)} />
                <span className="host-action-label">{action.vendor}</span>
                <button
                  type="button"
                  className="btn primary mini"
                  data-testid={`register-mcp-${action.vendor}`}
                  disabled={busy !== null}
                  onClick={() => onRegister(action.vendor)}
                >
                  {busy === registerKey(hostId, action.vendor)
                    ? t("registeringMcp")
                    : t("registerMcp")}
                </button>
              </span>
            ) : (
              <span className="host-action" key={`import:${action.slug}`}>
                <span className="host-action-label mono" title={action.path}>
                  {action.slug}
                </span>
                <button
                  type="button"
                  className="btn primary mini"
                  data-testid={`import-project-${action.slug}`}
                  disabled={busy !== null}
                  onClick={() => onImport(action.slug)}
                >
                  {busy === importKey(hostId, action.slug)
                    ? t("importingProject")
                    : t("importProject")}
                </button>
              </span>
            ),
          )
        )}
      </div>
      {notices.map((notice) => (
        <p
          className="host-actions-idle"
          data-testid={`host-tool-surface-${hostId}`}
          key={notice}
        >
          {notice}
        </p>
      ))}
    </div>
  );
}
