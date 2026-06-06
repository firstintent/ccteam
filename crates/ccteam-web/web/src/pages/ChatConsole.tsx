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
import { fetchDashboard } from "../lib/dashboardApi";
import {
  createSession as apiCreateSession,
  getHistory,
  listSessions,
  resolveApproval as apiResolveApproval,
  stopSession as apiStopSession,
  submitTurn,
  type SessionView,
} from "../lib/sessionsApi";
import { DEFAULT_ROLE, ROLE_SUGGESTIONS } from "./chatDefaults";
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

  // ---- create a new session ---------------------------------------------
  const createSession = useCallback(
    async (slug: string, role: string, vendor: string, permissionMode: "skip" | "hitl") => {
      setModalOpen(false);
      try {
        const { sid: newSid } = await apiCreateSession(slug, {
          role,
          vendor,
          permission_mode: permissionMode,
        });
        await refreshSessions();
        navigate(`/chat/s/${encodeURIComponent(newSid)}`);
      } catch (e) {
        pushRow({
          kind: "system",
          content: `新建 session 失败: ${e instanceof Error ? e.message : "unknown"}`,
        });
      }
    },
    [refreshSessions, navigate, pushRow],
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
              onClick={() => setModalOpen(true)}
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
          roleOptions={roleOptions}
          defaultProject={activeView?.project ?? projects[0] ?? ""}
          onCancel={() => setModalOpen(false)}
          onCreate={createSession}
        />
      ) : null}
    </div>
  );
}

// New-session modal: pick an EXISTING project (the gateway create endpoint
// is per-project), code agent, role (default cto), and permission mode.
// Brand-new project scaffolding is an operator action (`ccteam init` / the
// dashboard) — out of scope for the per-session chat create.
function NewSessionModal({
  projects,
  roleOptions,
  defaultProject,
  onCancel,
  onCreate,
}: {
  projects: string[];
  roleOptions: string[];
  defaultProject: string;
  onCancel: () => void;
  onCreate: (
    slug: string,
    role: string,
    vendor: string,
    permissionMode: "skip" | "hitl",
  ) => void;
}) {
  const [project, setProject] = useState(defaultProject);
  const [vendor, setVendor] = useState<"claude" | "codex">("claude");
  const [role, setRole] = useState("");
  const [hitl, setHitl] = useState(false);

  const effectiveRole = role.trim() || DEFAULT_ROLE;
  const ready = project.length > 0;

  const submit = () => {
    if (!ready) return;
    onCreate(project, effectiveRole, vendor, hitl ? "hitl" : "skip");
  };

  return (
    <div className="fixed inset-0 z-50 bg-black/50 grid place-items-center p-4" onClick={onCancel}>
      <div
        className="w-full max-w-md rounded-lg bg-surface-900 border border-surface-700 shadow-xl"
        onClick={(event) => event.stopPropagation()}
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
            className="w-full h-9 rounded-md bg-surface-800 border border-surface-700 px-3 text-sm outline-none focus:border-amber-500"
          >
            {projects.length === 0 ? <option value="">（暂无已有项目）</option> : null}
            {projects.map((item) => (
              <option key={item} value={item}>
                {item}
              </option>
            ))}
          </select>

          <label className="block text-xs text-text-dim">Code agent</label>
          <div className="flex gap-1 rounded-md bg-surface-800 p-0.5">
            {(["claude", "codex"] as const).map((value) => (
              <button
                key={value}
                type="button"
                onClick={() => setVendor(value)}
                className={`flex-1 h-8 rounded text-xs ${
                  vendor === value ? "bg-surface-700 text-text-primary" : "text-text-dim"
                }`}
              >
                {value === "claude" ? "Claude Code · tmux" : "Codex · app-server"}
              </button>
            ))}
          </div>

          <label className="block text-xs text-text-dim">Role</label>
          <input
            list="ccteam-chat-roles"
            value={role}
            onChange={(event) => setRole(event.target.value)}
            placeholder={`${DEFAULT_ROLE} / reviewer / api …`}
            className="w-full h-9 rounded-md bg-surface-800 border border-surface-700 px-3 text-sm outline-none focus:border-amber-500"
          />
          <datalist id="ccteam-chat-roles">
            {roleOptions.map((item) => (
              <option key={item} value={item} />
            ))}
          </datalist>

          <label className="flex items-center gap-2 text-xs text-text-secondary">
            <input
              type="checkbox"
              checked={hitl}
              onChange={(event) => setHitl(event.target.checked)}
              className="accent-amber-500"
            />
            HITL 批准（非 allowlist 工具需在此点同意/拒绝）
          </label>

          <div className="text-[11px] font-mono text-text-dim leading-5">
            → <span className="text-text-secondary">POST /api/v1/projects/{project || "<project>"}/sessions</span>
            <br />
            → role=<span className="text-text-secondary">{effectiveRole}</span> vendor=
            <span className="text-text-secondary">{vendor}</span> mode=
            <span className="text-text-secondary">{hitl ? "hitl" : "skip"}</span>
          </div>
        </div>
        <div className="px-4 py-3 flex justify-end gap-2 border-t border-surface-700/50">
          <button
            type="button"
            onClick={onCancel}
            className="h-9 px-3 rounded-md text-sm text-text-dim hover:text-text-primary"
          >
            取消
          </button>
          <button
            type="button"
            disabled={!ready}
            onClick={submit}
            className="h-9 px-3 rounded-md text-sm bg-amber-500 text-surface-950 hover:bg-amber-400 disabled:opacity-40"
          >
            创建并切过去
          </button>
        </div>
      </div>
    </div>
  );
}
