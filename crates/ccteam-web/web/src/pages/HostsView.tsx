// v0.9.13 — 主机与 harness 管理 (设置→运维总览), grown out of the v0.9.11
// TEAM-9 action-only panel: ops can now SEE and MANAGE every vendor harness
// per host, not just the rows that happened to need something.
//
// One card per machine, one row per vendor (the full `AGENT_PROBE_SPECS`
// axis): installed / version / ready-state badge / MCP registration state,
// plus the management actions that exist server-side —
//   · register-mcp — write ccteam's own MCP server into a LOCAL vendor
//     config (never a vendor login; the backend 404s non-local, so
//     satellites render the state without a CTA). The same endpoint with
//     `?vendor=dsh` registers ccteam's DSH plugin into the operator's
//     ~/.dsh web profile instead (ADMIN-only CTA on the dsh row; takes
//     effect when the human restarts their own `dsh web`).
//   · install / update (VENDOR-INSTALL-1) — ADMIN-only one-click npm
//     install/update on the local host (recipe argv is pinned server-side in
//     `AgentProbeSpec::install_recipe` and runs shell-free; kimi/pi have no
//     recipe and keep manual guidance with a docs link). The row polls the
//     job and re-probes the host on success. The 403 is the real gate —
//     `useMe().isAdmin` only hides the button.
//   · import — adopt a satellite-reported project into the daemon catalog.
// A vendor that is not installed shows its remediation `hint` verbatim from
// the backend.
//
// Fleet observation (live session counts, spend, offline age, host removal)
// stays on the Team page's charter roster; the header links there. JoinCard
// (the real `ccteam host join` command) is exported from here and ALSO
// rendered by AccessView (设置·接入), where this panel's footer points.
//
// Data: GET /api/v1/hosts (registry) fanned into GET /api/v1/hosts/{host}; a
// host whose detail probe fails renders offline (honest state — we then say
// we cannot see what it needs, not that there is nothing to do).

import { useCallback, useEffect, useState } from "react";
import { Link } from "react-router-dom";
import {
  getHostDetail,
  getHosts,
  getInstallJob,
  getJoinToken,
  installVendor,
  mintJoinToken,
  registerMcp,
  type AgentHealth,
  type HostDetail,
  type HostSummary,
  type InstallJob,
  type JoinTokenInfo,
} from "../lib/hostsApi";
import { importProject } from "../lib/dashboardApi";
import { copyText } from "../lib/clipboard";
import { makeT, type Lang } from "../lib/i18n";
import { vendorDotClass } from "../lib/vendors";
import { fetchVendorLatests, isOutdated, npmPackageForVendor } from "../lib/vendorLatest";
import { getVendorQuotas, type VendorQuota } from "../lib/vendorQuotaApi";
import { quotaLines, quotaPlan } from "../lib/quotaBars";
import { useMe } from "../hooks/useMe";

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

/** Actionable items for one probed host — the ELIGIBILITY single home the
 *  vendor rows consult before offering a CTA.
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

/** VENDOR-INSTALL-1 — what the row's one-click CTA offers, if anything.
 *  `install` for a missing npm-packaged vendor, `update` when the npm
 *  "latest" is strictly newer than the probe version, `none` otherwise
 *  (up-to-date, non-admin, satellite, or a recipe-less vendor — kimi/pi keep
 *  their manual-install hint). Pure + hook-free so the node tests pin the
 *  three states without a DOM. */
// eslint-disable-next-line react-refresh/only-export-components -- pure helper co-located with its only consumer for unit tests.
export function installCtaFor(
  agent: AgentHealth,
  opts: { isAdmin: boolean; isLocal: boolean; latest: string | null },
): { kind: "install" } | { kind: "update"; latest: string } | { kind: "none" } {
  if (!opts.isAdmin || !opts.isLocal) return { kind: "none" };
  if (npmPackageForVendor(agent.vendor) === null) return { kind: "none" };
  if (!agent.installed) return { kind: "install" };
  if (opts.latest && isOutdated(agent.version, opts.latest)) {
    return { kind: "update", latest: opts.latest };
  }
  return { kind: "none" };
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

/** VENDOR-INSTALL-1 — poll a running install job to its terminal state,
 *  reporting every snapshot through `onUpdate`. Module-level (not a hook):
 *  the reassignment loop is plain async control flow, kept out of the
 *  component so the react-hooks immutability rule stays silent. */
async function pollInstallJob(
  vendor: string,
  started: InstallJob,
  onUpdate: (job: InstallJob) => void,
): Promise<InstallJob> {
  let job = started;
  while (job.state === "running") {
    await new Promise((resolve) => setTimeout(resolve, 1500));
    job = await getInstallJob("local", vendor, job.job_id);
    onUpdate(job);
  }
  return job;
}

export default function HostsView({
  lang = "zh",
  embedded = false,
}: { lang?: Lang; /** hide page title when nested under Ops panel */ embedded?: boolean } = {}) {
  const t = makeT(lang);
  const { isAdmin } = useMe();
  const [state, setState] = useState<LoadState>({ kind: "loading" });
  /** vendor token currently registering (scoped per host:vendor), or REFRESH. */
  const [busy, setBusy] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  /** npm "latest" per vendor (best-effort, for the Update CTA). */
  const [latests, setLatests] = useState<Record<string, string>>({});
  /** Local-host install jobs by vendor — running progress or a kept failure. */
  const [installJobs, setInstallJobs] = useState<Record<string, InstallJob>>({});
  /** VENDOR-QUOTA-1 — quota rows by vendor (admin only; see the effect). */
  const [quotas, setQuotas] = useState<Record<string, VendorQuota>>({});

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

  // Latest published versions for the Update CTA — fired once over the whole
  // vendor axis (mirrors the Team roster); `fetchVendorLatests` drops the
  // vendors it has no npm channel for (kimi/pi) and caches the rest. Pure
  // decoration: any failure leaves the map empty and rows simply never show
  // an Update button.
  useEffect(() => {
    let cancelled = false;
    fetchVendorLatests(["claude", "codex", "grok", "opencode", "kimi", "pi", "dsh"])
      .then((map) => {
        if (!cancelled) setLatests(map);
      })
      .catch(() => {
        if (!cancelled) setLatests({});
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // VENDOR-QUOTA-1 — vendor quota bars, ADMIN only: the endpoint 403s a
  // tenant, and `isAdmin` is fail-closed (stays false until /me resolves),
  // so a tenant never fires the request. Any failure leaves the map empty
  // and the rows render no quota zone — exactly the not_subscription /
  // unavailable presentation.
  useEffect(() => {
    if (!isAdmin) return;
    let cancelled = false;
    getVendorQuotas()
      .then((rows) => {
        if (cancelled) return;
        setQuotas(Object.fromEntries(rows.map((row) => [row.vendor, row])));
      })
      .catch(() => {
        if (!cancelled) setQuotas({});
      });
    return () => {
      cancelled = true;
    };
  }, [isAdmin]);

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

  /** VENDOR-INSTALL-1 — start (or join) the local one-click install/update
   *  job for one vendor, poll it to a terminal state, then re-probe the host
   *  (`refresh: true` breaks the daemon's probe cache so a freshly installed
   *  binary shows up). A failure keeps the job in the map so the row renders
   *  the installer's own output tail — no error styling beyond that line. */
  const onInstall = async (vendor: string) => {
    setActionError(null);
    const clearJob = () =>
      setInstallJobs((prev) => {
        const next = { ...prev };
        delete next[vendor];
        return next;
      });
    try {
      const started = await installVendor("local", vendor);
      setInstallJobs((prev) => ({ ...prev, [vendor]: started }));
      const final = await pollInstallJob(vendor, started, (job) =>
        setInstallJobs((prev) => ({ ...prev, [vendor]: job })),
      );
      if (final.state === "ok") {
        await load(true);
        clearJob();
      }
    } catch (e) {
      if (!(e instanceof Error && e.message === "UNAUTHENTICATED")) {
        setActionError(
          `${t("installFailed")}（${vendor}）: ${e instanceof Error ? e.message : t("unknownError")}`,
        );
        clearJob();
      }
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
            <HostManageCard
              key={h.detail.host}
              detail={h.detail}
              busy={busy}
              lang={lang}
              isAdmin={isAdmin}
              latests={latests}
              installJobs={installJobs}
              quotas={quotas}
              onRegister={(vendor) => void onRegister(h.detail.host, vendor)}
              onImport={(remoteSlug) => void onImport(h.detail.host, remoteSlug)}
              onInstall={(vendor) => void onInstall(vendor)}
            />
          ) : (
            <OfflineHostCard
              key={h.summary.host}
              hostId={h.summary.host}
              hostname={h.summary.hostname || h.summary.host}
              lang={lang}
            />
          ),
        )
      )}

      <p className="text-xs text-text-muted">
        <Link className="text-brand-400 hover:underline" to="/settings/access">
          {t("hostsAccessPointer")}
        </Link>
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

/** Ready-state → badge, verbatim off the API (an unknown status falls
 *  through as its own label — honesty over prettiness). */
function statusBadge(status: string, t: (key: string) => string): { cls: string; label: string } {
  if (status === "ready") return { cls: "badge ok", label: t("rosterStatusReady") };
  if (status === "needs_config") return { cls: "badge warn", label: t("rosterStatusNeedsConfig") };
  if (status === "not_installed") return { cls: "badge", label: t("notInstalled") };
  return { cls: "badge", label: status };
}

/** The last non-empty line of a job's output tail — the inline progress /
 *  diagnostic line; the full tail rides the `title` tooltip. */
function tailLastLine(job: InstallJob): string {
  const lines = job.output_tail.split("\n").filter((line) => line.trim().length > 0);
  return lines[lines.length - 1] ?? "";
}

/** One vendor's management row: identity · version · ready badge · MCP
 *  registration state (CTA only where {@link pendingActionsFor} says the
 *  backend will accept it) · VENDOR-INSTALL-1 one-click install/update CTA
 *  (admin + local + npm recipe only; the backend 403 is the real gate) ·
 *  remediation hint verbatim. Hook-free. */
function VendorManageRow({
  hostId,
  agent,
  registerable,
  busy,
  lang = "zh",
  isLocal = false,
  isAdmin = false,
  latest = null,
  npmAvailable = false,
  job = null,
  quota = null,
  onRegister,
  onInstall,
}: {
  hostId: string;
  agent: AgentHealth;
  /** Vendors {@link pendingActionsFor} deems register-eligible on this host. */
  registerable: ReadonlySet<string>;
  busy: string | null;
  lang?: Lang;
  /** Install CTA inputs — the card is local-host + admin-only. */
  isLocal?: boolean;
  isAdmin?: boolean;
  /** npm "latest" for this vendor, when known. */
  latest?: string | null;
  /** Whether npm runs on this host (every recipe is npm-based). */
  npmAvailable?: boolean;
  /** The vendor's current/last install job, if any. */
  job?: InstallJob | null;
  /** VENDOR-QUOTA-1 — this vendor's quota row, when the admin fetched any. */
  quota?: VendorQuota | null;
  onRegister: (vendor: string) => void;
  onInstall?: (vendor: string) => void;
}) {
  const t = makeT(lang);
  const badge = statusBadge(agent.status, t);
  const cta = installCtaFor(agent, { isAdmin, isLocal, latest });
  const jobRunning = job?.state === "running";
  // Quota: up to two window lines + an optional plan badge; every
  // non-available state collapses to nothing (no error styling).
  const plan = quotaPlan(quota);
  const lines = quotaLines(quota, new Date(), lang);
  return (
    <div className="host-vendor-row" data-testid={`host-vendor-${hostId}-${agent.vendor}`}>
      <span className={vendorDotClass(agent.vendor)} />
      <span className="host-vendor-name">{agent.vendor}</span>
      <span className="host-vendor-version mono" data-testid={`host-vendor-version-${agent.vendor}`}>
        {agent.installed ? (agent.version ?? "—") : t("notInstalled")}
      </span>
      <span className={badge.cls}>{badge.label}</span>
      {plan ? (
        <span className="badge" data-testid={`quota-plan-${agent.vendor}`}>
          {plan}
        </span>
      ) : null}
      {lines.length > 0 ? (
        <span className="host-vendor-quota" data-testid={`quota-bars-${agent.vendor}`}>
          {lines.map((line) => (
            <span key={line} className="host-vendor-quota-line mono">
              {line}
            </span>
          ))}
        </span>
      ) : null}
      <span className="host-vendor-mcp">
        {agent.tool_surface === "native_mcp_config" && agent.installed ? (
          agent.mcp_registered ? (
            <span className="ok" data-testid={`host-vendor-mcp-ok-${agent.vendor}`}>
              ✓ {t("mcpOk")}
            </span>
          ) : registerable.has(agent.vendor) ? (
            <button
              type="button"
              className="btn primary mini"
              data-testid={`register-mcp-${agent.vendor}`}
              disabled={busy !== null}
              onClick={() => onRegister(agent.vendor)}
            >
              {busy === registerKey(hostId, agent.vendor)
                ? t("registeringMcp")
                : t("registerMcp")}
            </button>
          ) : (
            // Installed but unregistered on a host where the backend refuses
            // the write (satellite): state without a dead-end CTA.
            <span>{t("mcpNotRegistered")}</span>
          )
        ) : agent.vendor === "dsh" && agent.installed && agent.mcp_registered ? (
          // Already in the operator's ~/.dsh web profile — state, no CTA.
          <span className="ok" data-testid="host-vendor-dsh-plugin-ok">
            ✓ {t("dshPluginOk")}
          </span>
        ) : agent.vendor === "dsh" && agent.installed && isLocal && isAdmin ? (
          // DSH's "register ccteam" writes ccteam's plugin bundle + patch row
          // into the operator's ~/.dsh web profile (server-side gate ①); the
          // human then restarts their own `dsh web` to load it. Idempotent, so
          // the CTA stays until the profile actually carries our row.
          <button
            type="button"
            className="btn primary mini"
            data-testid="register-dsh-plugin"
            disabled={busy !== null}
            onClick={() => onRegister(agent.vendor)}
          >
            {busy === registerKey(hostId, agent.vendor)
              ? t("registeringMcp")
              : t("registerDshPlugin")}
          </button>
        ) : null}
      </span>
      {cta.kind !== "none" ? (
        <span className="host-vendor-install">
          <button
            type="button"
            className="btn ghost mini"
            data-testid={`install-vendor-${agent.vendor}`}
            disabled={!npmAvailable || jobRunning}
            title={!npmAvailable ? t("npmMissingHint") : undefined}
            onClick={() => onInstall?.(agent.vendor)}
          >
            {jobRunning
              ? t("installingVendor")
              : cta.kind === "update"
                ? `${t("updateVendor")} → ${cta.latest}`
                : t("install")}
          </button>
        </span>
      ) : null}
      {jobRunning && job ? (
        <span
          className="host-vendor-hint mono"
          data-testid={`install-progress-${agent.vendor}`}
          title={job.output_tail}
        >
          {t("installingVendor")} {tailLastLine(job)}
        </span>
      ) : null}
      {job?.state === "failed" ? (
        <span
          className="host-vendor-hint mono"
          data-testid={`install-failed-${agent.vendor}`}
          title={job.output_tail}
        >
          {t("installFailed")}
          {job.exit_code !== null ? ` (exit ${job.exit_code})` : ""}
          {tailLastLine(job) ? ` — ${tailLastLine(job)}` : ""}
        </span>
      ) : null}
      {agent.hint ? <span className="host-vendor-hint mono">{agent.hint}</span> : null}
    </div>
  );
}

/** One machine's management card: identity head (dot · hostname · host id ·
 *  os/arch · ccteam build), the full vendor inventory, tool-surface notices,
 *  and — for a satellite — its reported projects with adopt state. Hook-free
 *  so the node test suite can walk it and fire `onClick` without a DOM. */
export function HostManageCard({
  detail,
  busy,
  lang = "zh",
  isAdmin = false,
  latests = {},
  installJobs = {},
  quotas = {},
  onRegister,
  onImport,
  onInstall,
}: {
  detail: HostDetail;
  busy: string | null;
  lang?: Lang;
  /** VENDOR-INSTALL-1 — the caller's admin flag (fail-closed default), the
   *  npm-latest map, and the local host's install jobs by vendor. */
  isAdmin?: boolean;
  latests?: Record<string, string>;
  installJobs?: Record<string, InstallJob>;
  /** VENDOR-QUOTA-1 — quota rows by vendor (admin only; empty otherwise). */
  quotas?: Record<string, VendorQuota>;
  onRegister: (vendor: string) => void;
  onImport: (remoteSlug: string) => void;
  onInstall?: (vendor: string) => void;
}) {
  const t = makeT(lang);
  const registerable: ReadonlySet<string> = new Set(
    pendingActionsFor(detail).flatMap((a) => (a.kind === "register" ? [a.vendor] : [])),
  );
  const notices = toolSurfaceNoticesFor(detail);
  const projects = detail.is_local ? [] : (detail.projects ?? []);
  return (
    <div className="host-manage" data-testid={`host-manage-${detail.host}`}>
      <div className="host-actions-head">
        <span className="dot on" />
        <span className="host-actions-name">{detail.hostname}</span>
        <span className="host-actions-id mono">{detail.host}</span>
        <span className="host-actions-id mono" style={{ marginLeft: "auto" }}>
          {detail.os}/{detail.arch} · ccteam {detail.ccteam_version}
        </span>
      </div>
      <div className="host-vendors">
        {detail.agents.map((agent) => (
          <VendorManageRow
            key={agent.vendor}
            hostId={detail.host}
            agent={agent}
            registerable={registerable}
            busy={busy}
            lang={lang}
            isLocal={detail.is_local}
            isAdmin={isAdmin}
            latest={latests[agent.vendor] ?? null}
            npmAvailable={detail.npm_available ?? false}
            job={installJobs[agent.vendor] ?? null}
            quota={quotas[agent.vendor] ?? null}
            onRegister={onRegister}
            onInstall={onInstall}
          />
        ))}
      </div>
      {notices.map((notice) => (
        <p
          className="host-actions-idle"
          data-testid={`host-tool-surface-${detail.host}`}
          key={notice}
        >
          {notice}
        </p>
      ))}
      {projects.length > 0 ? (
        <div className="host-projects" data-testid={`host-projects-${detail.host}`}>
          <span className="host-projects-title">{t("hostSatProjects")}</span>
          {projects.map((project) => (
            <span className="host-action" key={project.slug}>
              <span className="host-action-label mono" title={project.path}>
                {project.slug}
              </span>
              {project.cataloged ? (
                <span className="badge ok" data-testid={`host-project-adopted-${project.slug}`}>
                  {t("hostCataloged")}
                  {project.catalog_slug && project.catalog_slug !== project.slug
                    ? ` → ${project.catalog_slug}`
                    : ""}
                </span>
              ) : (
                <button
                  type="button"
                  className="btn primary mini"
                  data-testid={`import-project-${project.slug}`}
                  disabled={busy !== null}
                  onClick={() => onImport(project.slug)}
                >
                  {busy === importKey(detail.host, project.slug)
                    ? t("importingProject")
                    : t("importProject")}
                </button>
              )}
            </span>
          ))}
        </div>
      ) : null}
    </div>
  );
}

/** A registered host whose detail probe failed: identity + the honest
 *  offline line (we cannot see what it needs — not "nothing to do"). */
export function OfflineHostCard({
  hostId,
  hostname,
  lang = "zh",
}: {
  hostId: string;
  hostname: string;
  lang?: Lang;
}) {
  const t = makeT(lang);
  return (
    <div className="host-manage offline" data-testid={`host-manage-${hostId}`}>
      <div className="host-actions-head">
        <span className="dot off" />
        <span className="host-actions-name">{hostname}</span>
        <span className="host-actions-id mono">{hostId}</span>
      </div>
      <span className="host-actions-idle" data-testid={`host-offline-${hostId}`}>
        {t("offlineRow")}
      </span>
    </div>
  );
}
