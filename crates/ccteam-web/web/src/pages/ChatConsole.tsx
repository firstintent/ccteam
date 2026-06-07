// v0.8.7 W4 (DD.1) — per-session web chat console.
//
// Rewired from the v0.8.3 single global WS + flat session-mixing
// transcript into a PER-SESSION console keyed by the gateway `s{n}` id:
//
//   - ROUTE  `/chat/s/:sid` (nested in App.tsx) — `useParams` gives the
//            one session this view drives; `/chat` (no sid) is the
//            switcher with nothing selected.
//   - DATA   drives `/api/v1` exclusively (sessionsApi): listSessions for
//            the switcher, getHistory to seed a reopened page, submitTurn
//            to send, stopSession to stop, createSession for new sessions.
//   - STREAM live events via `useSessionEvents(sid)` (the W2-tagged
//            per-sid SSE at `/api/v1/sessions/{sid}/events`).
//   - STORE  each sid owns its OWN transcript (per-sid localStorage key via
//            `chatTranscript`), so switching sessions NEVER mixes streams.
//   - APPROVE W2 ChoicePrompt events (tagged with sid + options + token)
//            render as "session sX wants to run … [option chips]"; clicking
//            POSTs {token, selection=id} to /resolve (R-H1) — the SAME
//            gateway pending machinery an IM click uses, NOT a turn — so the
//            blocked tool actually runs on Approve / denies on Deny.
//
// Red lines: reads structured turn/SSE frames (never scrapes a pane); the
// terminal view is the existing `ccteam-pty.v1` byte relay; the new-session
// default role stays `cto` (chatDefaults.DEFAULT_ROLE, FIX-2).
//
// LEGACY (unrepointed): SessionsListPage / SessionDetail + `/sessions/active`
// remain the operator/bg view in the `claude-N`/`codex-N` namespace — this
// per-session chat is the additive `s{n}` surface.

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { Link, useNavigate, useParams } from "react-router-dom";
import { MessageSquare, Plus, Send, Square, Terminal } from "lucide-react";
import { TerminalView } from "../components/TerminalView";
import { useSessionEvents } from "../hooks/useSessionEvents";
import { createProject as apiCreateProject, fetchDashboard } from "../lib/dashboardApi";
import {
  createSession as apiCreateSession,
  getHistory,
  listProjectRoles,
  listSessions,
  resolveApproval as apiResolveApproval,
  stopSession as apiStopSession,
  submitTurn,
  type RoleSummary,
  type SessionView,
} from "../lib/sessionsApi";
import { toastBus } from "../lib/toastBus";
import { DEFAULT_ROLE, ROLE_SUGGESTIONS, ROLELESS, resolveRole } from "./chatDefaults";
import {
  appendRow,
  eventToRow,
  historyToRows,
  loadRows,
  nextRowId,
  saveRows,
  type TranscriptRow,
} from "./chatTranscript";

/** A switcher entry — one live gateway session, grouped under its project. */
type RailSession = SessionView;

export default function ChatConsole() {
  const { sid: routeSid } = useParams<{ sid: string }>();
  const sid = routeSid ?? null;
  const navigate = useNavigate();

  // The switcher's session list (gateway `s{n}`), fanned out across every
  // project from /api/v1/projects → /api/v1/projects/{slug}/sessions.
  const [railSessions, setRailSessions] = useState<RailSession[]>([]);
  const [railError, setRailError] = useState<string | null>(null);
  const [draft, setDraft] = useState("");
  const [view, setView] = useState<"chat" | "terminal">("chat");
  const [modalOpen, setModalOpen] = useState(false);

  // Per-sid transcript. Seeded from localStorage on sid change, then from
  // mirrored history, then live-appended from SSE. Keying on sid is THE
  // fix: two sessions never share one buffer.
  const [rows, setRows] = useState<TranscriptRow[]>([]);

  const { events, connected, lastError, gatewayUnavailable } = useSessionEvents(sid);

  // The active session's view (vendor/role/project) for the crumb + terminal
  // gating — read from the rail list.
  const activeView = useMemo(
    () => railSessions.find((s) => s.sid === sid) ?? null,
    [railSessions, sid],
  );

  // ---- switcher list (cross-project fan-out) -----------------------------
  const refreshSessions = useCallback(async () => {
    try {
      const projects = await fetchDashboard();
      const lists = await Promise.all(
        projects.map((p) =>
          listSessions(p.slug).catch(() => [] as SessionView[]),
        ),
      );
      setRailSessions(lists.flat());
      setRailError(null);
    } catch (e) {
      if (e instanceof Error && e.message === "UNAUTHENTICATED") {
        // global TokenEntryGate handles re-auth; don't double-report.
        return;
      }
      setRailError(e instanceof Error ? e.message : "failed to load sessions");
    }
  }, []);

  useEffect(() => {
    void refreshSessions();
  }, [refreshSessions]);

  // v0.8.8 bug5 — pick up projects/sessions registered out-of-band (a CLI
  // `ccteam init` while this tab is open) without a manual page refresh. The
  // backend list (GET /api/v1/projects → collect_projects) reads config.yaml
  // live, so a refetch when the tab regains focus surfaces them; the
  // new-session modal also refetches on open (see the ＋ button) for the
  // create-a-session flow.
  useEffect(() => {
    const onFocus = () => {
      void refreshSessions();
    };
    window.addEventListener("focus", onFocus);
    return () => window.removeEventListener("focus", onFocus);
  }, [refreshSessions]);

  // ---- per-sid transcript: seed on sid change ----------------------------
  useEffect(() => {
    if (!sid) {
      setRows([]);
      return;
    }
    // 1) instant: persisted rows for this sid (refresh/reopen continuity).
    setRows(loadRows(sid));
    // 2) authoritative: mirrored history seeds (or replaces an empty) buffer.
    let cancelled = false;
    getHistory(sid)
      .then((h) => {
        if (cancelled) return;
        const seeded = historyToRows(h.events);
        if (seeded.length > 0) setRows(seeded);
      })
      .catch(() => {
        // best-effort — keep the localStorage rows (or empty) on error.
      });
    return () => {
      cancelled = true;
    };
  }, [sid]);

  // ---- live SSE → append into the active sid's transcript ----------------
  // We track how many events we've folded so a re-render doesn't re-append.
  const foldedRef = useRef(0);
  useEffect(() => {
    foldedRef.current = 0;
  }, [sid]);
  useEffect(() => {
    if (!sid) return;
    if (events.length <= foldedRef.current) return;
    const fresh = events.slice(foldedRef.current);
    foldedRef.current = events.length;
    setRows((current) => {
      let next = current;
      for (const ev of fresh) {
        const row = eventToRow(ev);
        if (row) next = appendRow(next, row);
      }
      return next;
    });
  }, [events, sid]);

  // ---- persist the active sid's transcript -------------------------------
  useEffect(() => {
    if (sid) saveRows(sid, rows);
  }, [sid, rows]);

  const pushRow = useCallback((row: Omit<TranscriptRow, "id">) => {
    setRows((current) => appendRow(current, { ...row, id: nextRowId(row.kind) }));
  }, []);

  // ---- send a turn -------------------------------------------------------
  const submit = useCallback(() => {
    const content = draft.trim();
    if (!content || !sid) return;
    pushRow({ kind: "user", content });
    setDraft("");
    submitTurn(sid, content).catch((e) => {
      pushRow({
        kind: "system",
        content: `发送失败: ${e instanceof Error ? e.message : "unknown"}`,
      });
    });
  }, [draft, sid, pushRow]);

  // ---- resolve a W2 approval prompt (R-H1) -------------------------------
  // The per-sid SSE tags the ChoicePrompt with sid + each option's {label,id}
  // + the pending-resolution token. Clicking POSTs {token, selection=id} to
  // `/resolve`, which routes through the gateway's SAME pending machinery an
  // IM click uses (take_by_token → apply_pending) — NOT a turn. So [Approve]
  // makes the blocked tool actually run and [Deny] denies immediately (no
  // 600s timeout). A row with no token (or a chosen option with no id) can't
  // be resolved this way; we surface that rather than misfire a fake turn.
  const resolveApproval = useCallback(
    (row: TranscriptRow, optionIndex: number) => {
      if (!sid) return;
      const option = row.options?.[optionIndex];
      if (!row.token || !option?.id) {
        pushRow({
          kind: "system",
          content: "无法批准: 该提示缺少 token/选项 id(请在 IM 批准,或重开会话)",
        });
        return;
      }
      // mark the row resolved so the chips disable.
      setRows((current) =>
        current.map((r) => (r.id === row.id ? { ...r, resolved: true } : r)),
      );
      pushRow({ kind: "user", content: `→ ${option.label}` });
      apiResolveApproval(sid, row.token, option.id).catch((e) => {
        // Re-enable the chips so the user can retry on a transient failure.
        setRows((current) =>
          current.map((r) => (r.id === row.id ? { ...r, resolved: false } : r)),
        );
        pushRow({
          kind: "system",
          content: `批准提交失败: ${e instanceof Error ? e.message : "unknown"}`,
        });
      });
    },
    [sid, pushRow],
  );

  // ---- stop the session --------------------------------------------------
  const stopActive = useCallback(() => {
    if (!sid) return;
    apiStopSession(sid)
      .then(() => {
        pushRow({ kind: "system", content: "会话已停止" });
        void refreshSessions();
      })
      .catch((e) => {
        pushRow({
          kind: "system",
          content: `停止失败: ${e instanceof Error ? e.message : "unknown"}`,
        });
      });
  }, [sid, pushRow, refreshSessions]);

  // ---- create a new session (optionally a brand-new project first) -------
  // `newProjectPath` present ⇒ B2: POST /projects to scaffold+register `slug`
  // first, then feed the returned slug into the existing create-session flow.
  // Returns `true` on success so the modal can close itself; on failure it
  // resolves `false` so the modal stays open (input preserved) and the error
  // is surfaced via toast (human-readable) — NOT pushed into the transcript.
  const createSession = useCallback(
    async (
      slug: string,
      role: string,
      vendor: string,
      permissionMode: "skip" | "hitl",
      newProjectPath?: string,
    ): Promise<boolean> => {
      try {
        // B2: create the project first when a path was supplied.
        let targetSlug = slug;
        if (newProjectPath !== undefined) {
          const created = await apiCreateProject(slug, newProjectPath);
          targetSlug = created.slug;
        }
        const { sid: newSid } = await apiCreateSession(targetSlug, {
          role,
          vendor,
          permission_mode: permissionMode,
        });
        await refreshSessions();
        navigate(`/chat/s/${encodeURIComponent(newSid)}`);
        return true;
      } catch (e) {
        if (e instanceof Error && e.message === "UNAUTHENTICATED") {
          // global TokenEntryGate handles re-auth; don't toast/transcript.
          return false;
        }
        // Surface as a human-readable toast (e.g. "项目 demo 已存在"); never
        // leak a raw HTTP/stack into the transcript stream.
        const detail = e instanceof Error ? e.message : "unknown";
        toastBus.handler?.error(
          newProjectPath !== undefined ? `新建项目失败: ${detail}` : `新建 session 失败: ${detail}`,
        );
        return false;
      }
    },
    [refreshSessions, navigate],
  );

  const projects = useMemo(
    () => Array.from(new Set(railSessions.map((s) => s.project))).sort(),
    [railSessions],
  );
  const roleOptions = useMemo(() => {
    const seen = new Set(ROLE_SUGGESTIONS);
    railSessions.forEach((s) => s.role && seen.add(s.role));
    return Array.from(seen);
  }, [railSessions]);

  const switchTo = useCallback(
    (s: RailSession) => {
      navigate(`/chat/s/${encodeURIComponent(s.sid)}`);
      setView("chat");
    },
    [navigate],
  );

  // v0.8.8 B5 — claude-only for now. The PTY backend now resolves per-sid panes
  // for BOTH vendors (codex pane name = ccteam-{slug}-{sid}, wired in
  // routes/session_pane.rs), so the codex terminal is backend-ready. But it is
  // unverified on a real codex pane (the daemon currently wires the codex
  // app-server adapter, which owns no tmux/rmux pane), so we don't promise a
  // codex terminal in the UI yet. TODO: flip to allow codex once the codex
  // per-session pane is dogfooded on a real box.
  const canTerminal = activeView?.vendor === "claude" && !!sid;
  const showTerminal = view === "terminal" && canTerminal;

  // Auto-scroll to the newest message only when already near the bottom.
  const scrollRef = useRef<HTMLDivElement>(null);
  const atBottomRef = useRef(true);
  const onTranscriptScroll = useCallback(() => {
    const el = scrollRef.current;
    if (el) atBottomRef.current = el.scrollHeight - el.scrollTop - el.clientHeight < 120;
  }, []);
  useLayoutEffect(() => {
    if (!showTerminal && atBottomRef.current && scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [rows, showTerminal]);

  const statusText = gatewayUnavailable
    ? "无可用 gateway"
    : connected
      ? "已连接"
      : lastError
        ? "连接失败"
        : sid
          ? "连接中…"
          : "未选择 session";

  return (
    <div className="h-full min-h-0 flex flex-col bg-surface-900 text-text-primary">
      {/* standalone app bar */}
      <header className="h-12 shrink-0 border-b border-surface-700/40 px-4 flex items-center gap-3">
        <MessageSquare className="h-4 w-4 text-amber-400 shrink-0" />
        <span className="text-sm font-semibold">
          ccteam <span className="text-amber-400">chat</span>
        </span>
        <span className="hidden sm:inline text-[11px] font-mono text-text-dim px-1.5 py-0.5 rounded bg-surface-800">
          per-session · /api/v1
        </span>
        <span className="flex items-center gap-1.5 text-xs text-text-dim">
          <span
            className={`h-2 w-2 rounded-full ${
              connected ? "bg-green-400" : gatewayUnavailable || lastError ? "bg-red-500" : "bg-amber-500"
            }`}
          />
          {statusText}
        </span>
        <span className="flex-1" />
        <Link to="/" className="text-xs text-text-dim hover:text-text-primary transition-colors">
          Dashboard ↗
        </Link>
      </header>

      <div className="flex flex-1 min-h-0">
        {/* left rail — every project's gateway sessions, grouped */}
        <aside className="w-60 shrink-0 border-r border-surface-700/40 flex flex-col">
          <div className="h-10 shrink-0 px-3 flex items-center justify-between border-b border-surface-700/30">
            <span className="text-xs font-mono uppercase text-text-dim">所有 session</span>
            <button
              type="button"
              onClick={() => {
                // bug5 — refetch so a project created out-of-band (CLI
                // `ccteam init`) is in the list when the modal opens.
                void refreshSessions();
                setModalOpen(true);
              }}
              className="h-6 px-2 rounded-md bg-amber-500/90 text-surface-950 hover:bg-amber-400 text-xs flex items-center gap-1"
              title="新建 session"
            >
              <Plus className="h-3.5 w-3.5" /> 新建
            </button>
          </div>
          <div className="flex-1 overflow-y-auto p-2 space-y-2">
            {projects.map((project) => {
              const items = railSessions.filter((s) => s.project === project);
              return (
                <div key={project}>
                  <div className="px-1.5 py-1 text-[11px] font-mono text-text-dim">{project}</div>
                  <div className="space-y-0.5">
                    {items.map((s) => {
                      const active = s.sid === sid;
                      const isClaude = s.vendor === "claude";
                      return (
                        <button
                          key={s.sid}
                          type="button"
                          onClick={() => switchTo(s)}
                          className={`w-full text-left px-2 py-1.5 rounded-md flex items-center gap-2 text-xs ${
                            active
                              ? "bg-surface-700 text-text-primary"
                              : "text-text-secondary hover:bg-surface-800/70"
                          }`}
                        >
                          <span
                            className={`h-1.5 w-1.5 rounded-full shrink-0 ${
                              active ? "bg-green-400" : "bg-surface-500"
                            }`}
                          />
                          <span
                            className={`font-mono px-1 rounded text-[10px] ${
                              isClaude ? "bg-amber-500/15 text-amber-300" : "bg-sky-500/15 text-sky-300"
                            }`}
                          >
                            {s.vendor}
                          </span>
                          <span className="truncate flex-1">{s.role}</span>
                          {s.permission_mode === "hitl" ? (
                            <span
                              className="font-mono text-[9px] text-amber-300/90"
                              title="HITL: 非 allowlist 工具需批准"
                            >
                              hitl
                            </span>
                          ) : null}
                          <span className="text-text-dim font-mono">{s.sid}</span>
                        </button>
                      );
                    })}
                    {items.length === 0 ? (
                      <div className="px-2 py-1 text-[10px] text-text-dim/60">无 session</div>
                    ) : null}
                  </div>
                </div>
              );
            })}
            {projects.length === 0 ? (
              <div className="px-2 py-3 text-xs text-text-dim leading-5">
                {railError ? `加载失败: ${railError}` : "还没有 session。点「＋ 新建」创建。"}
              </div>
            ) : null}
          </div>
        </aside>

        {/* main: crumb + view toggle + transcript/terminal + composer */}
        <main className="flex-1 min-w-0 min-h-0 flex flex-col">
          <div className="h-10 shrink-0 px-4 flex items-center gap-3 border-b border-surface-700/30">
            <span className="text-xs text-text-dim shrink-0">会话 →</span>
            {activeView ? (
              <span className="flex items-center gap-2 text-xs min-w-0">
                <span className="text-green-400">●</span>
                <span className="font-semibold truncate">{activeView.project}</span>
                <span className="text-text-dim">/</span>
                <span className={activeView.vendor === "claude" ? "text-amber-300" : "text-sky-300"}>
                  {[activeView.vendor, activeView.role].filter(Boolean).join(" · ")}
                </span>
                <span className="font-mono text-text-dim">{activeView.sid}</span>
              </span>
            ) : sid ? (
              <span className="text-xs text-text-dim font-mono">{sid}</span>
            ) : (
              <span className="text-xs text-text-dim">从左侧选一个 session</span>
            )}
            <span className="flex-1" />
            {sid ? (
              <button
                type="button"
                onClick={stopActive}
                title="停止会话"
                className="h-7 px-2 rounded text-xs flex items-center gap-1 text-text-dim hover:text-red-300 hover:bg-surface-800"
              >
                <Square className="h-3.5 w-3.5" /> 停止
              </button>
            ) : null}
            <div className="flex items-center gap-1 rounded-md bg-surface-800 p-0.5">
              <button
                type="button"
                onClick={() => setView("chat")}
                className={`h-7 px-2 rounded text-xs flex items-center gap-1 ${
                  !showTerminal ? "bg-surface-700 text-text-primary" : "text-text-dim"
                }`}
              >
                <MessageSquare className="h-3.5 w-3.5" /> Chat
              </button>
              <button
                type="button"
                disabled={!canTerminal}
                onClick={() => canTerminal && setView("terminal")}
                title={canTerminal ? "终端(tmux pane)" : "仅 Claude/tmux session 有终端"}
                className={`h-7 px-2 rounded text-xs flex items-center gap-1 ${
                  showTerminal ? "bg-surface-700 text-text-primary" : "text-text-dim"
                } ${canTerminal ? "" : "opacity-40 cursor-not-allowed"}`}
              >
                <Terminal className="h-3.5 w-3.5" /> 终端
              </button>
            </div>
          </div>

          {showTerminal && activeView?.project && sid ? (
            <TerminalView slug={activeView.project} sid={sid} className="flex-1 min-h-0" />
          ) : (
            <>
              <div
                ref={scrollRef}
                onScroll={onTranscriptScroll}
                className="flex-1 min-h-0 overflow-y-auto p-4 space-y-3"
              >
                {!sid ? (
                  <div className="h-full grid place-items-center text-xs text-text-dim">
                    选一个 session 或点「＋ 新建」开始。
                  </div>
                ) : (
                  rows.map((row) => {
                    if (row.kind === "approval") {
                      return (
                        <div
                          key={row.id}
                          className="max-w-[760px] rounded-md px-3 py-2.5 text-sm bg-amber-500/10 border border-amber-500/30"
                        >
                          <div className="text-amber-200 mb-2">{row.content}</div>
                          <div className="flex flex-wrap gap-2">
                            {(row.options ?? []).map((opt, i) => (
                              <button
                                key={`${row.id}-${i}`}
                                type="button"
                                disabled={row.resolved}
                                onClick={() => resolveApproval(row, i)}
                                className="h-7 px-3 rounded-md text-xs bg-amber-500 text-surface-950 hover:bg-amber-400 disabled:opacity-40"
                              >
                                {opt.label}
                              </button>
                            ))}
                          </div>
                          {row.resolved ? (
                            <div className="mt-1.5 text-[10px] text-text-dim">已回应</div>
                          ) : null}
                        </div>
                      );
                    }
                    if (row.kind === "system") {
                      return (
                        <div key={row.id} className="text-center text-[11px] text-text-dim">
                          {row.content}
                        </div>
                      );
                    }
                    return (
                      <div
                        key={row.id}
                        className={`max-w-[760px] rounded-md px-3 py-2 text-sm leading-6 whitespace-pre-wrap break-words ${
                          row.kind === "user"
                            ? "ml-auto bg-amber-500/15 border border-amber-500/20"
                            : row.kind === "tool"
                              ? "bg-surface-800/70 border border-surface-700/50 text-text-secondary font-mono text-xs"
                              : "bg-surface-800 border border-surface-700/40"
                        }`}
                      >
                        {row.content}
                      </div>
                    );
                  })
                )}
              </div>
              <div className="border-t border-surface-700/40 p-3">
                <div className="flex gap-2">
                  <textarea
                    value={draft}
                    onChange={(event) => setDraft(event.target.value)}
                    onKeyDown={(event) => {
                      if (event.key === "Enter" && !event.shiftKey) {
                        event.preventDefault();
                        submit();
                      }
                    }}
                    disabled={!sid}
                    className="min-h-11 max-h-32 flex-1 resize-y rounded-md bg-surface-800 border border-surface-700 px-3 py-2 text-sm outline-none focus:border-amber-500 disabled:opacity-40"
                    placeholder={sid ? "发消息 / 命令(/compact /clear …)…" : "先选一个 session"}
                  />
                  <button
                    type="button"
                    onClick={submit}
                    disabled={!sid}
                    className="h-11 w-11 shrink-0 rounded-md bg-amber-500 text-surface-950 hover:bg-amber-400 disabled:opacity-40 grid place-items-center"
                    title={sid ? "发送" : "未选择 session"}
                  >
                    <Send className="h-4 w-4" />
                  </button>
                </div>
              </div>
            </>
          )}
        </main>
      </div>

      {modalOpen ? (
        <NewSessionModal
          projects={projects}
          fallbackRoles={roleOptions}
          defaultProject={activeView?.project ?? projects[0] ?? ""}
          onCancel={() => setModalOpen(false)}
          onCreate={createSession}
        />
      ) : null}
    </div>
  );
}

// New-session modal: pick a project (an existing one, OR scaffold a brand-new
// one inline — B2), code agent, role, and permission mode.
//   - B2: selecting the "＋ 新建项目…" sentinel reveals slug(name)+path fields;
//     submit first POSTs /api/v1/projects (createProject) then feeds the
//     returned slug into the existing create-session flow. Slug/path are
//     validated client-side mirroring the backend (validate_slug_format +
//     expand_project_path) so the user gets inline feedback before the round
//     trip.
//   - B3 / BUG-4: the role field is now a real dropdown sourced from
//     `GET /api/v1/projects/{slug}/roles` (the project's `.claude/agents/`)
//     for an EXISTING project, with the static ROLE_SUGGESTIONS/DEFAULT_ROLE
//     as the fallback/seed (FIX-2). A brand-new project has no roles yet, so
//     that branch uses the static fallback and does NOT fetch (would 404).
//
//   - F2-web: the role dropdown now offers an explicit "(无角色 / 裸 claude)"
//     choice (the ROLELESS sentinel). Picking it sends an empty role (a
//     bare-claude session that self-reads the project CLAUDE.md);
//     `resolveRole` (chatDefaults) maps the sentinel → "" while an
//     un-touched modal still falls back to DEFAULT_ROLE (cto), never roleless.

/** Sentinel project value selecting the "create a new project" branch. */
const NEW_PROJECT = "__new";

/** Mirror of the backend slug grammar (`ccteam_core::validate_slug_format`):
 *  `[a-z0-9-]+`, ≤60, no leading/trailing `-`. */
function slugError(slug: string): string | null {
  if (slug.length === 0) return "项目名不能为空";
  if (slug.length > 60) return "项目名最多 60 字符";
  if (!/^[a-z0-9-]+$/.test(slug)) return "只允许小写字母、数字、连字符";
  if (slug.startsWith("-") || slug.endsWith("-")) return "不能以连字符开头或结尾";
  return null;
}

/** Mirror of the backend path rule (`expand_project_path`): non-empty after
 *  trim and starting with `/` or `~`. */
function pathError(path: string): string | null {
  const p = path.trim();
  if (p.length === 0) return "路径不能为空";
  if (!p.startsWith("/") && !p.startsWith("~")) return "路径需以 / 或 ~ 开头";
  return null;
}

type RoleFetchState =
  | { kind: "idle" }
  | { kind: "loading" }
  | { kind: "ready"; roles: RoleSummary[] }
  | { kind: "error" };

function NewSessionModal({
  projects,
  fallbackRoles,
  defaultProject,
  onCancel,
  onCreate,
}: {
  projects: string[];
  /** Static role hints (ROLE_SUGGESTIONS ∪ live session roles) — the seed/
   *  fallback when a project's real roles can't be / aren't fetched. */
  fallbackRoles: string[];
  defaultProject: string;
  onCancel: () => void;
  onCreate: (
    slug: string,
    role: string,
    vendor: string,
    permissionMode: "skip" | "hitl",
    newProjectPath?: string,
  ) => Promise<boolean>;
}) {
  const [project, setProject] = useState(defaultProject);
  const [newSlug, setNewSlug] = useState("");
  const [newPath, setNewPath] = useState("");
  const [vendor, setVendor] = useState<"claude" | "codex">("claude");
  const [role, setRole] = useState("");
  const [hitl, setHitl] = useState(false);
  const [pending, setPending] = useState(false);
  const [roleState, setRoleState] = useState<RoleFetchState>({ kind: "idle" });

  const isNew = project === NEW_PROJECT;

  // ---- B3: fetch the selected (existing) project's real roles ------------
  // A brand-new project has no roles on disk yet, so we skip the fetch (it
  // would 404) and lean on the static fallback below — `roleChoices`/
  // `roleLoading` ignore `roleState` while `isNew`, so a stale value here is
  // inert and we don't need a synchronous reset (which would trip
  // react-hooks/set-state-in-effect). The async transitions (loading→ready/
  // error) follow the same data-fetch pattern as the per-sid history seed.
  useEffect(() => {
    if (isNew || !project) return;
    let cancelled = false;
    setRoleState({ kind: "loading" });
    listProjectRoles(project)
      .then((roles) => {
        if (!cancelled) setRoleState({ kind: "ready", roles });
      })
      .catch((e) => {
        if (cancelled) return;
        if (e instanceof Error && e.message === "UNAUTHENTICATED") {
          // global gate re-auths; just fall back silently.
          setRoleState({ kind: "error" });
          return;
        }
        toastBus.handler?.error(
          `加载角色失败: ${e instanceof Error ? e.message : "unknown"}（回退默认角色）`,
        );
        setRoleState({ kind: "error" });
      });
    return () => {
      cancelled = true;
    };
  }, [project, isNew]);

  // The role <select> options. For an EXISTING project with a successful,
  // non-empty fetch we show its real roles ("role — description"); otherwise
  // (new project / empty / error / still-loading) we fall back to the static
  // ROLE_SUGGESTIONS-seeded list so the user always has cto + the usual hints.
  // `isNew` shortcuts to the fallback regardless of any stale `roleState`.
  // F2-web — the explicit "no role / bare claude" choice leads every option
  // set (existing + new project), so a roleless session is always reachable
  // and never the silent default (resolveRole keeps `cto` as the no-pick
  // fallback).
  const roleChoices: { value: string; label: string }[] = useMemo(() => {
    const roleless = { value: ROLELESS, label: "(无角色 / 裸 claude)" };
    if (!isNew && roleState.kind === "ready" && roleState.roles.length > 0) {
      return [
        roleless,
        ...roleState.roles.map((r) => ({
          value: r.role,
          label: r.description ? `${r.role} — ${r.description}` : r.role,
        })),
      ];
    }
    return [roleless, ...fallbackRoles.map((r) => ({ value: r, label: r }))];
  }, [isNew, roleState, fallbackRoles]);

  const roleLoading = !isNew && roleState.kind === "loading";

  // The role <select>'s controlled value, DERIVED (not effect-synced) so the
  // option set changing (project switch / fetch resolve) can't desync state:
  // honor the user's explicit pick while it's still on offer, otherwise fall
  // back to a sensible default. `role===""` means "no explicit pick yet".
  //
  // F2-web: with ROLELESS now leading every option set, the no-pick fallback
  // must NOT be `roleChoices[0]` (that would silently default to roleless and
  // break FIX-2). Prefer DEFAULT_ROLE (`cto`) when it's on offer, else the
  // first concrete (non-roleless) option, else roleless as the last resort.
  const selectedRole = role && roleChoices.some((c) => c.value === role)
    ? role
    : roleChoices.find((c) => c.value === DEFAULT_ROLE)?.value ??
      roleChoices.find((c) => c.value !== ROLELESS)?.value ??
      roleChoices[0]?.value ??
      "";

  // The wire role: ROLELESS → "" (roleless passthrough), a concrete pick →
  // that role, blank → DEFAULT_ROLE. See chatDefaults.resolveRole (pure +
  // unit-tested for the sentinel semantics).
  const effectiveRole = resolveRole(selectedRole);

  // ---- submit gating -----------------------------------------------------
  const newSlugErr = isNew ? slugError(newSlug.trim()) : null;
  const newPathErr = isNew ? pathError(newPath) : null;
  const ready = isNew
    ? !pending && newSlugErr === null && newPathErr === null
    : !pending && project.length > 0;

  const submit = () => {
    if (!ready) return;
    setPending(true);
    const targetSlug = isNew ? newSlug.trim() : project;
    void onCreate(
      targetSlug,
      effectiveRole,
      vendor,
      hitl ? "hitl" : "skip",
      isNew ? newPath.trim() : undefined,
    )
      .then((ok) => {
        // On success the parent navigates away & unmounts us. On failure it
        // toasted the (human-readable) error; we re-enable so the user can
        // fix the input and retry without losing what they typed.
        if (ok) onCancel();
        else setPending(false);
      })
      .catch(() => setPending(false));
  };

  // Esc closes, Enter submits (when not composing in the textarea-less modal).
  const onKeyDown = (event: React.KeyboardEvent) => {
    if (event.key === "Escape") {
      event.preventDefault();
      if (!pending) onCancel();
    } else if (event.key === "Enter") {
      event.preventDefault();
      submit();
    }
  };

  return (
    <div className="fixed inset-0 z-50 bg-black/50 grid place-items-center p-4" onClick={onCancel}>
      <div
        className="w-full max-w-md rounded-lg bg-surface-900 border border-surface-700 shadow-xl"
        onClick={(event) => event.stopPropagation()}
        onKeyDown={onKeyDown}
      >
        <div className="px-4 h-11 flex items-center justify-between border-b border-surface-700/50">
          <span className="text-sm font-semibold">新建 session</span>
          <button type="button" onClick={onCancel} className="text-text-dim hover:text-text-primary">
            ✕
          </button>
        </div>
        <div className="p-4 space-y-3">
          <label className="block text-xs text-text-dim">项目</label>
          <select
            value={project}
            onChange={(event) => setProject(event.target.value)}
            disabled={pending}
            className="w-full h-9 rounded-md bg-surface-800 border border-surface-700 px-3 text-sm outline-none focus:border-amber-500 disabled:opacity-40"
          >
            {projects.length === 0 && !isNew ? (
              <option value="">（暂无已有项目）</option>
            ) : null}
            {projects.map((item) => (
              <option key={item} value={item}>
                {item}
              </option>
            ))}
            <option value={NEW_PROJECT}>＋ 新建项目…</option>
          </select>

          {isNew ? (
            <div className="space-y-3 rounded-md border border-amber-500/30 bg-amber-500/5 p-3">
              <div>
                <label className="block text-xs text-text-dim mb-1">项目名（slug）</label>
                <input
                  value={newSlug}
                  onChange={(event) => setNewSlug(event.target.value)}
                  disabled={pending}
                  autoFocus
                  placeholder="my-project"
                  className="w-full h-9 rounded-md bg-surface-800 border border-surface-700 px-3 text-sm outline-none focus:border-amber-500 disabled:opacity-40"
                />
                {newSlug.length > 0 && newSlugErr ? (
                  <div className="mt-1 text-[11px] text-red-400">{newSlugErr}</div>
                ) : null}
              </div>
              <div>
                <label className="block text-xs text-text-dim mb-1">工作目录</label>
                <input
                  value={newPath}
                  onChange={(event) => setNewPath(event.target.value)}
                  disabled={pending}
                  placeholder="~/code/my-project 或 /abs/path"
                  className="w-full h-9 rounded-md bg-surface-800 border border-surface-700 px-3 text-sm font-mono outline-none focus:border-amber-500 disabled:opacity-40"
                />
                {newPath.length > 0 && newPathErr ? (
                  <div className="mt-1 text-[11px] text-red-400">{newPathErr}</div>
                ) : null}
              </div>
            </div>
          ) : null}

          <label className="block text-xs text-text-dim">Code agent</label>
          <div className="flex gap-1 rounded-md bg-surface-800 p-0.5">
            {(["claude", "codex"] as const).map((value) => (
              <button
                key={value}
                type="button"
                disabled={pending}
                onClick={() => setVendor(value)}
                className={`flex-1 h-8 rounded text-xs disabled:opacity-40 ${
                  vendor === value ? "bg-surface-700 text-text-primary" : "text-text-dim"
                }`}
              >
                {value === "claude" ? "Claude Code · tmux" : "Codex · app-server"}
              </button>
            ))}
          </div>

          <div className="flex items-center justify-between">
            <label className="block text-xs text-text-dim">Role</label>
            {roleLoading ? (
              <span className="text-[10px] text-text-dim">加载角色中…</span>
            ) : null}
          </div>
          <select
            value={selectedRole}
            onChange={(event) => setRole(event.target.value)}
            disabled={pending || roleLoading}
            className="w-full h-9 rounded-md bg-surface-800 border border-surface-700 px-3 text-sm outline-none focus:border-amber-500 disabled:opacity-40"
          >
            {roleChoices.map((c) => (
              <option key={c.value} value={c.value}>
                {c.label}
              </option>
            ))}
          </select>

          <label className="flex items-center gap-2 text-xs text-text-secondary">
            <input
              type="checkbox"
              checked={hitl}
              disabled={pending}
              onChange={(event) => setHitl(event.target.checked)}
              className="accent-amber-500"
            />
            HITL 批准（非 allowlist 工具需在此点同意/拒绝）
          </label>

          <div className="text-[11px] font-mono text-text-dim leading-5">
            {isNew ? (
              <>
                → <span className="text-text-secondary">POST /api/v1/projects</span> slug=
                <span className="text-text-secondary">{newSlug.trim() || "<slug>"}</span>
                <br />
              </>
            ) : null}
            → <span className="text-text-secondary">
              POST /api/v1/projects/{(isNew ? newSlug.trim() : project) || "<project>"}/sessions
            </span>
            <br />
            → role=
            <span className="text-text-secondary">
              {effectiveRole || "(无角色 / 裸 claude)"}
            </span>{" "}
            vendor=
            <span className="text-text-secondary">{vendor}</span> mode=
            <span className="text-text-secondary">{hitl ? "hitl" : "skip"}</span>
          </div>
        </div>
        <div className="px-4 py-3 flex justify-end gap-2 border-t border-surface-700/50">
          <button
            type="button"
            onClick={onCancel}
            disabled={pending}
            className="h-9 px-3 rounded-md text-sm text-text-dim hover:text-text-primary disabled:opacity-40"
          >
            取消
          </button>
          <button
            type="button"
            disabled={!ready}
            onClick={submit}
            className="h-9 px-3 rounded-md text-sm bg-amber-500 text-surface-950 hover:bg-amber-400 disabled:opacity-40 flex items-center gap-1.5"
          >
            {pending ? (
              <>
                <span className="h-3 w-3 rounded-full border-2 border-surface-950/40 border-t-surface-950 animate-spin" />
                {isNew ? "创建中…" : "切换中…"}
              </>
            ) : isNew ? (
              "创建项目并开始"
            ) : (
              "创建并切过去"
            )}
          </button>
        </div>
      </div>
    </div>
  );
}
