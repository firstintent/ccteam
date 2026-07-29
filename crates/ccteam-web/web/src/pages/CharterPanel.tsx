// v0.9.11 TEAM-2 — 分工 charter tab body (Team page). Four blocks:
//
// - Vendor roster (TEAM-6: grouped by host): one collapsible section per
//   host — header = hostname + ALWAYS-shown mono host-id badge (the
//   disambiguator two hosts sharing an OS hostname need) + online/offline
//   dot, then that host's per-vendor cards straight off the hosts agent
//   report (installed/version/status never invented; live-session count +
//   Σcost aggregated from the SAME graph nodes the topology tab already
//   fetched, passed down as a prop, no refetch). Sort = `local` first, then
//   online before offline; offline (non-local) sections start collapsed.
//   Non-local headers carry a 移除/Remove button — offline fires the DELETE
//   immediately, online arms a same-button two-click confirm first. Cards
//   click through to the topology tab filtered to that vendor (TEAM-7).
// - 编队起手 playbook cards (TEAM-3): the shared formation definitions from
//   `lib/playbooks.ts` (same array the Home launcher renders — UI
//   documentation only, no shipped prompts/personas); the 起手 CTA hands off
//   to the Home composer via one-shot router state `{ playbook: id }`.
// - Charter editor: per-project `.ccteam/routing.md` (division-of-labor
//   charter). project source → editable; global source → read-only fallback
//   with 拷入起稿 / 空白起稿 CTAs; none → 空白起稿. Saving PUTs the PROJECT
//   file only (the global file's write surface stays CLI/filesystem).
// - Standing honesty note: agents PULL this file via the MCP status tool
//   (advisory, never injected); >~4k chars is excerpted there.
//
// State machine = `lib/charterState.ts` (pure reducer, node-env tested);
// roster grouping/remove orchestration = `lib/charterRoster.ts` (also pure,
// also its own test file — kept out of this page module so its plain
// `handleRosterRemoveClick` function doesn't trip
// `react-refresh/only-export-components`). The hook-free views below are
// exported for the SSR test suite.

import { useEffect, useReducer, useState } from "react";
import { Link } from "react-router-dom";
import type { AgentNode } from "../lib/agentsApi";
import { charterReducer, initialCharter, type CharterState } from "../lib/charterState";
import {
  handleRosterRemoveClick,
  offlineAge,
  sortRosterHosts,
  type RosterHost,
} from "../lib/charterRoster";
import { fetchDashboard, type DashboardRow } from "../lib/dashboardApi";
import { getHostDetail, getHosts } from "../lib/hostsApi";
import { getRouting, putRouting } from "../lib/routingApi";
import { makeT, tRosterOfflineFor, type Lang } from "../lib/i18n";
import { PLAYBOOKS } from "../lib/playbooks";
import { toastBus } from "../lib/toastBus";
import { VendorChip } from "../components/VendorChip";
import { Markdown } from "../components/Markdown";

export type { RosterHost } from "../lib/charterRoster";

/** Status → badge class/label. Renders EXACTLY what the API reports — an
 *  unknown status falls through verbatim (honesty over prettiness). */
function rosterBadge(status: string, t: (key: string) => string): { cls: string; label: string } {
  if (status === "ready") return { cls: "badge ok", label: t("rosterStatusReady") };
  if (status === "needs_config") return { cls: "badge warn", label: t("rosterStatusNeedsConfig") };
  if (status === "not_installed") return { cls: "badge", label: t("notInstalled") };
  return { cls: "badge", label: status };
}

/** Shared empty-Set default for `VendorRosterCards`' `collapsed` prop — a
 *  module-level constant so the default doesn't allocate a fresh Set (and
 *  thus a fresh identity) on every render. */
const EMPTY_COLLAPSED: Set<string> = new Set();

/** Vendor roster cards — hook-free presentational (exported for node-env
 *  tests): grouped one section per host (TEAM-6), `local` first then online
 *  before offline (see {@link sortRosterHosts}). `nodes` = the topology
 *  tab's graph nodes (prop-drilled, not refetched); live/Σcost aggregate
 *  over (host, vendor). Collapse + remove-confirm are OWNED BY THE CALLER
 *  (`collapsed` / `confirmingHost` state + callbacks) rather than internal
 *  `useState`, precisely so this view stays hook-free and directly
 *  callable from node-env tests (same reason {@link CharterEditorView}
 *  externalizes its state) — `CharterPanel` below wires the actual
 *  `useState<Set<string>>` (the AgentsTree collapsed-Set idiom) + the
 *  {@link handleRosterRemoveClick} orchestration.
 *
 *  TEAM-7: a card answers "what is this vendor doing?" — with `onVendorPick`
 *  it turns interactive (button role + Enter key + hover, like a tree row)
 *  and hands the vendor up; the caller lands on the filtered topology. No
 *  callback = pure display (no role, no pointer), so any other embedder of
 *  this view keeps the old behavior.
 *
 *  TEAM-8: an offline group also says HOW LONG it has been out of touch
 *  (`offlineAge`) and, past `STALE_AFTER_DAYS`, SUGGESTS cleanup (subdued
 *  hint + warn emphasis on the remove button). Suggestion only — ccteam
 *  never removes a host on its own; the click stays the user's. The clock
 *  arrives as the `nowMs` prop rather than being read here: `Date.now()`
 *  during render is impure (and `react-hooks/purity` rejects it outright),
 *  so the caller stamps it where the data lands. */
export function VendorRosterCards({
  hosts,
  nodes,
  lang: langProp,
  collapsed = EMPTY_COLLAPSED,
  onToggleCollapse = () => {},
  confirmingHost = null,
  onRemoveClick = () => {},
  onVendorPick,
  nowMs,
}: {
  hosts: RosterHost[];
  nodes: AgentNode[];
  lang?: Lang;
  /** Host ids currently collapsed (cards hidden, header stays). */
  collapsed?: Set<string>;
  onToggleCollapse?: (host: string) => void;
  /** Host id currently armed for a second-click remove confirmation. */
  confirmingHost?: string | null;
  onRemoveClick?: (host: string, online: boolean) => void;
  /** Present = cards are clickable "show me this vendor's sessions". */
  onVendorPick?: (vendor: string) => void;
  /** Clock the offline age is measured against (ms). Owned by the caller —
   *  omit it and no age is shown at all, which is the honest default for an
   *  embedder that has no clock to offer. */
  nowMs?: number;
}) {
  const lang = langProp ?? "zh";
  const t = makeT(lang);
  if (hosts.length === 0) return null;
  const sorted = sortRosterHosts(hosts);
  const pickable = onVendorPick != null;
  return (
    <section className="charter-roster-section">
      <h3>{t("charterRoster")}</h3>
      <div className="charter-roster-groups" data-testid="charter-roster">
        {sorted.map(({ host, hostname, status, agents, last_heartbeat_unix }) => {
          const isLocal = host === "local";
          const online = status === "online";
          const isCollapsed = collapsed.has(host);
          // Age is an OFFLINE-only story: an online host's heartbeat is by
          // definition fresh, so there is nothing worth reporting.
          const age = online || nowMs == null ? null : offlineAge(last_heartbeat_unix, nowMs);
          return (
            <div
              key={host}
              className="charter-roster-group"
              data-testid={`charter-roster-group-${host}`}
            >
              <div className="charter-roster-group-head" data-testid={`charter-roster-group-head-${host}`}>
                <button
                  type="button"
                  className="charter-roster-group-toggle"
                  aria-label={isCollapsed ? t("expand") : t("collapse")}
                  aria-expanded={!isCollapsed}
                  data-testid={`charter-roster-group-toggle-${host}`}
                  onClick={() => onToggleCollapse(host)}
                >
                  {isCollapsed ? "›" : "⌄"}
                </button>
                <span className="charter-roster-group-hostname">{hostname || host}</span>
                <span className="charter-roster-group-id mono">{host}</span>
                <span className={online ? "dot on" : "dot off"} aria-hidden="true" />
                <span className="charter-roster-group-status">
                  {online ? t("rosterHostOnline") : t("rosterHostOffline")}
                </span>
                {age ? (
                  <span
                    className="charter-roster-group-age"
                    data-testid={`charter-roster-age-${host}`}
                  >
                    {tRosterOfflineFor(lang, age.label === "days" ? age.days : age.hours, age.label)}
                  </span>
                ) : null}
                {age?.stale ? (
                  <span
                    className="charter-roster-group-stale"
                    data-testid={`charter-roster-stale-${host}`}
                  >
                    {t("rosterStaleHint")}
                  </span>
                ) : null}
                {!isLocal ? (
                  <button
                    type="button"
                    className={
                      age?.stale
                        ? "btn ghost mini charter-roster-remove warn"
                        : "btn ghost mini charter-roster-remove"
                    }
                    data-testid={`charter-roster-remove-${host}`}
                    onClick={() => onRemoveClick(host, online)}
                  >
                    {confirmingHost === host ? t("rosterRemoveConfirm") : t("rosterRemove")}
                  </button>
                ) : null}
              </div>
              {!isCollapsed ? (
                <div className="charter-roster" data-testid={`charter-roster-cards-${host}`}>
                  {agents.map((agent) => {
                    const mine = nodes.filter((n) => n.host === host && n.vendor === agent.vendor);
                    const live = mine.filter((n) => n.status === "live").length;
                    const cost = mine.reduce((sum, n) => sum + (n.cost_usd ?? 0), 0);
                    const badge = rosterBadge(agent.status, t);
                    return (
                      <div
                        key={`${host}-${agent.vendor}`}
                        className={pickable ? "charter-roster-card pickable" : "charter-roster-card"}
                        data-testid={`charter-roster-card-${host}-${agent.vendor}`}
                        role={pickable ? "button" : undefined}
                        tabIndex={pickable ? 0 : undefined}
                        title={pickable ? t("rosterPickHint") : undefined}
                        onClick={pickable ? () => onVendorPick?.(agent.vendor) : undefined}
                        onKeyDown={
                          pickable
                            ? (event) => {
                                if (event.key === "Enter") onVendorPick?.(agent.vendor);
                              }
                            : undefined
                        }
                      >
                        <div className="charter-roster-head">
                          <VendorChip vendor={agent.vendor} />
                          <span className="charter-roster-vendor">{agent.vendor}</span>
                          <span className={badge.cls}>{badge.label}</span>
                        </div>
                        <div className="charter-roster-meta mono">
                          {agent.installed ? agent.version ?? "—" : t("notInstalled")}
                        </div>
                        {agent.hint ? <div className="charter-roster-hint mono">{agent.hint}</div> : null}
                        <div className="charter-roster-stats">
                          <span className="charter-roster-live">{`●${live}`}</span>
                          <span>{t("teamKpiLive")}</span>
                          <span className="mono">{`$${cost.toFixed(2)}`}</span>
                        </div>
                      </div>
                    );
                  })}
                </div>
              ) : null}
            </div>
          );
        })}
      </div>
    </section>
  );
}

/** 编队起手 formation cards — hook-free presentational (exported for node-env
 *  tests) over the SAME `lib/playbooks.ts` array the Home launcher renders.
 *  Each card = icon + name + one-line description + vendor lineup chips; the
 *  起手 CTA is a plain `Link` to the Home composer carrying one-shot router
 *  state `{ playbook: id }` (HomeView applies it like a card click). The
 *  subdued honesty line keeps the promise exact: prefill only, orchestration
 *  happens inside the spawned session. */
export function PlaybookCards({ lang: langProp }: { lang?: Lang }) {
  const t = makeT(langProp ?? "zh");
  return (
    <section className="charter-playbooks-section">
      <h3>{t("playbookSection")}</h3>
      <div className="charter-playbooks" data-testid="charter-playbooks">
        {PLAYBOOKS.map(({ id, key, Icon, vendors }) => (
          <div key={id} className="charter-playbook-card" data-testid={`playbook-${id}`}>
            <div className="charter-playbook-head">
              <Icon />
              <span className="charter-playbook-name">{t(`${key}T`)}</span>
            </div>
            <div className="charter-playbook-desc">{t(`${key}D`)}</div>
            <div className="charter-playbook-foot">
              <span className="vs">
                {vendors.map((v) => (
                  <VendorChip key={v} vendor={v} />
                ))}
              </span>
              <Link
                className="btn ghost mini"
                to="/"
                state={{ playbook: id }}
                data-testid={`playbook-launch-${id}`}
              >
                {t("playbookLaunch")}
              </Link>
            </div>
          </div>
        ))}
      </div>
      <p className="charter-note" data-testid="playbook-honesty">
        {t("playbookHonesty")}
      </p>
    </section>
  );
}

/** Charter editor — hook-free view over {@link CharterState} (exported for
 *  node-env tests). All transitions go through the callbacks; the stateful
 *  wiring lives in {@link CharterPanel}. */
export function CharterEditorView({
  state,
  lang: langProp,
  onStartDraft,
  onEdit,
  onTogglePreview,
  onSave,
}: {
  state: CharterState;
  lang?: Lang;
  onStartDraft: (from: "copy" | "blank") => void;
  onEdit: (content: string) => void;
  onTogglePreview: () => void;
  onSave: () => void;
}) {
  const t = makeT(langProp ?? "zh");
  const { doc, draft, dirty, previewing, saving, saved, error, loading } = state;

  let body;
  if (loading) {
    body = <p className="charter-muted">{t("loading")}</p>;
  } else if (!doc) {
    body = <p className="charter-muted">{`${t("charterLoadFailed")}${error ? ` — ${error}` : ""}`}</p>;
  } else if (draft != null) {
    // Editing (project file, or a draft started from global/none).
    body = (
      <>
        <div className="charter-editor-bar">
          <div className="seg">
            <button
              type="button"
              className={previewing ? "" : "active"}
              data-testid="charter-mode-edit"
              onClick={() => {
                if (previewing) onTogglePreview();
              }}
            >
              {t("charterEdit")}
            </button>
            <button
              type="button"
              className={previewing ? "active" : ""}
              data-testid="charter-mode-preview"
              onClick={() => {
                if (!previewing) onTogglePreview();
              }}
            >
              {t("charterPreview")}
            </button>
          </div>
          <span className="charter-receipt mono" data-testid="charter-receipt">
            {saving
              ? t("charterSaving")
              : dirty
                ? t("charterUnsaved")
                : saved
                  ? `${saved.sha256.slice(0, 8)} · ${saved.updated_at.slice(0, 16).replace("T", " ")}`
                  : ""}
          </span>
          <button
            type="button"
            className="btn primary mini"
            data-testid="charter-save"
            disabled={!dirty || saving}
            onClick={onSave}
          >
            {t("save")}
          </button>
        </div>
        {error ? <p className="charter-error" role="alert">{error}</p> : null}
        {previewing ? (
          <Markdown className="charter-preview" content={draft} />
        ) : (
          <textarea
            className="charter-textarea mono"
            data-testid="charter-textarea"
            value={draft}
            spellCheck={false}
            onChange={(event) => onEdit(event.target.value)}
          />
        )}
      </>
    );
  } else if (doc.source === "global") {
    // Read-only global fallback + the two draft CTAs.
    body = (
      <>
        <p className="charter-muted" data-testid="charter-global-note">
          {t("charterGlobalFallback")}
          {doc.fallback_path ? <span className="mono"> ({doc.fallback_path})</span> : null}
        </p>
        <Markdown className="charter-preview readonly" content={doc.content} />
        <div className="charter-ctas">
          <button
            type="button"
            className="btn primary mini"
            data-testid="charter-copy-draft"
            onClick={() => onStartDraft("copy")}
          >
            {t("charterCopyDraft")}
          </button>
          <button
            type="button"
            className="btn ghost mini"
            data-testid="charter-blank-draft"
            onClick={() => onStartDraft("blank")}
          >
            {t("charterBlankDraft")}
          </button>
        </div>
      </>
    );
  } else {
    // source === "none": nothing anywhere → blank start only.
    body = (
      <>
        <p className="charter-muted" data-testid="charter-none-note">{t("charterNone")}</p>
        <div className="charter-ctas">
          <button
            type="button"
            className="btn primary mini"
            data-testid="charter-blank-draft"
            onClick={() => onStartDraft("blank")}
          >
            {t("charterBlankDraft")}
          </button>
        </div>
      </>
    );
  }

  return (
    <div className="charter-editor" data-testid="charter-editor">
      {doc ? <div className="charter-path mono">{doc.path}</div> : null}
      {body}
    </div>
  );
}

/** 分工 charter tab body. `nodes` come from AgentsView's already-fetched
 *  graph (single source for the roster aggregation); `onVendorPick` is that
 *  same view's topology filter (TEAM-7), threaded straight to the roster. */
export default function CharterPanel({
  nodes,
  lang: langProp,
  onVendorPick,
}: {
  nodes: AgentNode[];
  lang?: Lang;
  onVendorPick?: (vendor: string) => void;
}) {
  const lang = langProp ?? "zh";
  const t = makeT(lang);
  const [projects, setProjects] = useState<DashboardRow[] | null>(null);
  const [slug, setSlug] = useState<string | null>(null);
  const [roster, setRoster] = useState<RosterHost[]>([]);
  // The AgentsTree collapsed-Set idiom (`useState<Set<string>>`, toggle on
  // click) — seeded once with every offline non-local host when the roster
  // first loads (see the effect below), then owned by the user's clicks.
  const [rosterCollapsed, setRosterCollapsed] = useState<Set<string>>(() => new Set());
  const [confirmingHost, setConfirmingHost] = useState<string | null>(null);
  const [state, dispatch] = useReducer(charterReducer, initialCharter);
  // The clock every group's offline age is measured against, STAMPED WHEN
  // THE ROSTER LANDS (below) rather than read during render — `Date.now()`
  // in a render body is impure. `null` until then, which is exactly when the
  // roster is still empty and nothing renders anyway.
  const [nowMs, setNowMs] = useState<number | null>(null);

  // Visible projects → picker options; default to the first one.
  useEffect(() => {
    let cancelled = false;
    fetchDashboard()
      .then((rows) => {
        if (cancelled) return;
        setProjects(rows);
        setSlug((current) => current ?? rows[0]?.slug ?? null);
      })
      .catch(() => {
        if (!cancelled) setProjects([]);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // The charter follows the picked project.
  useEffect(() => {
    if (!slug) return;
    let cancelled = false;
    dispatch({ kind: "reset" });
    getRouting(slug)
      .then((doc) => {
        if (!cancelled) dispatch({ kind: "loaded", doc });
      })
      .catch((err) => {
        if (!cancelled) {
          dispatch({
            kind: "load-failed",
            error: err instanceof Error ? err.message : String(err),
          });
        }
      });
    return () => {
      cancelled = true;
    };
  }, [slug]);

  // Host agent reports for the roster (health is the API's word, verbatim).
  useEffect(() => {
    let cancelled = false;
    getHosts()
      .then(async ({ hosts }) => {
        const detailed = await Promise.all(
          hosts.map(async (h) => {
            const status = h.status ?? "online";
            // The heartbeat second rides on the SUMMARY row (the detail
            // endpoint has no such field), so it is threaded from `h` on
            // both the detail-ok and detail-failed paths.
            const beat = h.last_heartbeat_unix;
            try {
              const d = await getHostDetail(h.host);
              return {
                host: h.host,
                hostname: d.hostname,
                status,
                agents: d.agents,
                last_heartbeat_unix: beat,
              };
            } catch {
              return {
                host: h.host,
                hostname: h.hostname,
                status,
                agents: [],
                last_heartbeat_unix: beat,
              };
            }
          }),
        );
        if (!cancelled) {
          setRoster(detailed);
          // Pair the clock with the heartbeats it will be compared against.
          setNowMs(Date.now());
          // Offline satellites start collapsed (local + online start open).
          setRosterCollapsed(
            new Set(
              detailed.filter((h) => h.host !== "local" && h.status !== "online").map((h) => h.host),
            ),
          );
        }
      })
      .catch(() => {
        if (!cancelled) setRoster([]);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const toggleRosterCollapsed = (host: string) => {
    setRosterCollapsed((previous) => {
      const next = new Set(previous);
      if (next.has(host)) next.delete(host);
      else next.add(host);
      return next;
    });
  };

  const onRosterRemoveClick = (host: string, online: boolean) => {
    handleRosterRemoveClick({
      host,
      online,
      confirmingHost,
      setConfirmingHost,
      setRoster,
      onError: (message) => toastBus.handler?.error(message),
    });
  };

  const onSave = () => {
    if (!slug || state.draft == null || state.saving) return;
    const draft = state.draft;
    dispatch({ kind: "save-begin" });
    putRouting(slug, draft)
      .then((result) => dispatch({ kind: "saved", result }))
      .catch((err) =>
        dispatch({
          kind: "save-failed",
          error: err instanceof Error ? err.message : String(err),
        }),
      );
  };

  return (
    <div className="agents-charter" data-testid="charter-panel">
      <VendorRosterCards
        hosts={roster}
        nodes={nodes}
        lang={lang}
        collapsed={rosterCollapsed}
        onToggleCollapse={toggleRosterCollapsed}
        confirmingHost={confirmingHost}
        onRemoveClick={onRosterRemoveClick}
        onVendorPick={onVendorPick}
        nowMs={nowMs ?? undefined}
      />

      <PlaybookCards lang={lang} />

      <section className="charter-editor-section">
        <div className="charter-editor-head">
          <h3>{t("charterTitle")}</h3>
          {projects && projects.length > 0 ? (
            <label className="charter-project-pick">
              {t("project")}
              <select
                data-testid="charter-project-select"
                value={slug ?? ""}
                onChange={(event) => setSlug(event.target.value)}
              >
                {projects.map((p) => (
                  <option key={p.slug} value={p.slug}>
                    {p.slug}
                  </option>
                ))}
              </select>
            </label>
          ) : null}
        </div>
        {projects && projects.length === 0 ? (
          <p className="charter-muted">{t("charterNoProjects")}</p>
        ) : (
          <CharterEditorView
            state={state}
            lang={lang}
            onStartDraft={(from) => dispatch({ kind: "start-draft", from })}
            onEdit={(content) => dispatch({ kind: "edit", content })}
            onTogglePreview={() => dispatch({ kind: "toggle-preview" })}
            onSave={onSave}
          />
        )}
        <p className="charter-note" data-testid="charter-honesty">
          {t("charterHonesty")}
        </p>
      </section>
    </div>
  );
}
