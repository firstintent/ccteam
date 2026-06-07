// v0.8.9 — per-SID session view, extracted from ChatConsole.
//
// THE ARCHITECTURAL FIX: ChatConsole used to be ONE long-lived component
// mixing the persistent shell (sidebar / bottom-nav / cost pill / new-session
// modal / cross-project rail) with the PER-SID session state (transcript rows,
// the `useSessionEvents(sid)` SSE buffer + its fold, the localStorage seed +
// persist, the draft input, the chat|terminal toggle, HITL approval). Because
// per-sid state lived in the shell, switching `sid` only RE-RAN effects to
// reset state — racy: a freshly-opened session briefly showed the PREVIOUS
// session's messages (the SSE buffer + `foldedRef` reset lag a render; plus a
// latent `saveRows(newSid, oldRows)` persist race).
//
// FIX: this component owns everything per-sid and the shell renders it KEYED
// BY SID — `<SessionView key={sid} sid={sid} ... />`. React UNMOUNTS the old +
// MOUNTS a fresh instance on every switch, so ALL per-sid state (events, rows,
// foldedRef, draft, view, scroll) resets ATOMICALLY. "No state survives a
// session switch" is now a structural guarantee, not per-field cleanup —
// killing the reported bug AND the latent persist race at once.
//
// Because `sid` is a PROP fixed for the instance's whole life, the seed / fold
// / persist are MOUNT-scoped: the sid-change reset branches that used to live
// in these effects are gone (sid never changes within an instance).
//
// Red lines preserved: reads structured turn/SSE frames (never scrapes a
// pane); the terminal view is the existing `ccteam-pty.v1` byte relay; HITL
// approval routes through the gateway's pending machinery (NOT a turn).

import { useCallback, useEffect, useLayoutEffect, useRef, useState } from "react";
import { MessageSquare, Send, Square, Terminal } from "lucide-react";
import { TerminalView } from "../components/TerminalView";
import { useSessionEvents } from "../hooks/useSessionEvents";
import {
  getHistory,
  resolveApproval as apiResolveApproval,
  stopSession as apiStopSession,
  submitTurn,
  type SessionView as SessionSummary,
} from "../lib/sessionsApi";
import {
  appendRow,
  eventToRow,
  historyToRows,
  loadRows,
  nextRowId,
  saveRows,
  type TranscriptRow,
} from "./chatTranscript";

/** Per-SID session surface. `sid` is fixed for this instance's whole life
 *  (the shell renders `<SessionView key={sid} sid={sid} />`), so every effect
 *  here is mount-scoped — no sid-change reset branches needed.
 *
 *  `session` is the matching rail entry (vendor/role/project) for the terminal
 *  gating + crumb-adjacent affordances; it may be `null` for a brief window
 *  before the shell's session list resolves (the sid is in the URL but not yet
 *  in the rail). `onSessionChanged` lets a turn/stop nudge the shell to refresh
 *  its rail (so live/idle + new sessions surface) — the shell owns that list. */
export default function SessionView({
  sid,
  session,
  onSessionChanged,
}: {
  sid: string;
  session: SessionSummary | null;
  onSessionChanged?: () => void;
}) {
  const [draft, setDraft] = useState("");
  const [view, setView] = useState<"chat" | "terminal">("chat");

  // Per-sid transcript. Seeded from localStorage on MOUNT, then from mirrored
  // history, then live-appended from SSE. Because `sid` is fixed per instance,
  // the seed runs once on mount (not on a sid change) and a fresh instance
  // always starts from THIS sid's persisted rows (or empty) — never the
  // previous session's.
  const [rows, setRows] = useState<TranscriptRow[]>(() => loadRows(sid));

  const { events, connected, lastError, gatewayUnavailable } = useSessionEvents(sid);

  // ---- authoritative seed: mirrored history (mount-scoped) ---------------
  // Replaces the localStorage seed with the server's mirrored turns when it
  // has any. sid is fixed, so this is a one-shot per mount.
  useEffect(() => {
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

  // ---- live SSE → append into this sid's transcript ----------------------
  // Track how many events we've folded so a re-render doesn't re-append. sid
  // is fixed per instance, so `foldedRef` starts at 0 for a fresh mount — the
  // old "reset foldedRef on sid change" effect is gone (no longer needed).
  const foldedRef = useRef(0);
  useEffect(() => {
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
  }, [events]);

  // ---- persist this sid's transcript -------------------------------------
  // sid is fixed, so there is no `saveRows(newSid, oldRows)` race: this
  // instance only ever writes its own sid's key.
  useEffect(() => {
    saveRows(sid, rows);
  }, [sid, rows]);

  const pushRow = useCallback((row: Omit<TranscriptRow, "id">) => {
    setRows((current) => appendRow(current, { ...row, id: nextRowId(row.kind) }));
  }, []);

  // ---- send a turn -------------------------------------------------------
  const submit = useCallback(() => {
    const content = draft.trim();
    if (!content) return;
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
      const option = row.options?.[optionIndex];
      if (!row.token || !option?.id) {
        pushRow({
          kind: "system",
          content: "无法批准: 该提示缺少 token/选项 id(请在 IM 批准,或重开会话)",
        });
        return;
      }
      // mark the row resolved so the chips disable.
      setRows((current) => current.map((r) => (r.id === row.id ? { ...r, resolved: true } : r)));
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
    apiStopSession(sid)
      .then(() => {
        pushRow({ kind: "system", content: "会话已停止" });
        onSessionChanged?.();
      })
      .catch((e) => {
        pushRow({
          kind: "system",
          content: `停止失败: ${e instanceof Error ? e.message : "unknown"}`,
        });
      });
  }, [sid, pushRow, onSessionChanged]);

  // v0.8.8 B5 — claude-only for now. The PTY backend resolves per-sid panes for
  // BOTH vendors, but the codex terminal is unverified on a real codex pane
  // (the daemon wires the codex app-server adapter, which owns no tmux/rmux
  // pane), so we don't promise a codex terminal in the UI yet.
  const canTerminal = session?.vendor === "claude";
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
        : "连接中…";

  return (
    <>
      {/* session sub-bar: crumb + connection dot + Stop + Chat|终端 tabs */}
      <div className="h-10 shrink-0 px-4 flex items-center gap-3 border-b border-surface-700/30">
        <span className="text-xs text-text-dim shrink-0">会话 →</span>
        {session ? (
          <span className="flex items-center gap-2 text-xs min-w-0">
            <span className="text-status-running">●</span>
            <span className="font-semibold truncate">{session.project}</span>
            <span className="text-text-dim">/</span>
            <span className={session.vendor === "claude" ? "text-vendor-claude" : "text-vendor-codex"}>
              {[session.vendor, session.role].filter(Boolean).join(" · ")}
            </span>
            <span className="font-mono text-text-dim">{session.sid}</span>
          </span>
        ) : (
          <span className="text-xs text-text-dim font-mono">{sid}</span>
        )}
        <span className="hidden sm:flex items-center gap-1.5 text-xs text-text-dim">
          <span
            className={`h-2 w-2 rounded-full ${
              connected
                ? "bg-status-running"
                : gatewayUnavailable || lastError
                  ? "bg-status-error"
                  : "bg-brand-500"
            }`}
          />
          {statusText}
        </span>
        <span className="flex-1" />
        <button
          type="button"
          onClick={stopActive}
          title="停止会话"
          className="h-7 px-2 rounded text-xs flex items-center gap-1 text-text-dim hover:text-status-error hover:bg-surface-800"
        >
          <Square className="h-3.5 w-3.5" /> 停止
        </button>
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

      {showTerminal && session?.project ? (
        <TerminalView slug={session.project} sid={sid} className="flex-1 min-h-0" />
      ) : (
        <>
          <div
            ref={scrollRef}
            onScroll={onTranscriptScroll}
            className="flex-1 min-h-0 overflow-y-auto p-4 space-y-3"
          >
            {rows.map((row) => {
              if (row.kind === "approval") {
                return (
                  <div
                    key={row.id}
                    className="max-w-[760px] rounded-md px-3 py-2.5 text-sm bg-brand-500/10 border border-brand-500/30"
                  >
                    <div className="text-brand-400 mb-2">{row.content}</div>
                    <div className="flex flex-wrap gap-2">
                      {(row.options ?? []).map((opt, i) => (
                        <button
                          key={`${row.id}-${i}`}
                          type="button"
                          disabled={row.resolved}
                          onClick={() => resolveApproval(row, i)}
                          className="h-7 px-3 rounded-md text-xs bg-brand-500 text-surface-950 hover:bg-brand-400 disabled:opacity-40"
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
                      ? "ml-auto bg-brand-dim/40 border border-brand-500/20"
                      : row.kind === "tool"
                        ? "bg-surface-800/70 border border-surface-700/50 text-text-secondary font-mono text-xs"
                        : "bg-surface-800 border border-surface-700/40"
                  }`}
                >
                  {row.content}
                </div>
              );
            })}
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
                className="min-h-11 max-h-32 flex-1 resize-y rounded-md bg-surface-800 border border-surface-700 px-3 py-2 text-sm outline-none focus:border-brand-500"
                placeholder="发消息 / 命令(/compact /clear …)…"
              />
              <button
                type="button"
                onClick={submit}
                className="h-11 w-11 shrink-0 rounded-md bg-brand-500 text-surface-950 hover:bg-brand-400 grid place-items-center"
                title="发送"
              >
                <Send className="h-4 w-4" />
              </button>
            </div>
          </div>
        </>
      )}
    </>
  );
}
