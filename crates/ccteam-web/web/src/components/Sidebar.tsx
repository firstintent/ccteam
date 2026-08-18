// v0.8.24 Track A — the prototype sidebar (ui-prototype.html `.side`):
// expanded 296px column / collapsed 64px icon rail / mobile fixed drawer.
//
// Expanded, top→bottom: logo(→home)+collapse → ⌘K search → 「新建会话」 → 「工作流」 →
// 「工作区」 groups (per-project sessions, 4 shown + 展开显示(还有 N 个),
// hover-stop per running row) → bottom user row + 「设置」(settings last).
//
// Collapsed rail keeps the SAME order (prototype CSS comment is the
// acceptance): logo(→home) → expand → search → new → flow →
// blank(click expands) → avatar → settings.

import { useEffect, useState } from "react";
import {
  Blocks,
  ChevronDown,
  ChevronRight,
  ChevronsLeft,
  ChevronsRight,
  Copy,
  Folder,
  FolderMinus,
  MoreHorizontal,
  Pencil,
  Plus,
  Search,
  Settings,
  Users,
  Workflow,
} from "lucide-react";
import { CcLogo } from "./Logo";
import { InlineRename } from "./InlineRename";
import { VendorChip } from "./VendorChip";
import { Dialog, Input } from "./ui";
import { copyText } from "../lib/clipboard";
import { toastBus } from "../lib/toastBus";
import { makeT, tShowMore, type Lang } from "../lib/i18n";

/** One sidebar session row — a live gateway session OR a stopped (history)
 *  session (dimmer; clicking resumes it). */
export interface RailRow {
  sid: string;
  project: string;
  label: string;
  vendor: string;
  model?: string;
  /** Live status vocabulary (`live`/`idle`/`working`/`stale`/`stuck`) —
   *  ignored for history rows. */
  status?: string | null;
  /** A stopped session (resume on click). */
  history?: boolean;
}

/** Default rows shown per workspace before 「展开显示」. */
export const WS_SHOW = 4;

/** Filter rows by the sidebar query (title + sid + model + project + vendor —
 *  mirrors the prototype's haystack). Pure for unit tests. */
// eslint-disable-next-line react-refresh/only-export-components -- pure helper co-located for unit tests.
export function filterRows(rows: RailRow[], query: string): RailRow[] {
  const q = query.trim().toLowerCase();
  if (!q) return rows;
  return rows.filter((r) =>
    `${r.label} ${r.sid} ${r.model ?? ""} ${r.project} ${r.vendor}`
      .toLowerCase()
      .includes(q),
  );
}

/** Group rows under every known project (a registered project with no rows
 *  still gets a group — the chicken-and-egg fix). Pure for unit tests. */
// eslint-disable-next-line react-refresh/only-export-components -- pure helper co-located for unit tests.
export function groupRows(
  projects: readonly string[],
  rows: RailRow[],
): { project: string; rows: RailRow[] }[] {
  const byProject = new Map<string, RailRow[]>();
  for (const p of projects) byProject.set(p, []);
  for (const r of rows) {
    const list = byProject.get(r.project);
    if (list) list.push(r);
    else byProject.set(r.project, [r]);
  }
  return Array.from(byProject.entries()).map(([project, list]) => ({ project, rows: list }));
}

/** A row is "running" (hover shows the stop affordance) when it is a LIVE
 *  session — stop keeps state + stays resumable, so it's offered for any
 *  non-history row. Pure for unit tests. */
// eslint-disable-next-line react-refresh/only-export-components -- pure helper co-located for unit tests.
export function rowStoppable(row: Pick<RailRow, "history">): boolean {
  return !row.history;
}

/** The 「从 ccteam 移除」 confirm dialog behind the workspace ⋯ menu.
 *  Type-the-slug gate for a destructive-looking action that is deliberately
 *  NOT destructive on disk (`DELETE /api/v1/projects/{slug}` = deregister +
 *  stop live sessions only) — the copy says exactly that. Controlled (state
 *  lives in {@link Sidebar}) so tests can render it open directly. */
export function RemoveProjectDialog({
  lang,
  project,
  path,
  typed,
  busy,
  onTyped,
  onCancel,
  onConfirm,
}: {
  lang: Lang;
  project: string;
  path?: string;
  typed: string;
  busy: boolean;
  onTyped: (value: string) => void;
  onCancel: () => void;
  onConfirm: () => void;
}) {
  const t = makeT(lang);
  return (
    <Dialog
      open
      onClose={onCancel}
      title={`${t("projectRemoveTitle")} · ${project}`}
      className="max-w-md"
    >
      <div className="flex flex-col gap-3" data-testid={`project-remove-dialog-${project}`}>
        <p className="text-xs leading-relaxed text-text-muted">{t("projectRemoveHint")}</p>
        {path ? (
          <code className="break-all font-mono text-[11px] text-text-dim">{path}</code>
        ) : null}
        <label className="text-xs text-text-secondary" htmlFor="project-remove-typed">
          {t("projectRemoveType")}
        </label>
        <Input
          id="project-remove-typed"
          data-testid="project-remove-typed"
          value={typed}
          spellCheck={false}
          autoComplete="off"
          placeholder={project}
          disabled={busy}
          onChange={(e) => onTyped(e.target.value)}
          className="font-mono"
        />
        <div style={{ display: "flex", gap: 8, justifyContent: "flex-end" }}>
          <button type="button" className="btn ghost mini" onClick={onCancel} disabled={busy}>
            {t("cancel")}
          </button>
          <button
            type="button"
            className="btn danger mini"
            data-testid="project-remove-go"
            disabled={busy || typed.trim() !== project}
            onClick={onConfirm}
          >
            {busy ? t("projectRemoveBusy") : t("projectRemoveConfirm")}
          </button>
        </div>
      </div>
    </Dialog>
  );
}

export function Sidebar({
  lang,
  collapsed,
  mobileOpen,
  activeSid,
  projects,
  projectHosts = {},
  rows,
  query,
  flowActive,
  settingsActive,
  teamActive = false,
  dshActive = false,
  userName,
  userInitial,
  avatarColor,
  searchRef,
  onQuery,
  onCollapse,
  onOpenHome,
  onNewSession,
  onNewInProject,
  onOpenFlow,
  onOpenSettings,
  onOpenTeam,
  onOpenDsh,
  onOpenRow,
  onStopRow,
  onRenameRow,
  onRemoveProject,
  projectPaths = {},
}: {
  lang: Lang;
  collapsed: boolean;
  mobileOpen: boolean;
  activeSid: string | null;
  projects: string[];
  projectHosts?: Record<string, { host: string; online: boolean }>;
  /** Real working-tree path per slug — the ⋯ menu's 复制路径 source. */
  projectPaths?: Record<string, string>;
  rows: RailRow[];
  query: string;
  flowActive: boolean;
  settingsActive: boolean;
  /** v0.9.0 W4 — whether the 团队/Team route is the active view. */
  teamActive?: boolean;
  /** v0.9.15 — whether the DSH embedded page is the active view. */
  dshActive?: boolean;
  userName: string;
  userInitial: string;
  avatarColor?: string;
  searchRef?: React.Ref<HTMLInputElement>;
  onQuery: (q: string) => void;
  onCollapse: (collapsed: boolean) => void;
  /** Logo click — navigate home (`/`). */
  onOpenHome: () => void;
  onNewSession: () => void;
  onNewInProject: (project: string) => void;
  onOpenFlow: () => void;
  onOpenSettings: () => void;
  onOpenTeam?: () => void;
  /** v0.9.15 — open the DSH embedded page. */
  onOpenDsh?: () => void;
  onOpenRow: (row: RailRow) => void;
  onStopRow: (row: RailRow) => void;
  /** Rename a row's session (live or stopped) — double-click a row, or use
   *  its ✎ button. Optional so embedding surfaces can omit the affordance. */
  onRenameRow?: (sid: string, title: string) => void;
  /** Remove a project FROM CCTEAM (deregister only — disk untouched). The
   *  caller owns the API call + toast + list refresh and resolves `true` on
   *  success (closes the dialog). Omitted ⇒ the ⋯ menu hides the entry. */
  onRemoveProject?: (slug: string) => Promise<boolean>;
}) {
  const t = makeT(lang);
  const [closedWs, setClosedWs] = useState<Record<string, boolean>>({});
  const [expandedWs, setExpandedWs] = useState<Record<string, boolean>>({});
  /** sid currently being renamed inline (at most one at a time). */
  const [renaming, setRenaming] = useState<string | null>(null);
  /** Project whose ⋯ menu is open (at most one), and the remove-confirm
   *  dialog's target + typed echo + in-flight flag. */
  const [menuFor, setMenuFor] = useState<string | null>(null);
  const [removeFor, setRemoveFor] = useState<string | null>(null);
  const [removeTyped, setRemoveTyped] = useState("");
  const [removeBusy, setRemoveBusy] = useState(false);

  // Any click outside the open ⋯ menu closes it (the menu's own buttons
  // stopPropagation, so choosing an item wins the race).
  useEffect(() => {
    if (menuFor === null) return;
    const close = () => setMenuFor(null);
    document.addEventListener("click", close);
    return () => document.removeEventListener("click", close);
  }, [menuFor]);

  const confirmRemove = async () => {
    if (!onRemoveProject || !removeFor || removeBusy) return;
    setRemoveBusy(true);
    try {
      if (await onRemoveProject(removeFor)) {
        setRemoveFor(null);
        setRemoveTyped("");
      }
    } finally {
      setRemoveBusy(false);
    }
  };

  const copyProjectPath = (project: string) => {
    const path = projectPaths[project];
    if (!path) return;
    void copyText(path).then((ok) =>
      ok ? toastBus.handler?.info(t("pathCopied")) : toastBus.handler?.error(t("copyFailed")),
    );
  };

  const q = query.trim();
  const filtered = filterRows(rows, q);
  const groups = groupRows(projects, filtered);

  const avatarStyle = avatarColor
    ? { background: avatarColor }
    : undefined;

  return (
    <aside
      className={`side ${collapsed ? "collapsed" : ""} ${mobileOpen ? "mobile-open" : ""}`}
      data-testid="sidebar"
    >
      <div className="side-inner">
        <div className="side-logo">
          <CcLogo className="logo-mark lg" onClick={onOpenHome} title={t("home")} testId="side-home" />
          <span className="brand">ccteam</span>
          <button
            type="button"
            className="icon-btn sm"
            onClick={() => onCollapse(true)}
            title={t("collapse")}
            aria-label={t("collapse")}
            data-testid="side-collapse"
          >
            <ChevronsLeft />
          </button>
        </div>

        <div className="side-top">
          <div className="search">
            <Search />
            <input
              id="side-search"
              ref={searchRef}
              value={query}
              onChange={(e) => onQuery(e.target.value)}
              placeholder={t("search")}
              spellCheck={false}
            />
            <span className="kbd">⌘K</span>
          </div>
        </div>

        <button type="button" className="snew" onClick={onNewSession} data-testid="side-new">
          <Plus />
          <span>{t("newSession")}</span>
        </button>

        <button
          type="button"
          className={`sflow ${flowActive ? "active" : ""}`}
          onClick={onOpenFlow}
          data-testid="side-flow"
        >
          <Workflow />
          <span>{t("workflow")}</span>
          <ChevronRight className="chev" />
        </button>

        <button
          type="button"
          className={`sflow ${teamActive ? "active" : ""}`}
          onClick={onOpenTeam}
          data-testid="side-team"
        >
          <Users />
          <span>{t("team")}</span>
          <ChevronRight className="chev" />
        </button>

        <button
          type="button"
          className={`sflow ${dshActive ? "active" : ""}`}
          onClick={onOpenDsh}
          data-testid="side-dsh"
        >
          <Blocks />
          <span>{t("dsh")}</span>
          <ChevronRight className="chev" />
        </button>

        <div className="side-sec">
          <span>{t("workspaces")}</span>
          <button
            type="button"
            className="icon-btn sm"
            onClick={onNewSession}
            title={t("newWorkspace")}
            aria-label={t("newWorkspace")}
          >
            <Plus />
          </button>
        </div>

        <div className="side-list" data-testid="side-list">
          {groups.map(({ project, rows: list }) => {
            const closed = closedWs[project] && !q;
            const projectHost = projectHosts[project] ?? { host: "local", online: true };
            const shown = expandedWs[project] || q ? list : list.slice(0, WS_SHOW);
            return (
              <div key={project}>
                <div
                  className="ws-head"
                  role="button"
                  tabIndex={0}
                  onClick={() => setClosedWs((s) => ({ ...s, [project]: !s[project] }))}
                  onKeyDown={(e) => {
                    if (e.key === "Enter") setClosedWs((s) => ({ ...s, [project]: !s[project] }));
                  }}
                >
                  <ChevronDown className={`chev ${closed ? "closed" : ""}`} />
                  <Folder className="fold" />
                  <span className="wname">{project}</span>
                  {projectHost.host !== "local" ? (
                    <span
                      className={`project-host ${projectHost.online ? "" : "offline"}`}
                      title={projectHost.online ? projectHost.host : `${projectHost.host} · ${t("offline")}`}
                    >
                      @ {projectHost.host}
                      {!projectHost.online ? ` · ${t("offline")}` : ""}
                    </span>
                  ) : null}
                  <button
                    type="button"
                    className="wplus"
                    title={t("newInWs")}
                    aria-label={t("newInWs")}
                    onClick={(e) => {
                      e.stopPropagation();
                      onNewInProject(project);
                    }}
                  >
                    ＋
                  </button>
                  <span className="ws-menu-wrap">
                    <button
                      type="button"
                      className={`wplus wmore ${menuFor === project ? "open" : ""}`}
                      title={t("wsMenu")}
                      aria-label={`${t("wsMenu")} ${project}`}
                      aria-expanded={menuFor === project}
                      data-testid={`ws-menu-btn-${project}`}
                      onClick={(e) => {
                        e.stopPropagation();
                        setMenuFor((cur) => (cur === project ? null : project));
                      }}
                    >
                      <MoreHorizontal />
                    </button>
                    {menuFor === project ? (
                      <div
                        className="ws-menu"
                        data-testid={`ws-menu-${project}`}
                        role="menu"
                        onClick={(e) => e.stopPropagation()}
                      >
                        <button
                          type="button"
                          role="menuitem"
                          onClick={() => {
                            setMenuFor(null);
                            onNewInProject(project);
                          }}
                        >
                          <Plus />
                          {t("newInWs")}
                        </button>
                        {projectPaths[project] ? (
                          <button
                            type="button"
                            role="menuitem"
                            data-testid={`ws-menu-copy-${project}`}
                            onClick={() => {
                              setMenuFor(null);
                              copyProjectPath(project);
                            }}
                          >
                            <Copy />
                            {t("wsMenuCopyPath")}
                          </button>
                        ) : null}
                        {onRemoveProject ? (
                          <button
                            type="button"
                            role="menuitem"
                            className="danger"
                            data-testid={`ws-menu-remove-${project}`}
                            onClick={() => {
                              setMenuFor(null);
                              setRemoveTyped("");
                              setRemoveFor(project);
                            }}
                          >
                            <FolderMinus />
                            {t("wsMenuRemove")}
                          </button>
                        ) : null}
                      </div>
                    ) : null}
                  </span>
                </div>
                {closed
                  ? null
                  : list.length === 0
                    ? (
                        <div className="srow empty">{q ? t("noMatch") : t("noSessions")}</div>
                      )
                    : (
                        <>
                          {shown.map((row) => (
                            <div
                              key={row.sid}
                              className={`srow ${row.sid === activeSid ? "active" : ""} ${row.history ? "hist" : ""}`}
                              role="button"
                              tabIndex={0}
                              title={`${row.sid} · ${row.vendor}${row.model ? ` ${row.model}` : ""}${row.history ? ` · ${t("historySec")}` : ""}${onRenameRow ? ` · ${t("renameTip")}` : ""}`}
                              onClick={() => {
                                if (renaming !== row.sid) onOpenRow(row);
                              }}
                              onDoubleClick={() => {
                                if (onRenameRow) setRenaming(row.sid);
                              }}
                              onKeyDown={(e) => {
                                if (e.key === "Enter" && renaming !== row.sid) onOpenRow(row);
                              }}
                            >
                              <VendorChip vendor={row.vendor} />
                              {renaming === row.sid && onRenameRow ? (
                                <InlineRename
                                  className="name rename-input"
                                  initial={row.label}
                                  ariaLabel={`${t("renameTip")} ${row.sid}`}
                                  onSubmit={(title) => {
                                    setRenaming(null);
                                    onRenameRow(row.sid, title);
                                  }}
                                  onCancel={() => setRenaming(null)}
                                />
                              ) : (
                                <span className="name">{row.label}</span>
                              )}
                              {onRenameRow && renaming !== row.sid ? (
                                <button
                                  type="button"
                                  className="stop rename"
                                  title={t("renameTip")}
                                  aria-label={`${t("renameTip")} ${row.sid}`}
                                  onClick={(e) => {
                                    e.stopPropagation();
                                    setRenaming(row.sid);
                                  }}
                                >
                                  <Pencil />
                                </button>
                              ) : null}
                              {rowStoppable(row) && renaming !== row.sid ? (
                                <button
                                  type="button"
                                  className="stop"
                                  title={t("stopTip")}
                                  aria-label={`${t("stopTip")} ${row.sid}`}
                                  onClick={(e) => {
                                    e.stopPropagation();
                                    onStopRow(row);
                                  }}
                                >
                                  <svg viewBox="0 0 24 24" fill="currentColor" aria-hidden>
                                    <rect x="6" y="6" width="12" height="12" rx="2" />
                                  </svg>
                                </button>
                              ) : null}
                            </div>
                          ))}
                          {list.length > shown.length ? (
                            <div
                              className="srow more"
                              role="button"
                              tabIndex={0}
                              onClick={() => setExpandedWs((s) => ({ ...s, [project]: true }))}
                              onKeyDown={(e) => {
                                if (e.key === "Enter")
                                  setExpandedWs((s) => ({ ...s, [project]: true }));
                              }}
                            >
                              {tShowMore(lang, list.length - shown.length)}
                            </div>
                          ) : null}
                        </>
                      )}
              </div>
            );
          })}
          {groups.length === 0 ? (
            <div className="srow empty">{q ? t("noMatch") : t("noSessions")}</div>
          ) : null}
        </div>

        <div className="side-bottom">
          <div className="side-user">
            <span className="avatar" style={{ width: 26, height: 26, fontSize: 11, ...avatarStyle }}>
              {userInitial}
            </span>
            {userName}
          </div>
          <button
            type="button"
            className={`sflow ${settingsActive ? "active" : ""}`}
            onClick={onOpenSettings}
            data-testid="side-settings"
          >
            <Settings />
            <span>{t("settings")}</span>
            <ChevronRight className="chev" />
          </button>
        </div>
      </div>

      {/* 折叠态(顺序与展开一致:logo→展开→搜索→新建→工作流→空白→头像→设置) */}
      <div className="side-mini" data-testid="side-mini">
        <CcLogo className="logo-mark" onClick={onOpenHome} title={t("home")} testId="side-home-rail" />
        <button
          type="button"
          className="rail-btn"
          onClick={() => onCollapse(false)}
          title={t("expand")}
          aria-label={t("expand")}
          data-testid="side-expand"
        >
          <ChevronsRight />
        </button>
        <button
          type="button"
          className="rail-btn"
          onClick={() => {
            onCollapse(false);
            window.setTimeout(() => {
              const el = document.getElementById("side-search") as HTMLInputElement | null;
              el?.focus();
            }, 220);
          }}
          title={t("search")}
          aria-label={t("search")}
        >
          <Search />
        </button>
        <button
          type="button"
          className="rail-btn"
          onClick={onNewSession}
          title={t("newSession")}
          aria-label={t("newSession")}
        >
          <Plus />
        </button>
        <button
          type="button"
          className="rail-btn"
          onClick={onOpenFlow}
          title={t("workflow")}
          aria-label={t("workflow")}
        >
          <Workflow />
        </button>
        <button
          type="button"
          className="rail-btn"
          onClick={onOpenTeam}
          title={t("team")}
          aria-label={t("team")}
          data-testid="side-team-rail"
        >
          <Users />
        </button>
        <button
          type="button"
          className="rail-btn"
          onClick={onOpenDsh}
          title={t("dsh")}
          aria-label={t("dsh")}
          data-testid="side-dsh-rail"
        >
          <Blocks />
        </button>
        {/* 会话区空白:点击展开 */}
        <div
          className="mini-blank"
          title={t("expand")}
          role="button"
          tabIndex={0}
          onClick={() => onCollapse(false)}
          onKeyDown={(e) => {
            if (e.key === "Enter") onCollapse(false);
          }}
        />
        <div className="avatar" title={userName} style={avatarStyle}>
          {userInitial}
        </div>
        <button
          type="button"
          className="rail-btn"
          onClick={onOpenSettings}
          title={t("settings")}
          aria-label={t("settings")}
          data-testid="side-settings-rail"
        >
          <Settings />
        </button>
      </div>

      {removeFor && onRemoveProject ? (
        <RemoveProjectDialog
          lang={lang}
          project={removeFor}
          path={projectPaths[removeFor]}
          typed={removeTyped}
          busy={removeBusy}
          onTyped={setRemoveTyped}
          onCancel={() => {
            if (!removeBusy) {
              setRemoveFor(null);
              setRemoveTyped("");
            }
          }}
          onConfirm={() => void confirmRemove()}
        />
      ) : null}
    </aside>
  );
}
