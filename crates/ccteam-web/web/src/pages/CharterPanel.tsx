// v0.9.11 TEAM-2 — 分工 charter tab body (Team page). Four blocks:
//
// - Vendor roster: one card per (host, vendor) from the hosts agent report —
//   installed/version/status straight off the API (never invented), plus
//   live-session count + Σcost aggregated from the SAME graph nodes the
//   topology tab already fetched (passed down as a prop, no refetch).
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
// the hook-free views below are exported for the SSR test suite.

import { useEffect, useReducer, useState } from "react";
import { Link } from "react-router-dom";
import type { AgentNode } from "../lib/agentsApi";
import { charterReducer, initialCharter, type CharterState } from "../lib/charterState";
import { fetchDashboard, type DashboardRow } from "../lib/dashboardApi";
import { getHostDetail, getHosts, type AgentHealth } from "../lib/hostsApi";
import { getRouting, putRouting } from "../lib/routingApi";
import { makeT, type Lang } from "../lib/i18n";
import { PLAYBOOKS } from "../lib/playbooks";
import { VendorChip } from "../components/VendorChip";
import { Markdown } from "../components/Markdown";

/** One host's agent report, resolved for the roster. */
export interface RosterHost {
  host: string;
  hostname: string;
  agents: AgentHealth[];
}

/** Status → badge class/label. Renders EXACTLY what the API reports — an
 *  unknown status falls through verbatim (honesty over prettiness). */
function rosterBadge(status: string, t: (key: string) => string): { cls: string; label: string } {
  if (status === "ready") return { cls: "badge ok", label: t("rosterStatusReady") };
  if (status === "needs_config") return { cls: "badge warn", label: t("rosterStatusNeedsConfig") };
  if (status === "not_installed") return { cls: "badge", label: t("notInstalled") };
  return { cls: "badge", label: status };
}

/** Vendor roster cards — hook-free presentational (exported for node-env
 *  tests). `nodes` = the topology tab's graph nodes (prop-drilled, not
 *  refetched); live/Σcost aggregate over (host, vendor). */
export function VendorRosterCards({
  hosts,
  nodes,
  lang: langProp,
}: {
  hosts: RosterHost[];
  nodes: AgentNode[];
  lang?: Lang;
}) {
  const t = makeT(langProp ?? "zh");
  if (hosts.length === 0) return null;
  const showHost = hosts.length > 1;
  return (
    <section className="charter-roster-section">
      <h3>{t("charterRoster")}</h3>
      <div className="charter-roster" data-testid="charter-roster">
        {hosts.flatMap(({ host, hostname, agents }) =>
          agents.map((agent) => {
            const mine = nodes.filter((n) => n.host === host && n.vendor === agent.vendor);
            const live = mine.filter((n) => n.status === "live").length;
            const cost = mine.reduce((sum, n) => sum + (n.cost_usd ?? 0), 0);
            const badge = rosterBadge(agent.status, t);
            return (
              <div
                key={`${host}-${agent.vendor}`}
                className="charter-roster-card"
                data-testid={`charter-roster-card-${host}-${agent.vendor}`}
              >
                <div className="charter-roster-head">
                  <VendorChip vendor={agent.vendor} />
                  <span className="charter-roster-vendor">{agent.vendor}</span>
                  {showHost ? <span className="charter-roster-host mono">{hostname || host}</span> : null}
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
          }),
        )}
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
 *  graph (single source for the roster aggregation). */
export default function CharterPanel({
  nodes,
  lang: langProp,
}: {
  nodes: AgentNode[];
  lang?: Lang;
}) {
  const lang = langProp ?? "zh";
  const t = makeT(lang);
  const [projects, setProjects] = useState<DashboardRow[] | null>(null);
  const [slug, setSlug] = useState<string | null>(null);
  const [roster, setRoster] = useState<RosterHost[]>([]);
  const [state, dispatch] = useReducer(charterReducer, initialCharter);

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
            try {
              const d = await getHostDetail(h.host);
              return { host: h.host, hostname: d.hostname, agents: d.agents };
            } catch {
              return { host: h.host, hostname: h.hostname, agents: [] };
            }
          }),
        );
        if (!cancelled) setRoster(detailed);
      })
      .catch(() => {
        if (!cancelled) setRoster([]);
      });
    return () => {
      cancelled = true;
    };
  }, []);

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
      <VendorRosterCards hosts={roster} nodes={nodes} lang={lang} />

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
