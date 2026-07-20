// v0.8.24 Track A — the Conversation view (prototype `#view-conv`), keyed by
// sid from the shell (`<SessionView key={sid} …/>` — a fresh instance mounts
// on every switch, so all per-sid state resets atomically; the v0.8.9
// structural guarantee is unchanged).
//
// Skin = prototype: conv-head (status dot · title · meta chips · Chat|终端
// tabs · cost pill) + chat-scroll message stream (user right-aligned soft
// bubble 14/14/4/14, agent left with NO fill + full Markdown rendering,
// streaming cursor while a turn is in flight) + the same composer as Home
// (sans ctx-bar).
//
// Data spine unchanged (红线 §1.6-7): localStorage seed → getHistory mirror →
// live SSE fold; drafts persist per-sid; IME composition guard; Stop
// interrupts the running turn (session kept); HITL approvals resolve through
// the gateway pending machinery (never a fake turn). The terminal tab renders
// ONLY for a claude session that owns a pane (protocol ≠ stream-json) — the
// byte-exact `ccteam-pty.v1` relay via TerminalView.

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { ArrowDown } from "lucide-react";
import { ChatComposer } from "../components/ChatComposer";
import type { TurnAttachment } from "../lib/attachmentsApi";
import CostPill from "../components/CostPill";
import { Markdown } from "../components/Markdown";
import { TerminalView } from "../components/TerminalView";
import { VendorChip } from "../components/VendorChip";
import { useSessionEvents } from "../hooks/useSessionEvents";
import { makeT, type Lang } from "../lib/i18n";
import {
  defaultDraft,
  normalizeDraft,
  type ComposerDraft,
} from "../lib/vendors";
import {
  getHistory,
  getSessionStatus,
  interruptSession as apiInterruptSession,
  resolveApproval as apiResolveApproval,
  submitTurn,
  type SessionView as SessionSummary,
} from "../lib/sessionsApi";
import {
  appendEvent,
  appendRow,
  historyToRows,
  loadRows,
  nextRowId,
  saveRows,
  type TranscriptRow,
} from "./chatTranscript";
import { railSessionLabel } from "./railHelpers";

/** Map the backend effort token to the dictionary key (unknown → null hides
 *  the label — never a fake value). */
// eslint-disable-next-line react-refresh/only-export-components -- pure helper co-located for unit tests.
export function effortKeyOf(effort: string | null | undefined): ComposerDraft["effortKey"] | null {
  switch ((effort ?? "").toLowerCase()) {
    case "low":
      return "effLow";
    case "medium":
      return "effMid";
    case "high":
      return "effHigh";
    case "max":
    case "xhigh":
      return "effMax";
    default:
      return null;
  }
}

export default function SessionView({
  sid,
  session,
  lang = "zh",
  isAdmin = false,
}: {
  sid: string;
  session: SessionSummary | null;
  lang?: Lang;
  isAdmin?: boolean;
}) {
  const t = makeT(lang);
  const [view, setView] = useState<"chat" | "terminal">("chat");
  const [busyMark, setBusyMark] = useState<number | null>(null);
  const [rows, setRows] = useState<TranscriptRow[]>(() => loadRows(sid));

  const { events, connected, lastError, gatewayUnavailable } = useSessionEvents(sid);

  // ---- authoritative seed: mirrored history (mount-scoped) -----------------
  useEffect(() => {
    let cancelled = false;
    getHistory(sid)
      .then((h) => {
        if (cancelled) return;
        const seeded = historyToRows(h.events);
        if (seeded.length > 0) setRows(seeded);
      })
      .catch(() => {
        /* best-effort — keep the localStorage rows (or empty) on error */
      });
    return () => {
      cancelled = true;
    };
  }, [sid]);

  // ---- live SSE → append into this sid's transcript ------------------------
  const foldedRef = useRef(0);
  useEffect(() => {
    if (events.length <= foldedRef.current) return;
    const fresh = events.slice(foldedRef.current);
    foldedRef.current = events.length;
    setRows((current) => {
      let next = current;
      for (const ev of fresh) {
        next = appendEvent(next, ev);
      }
      return next;
    });
  }, [events]);

  // ---- persist this sid's transcript ---------------------------------------
  useEffect(() => {
    saveRows(sid, rows);
  }, [sid, rows]);

  // ---- per-session status (model + effort + ctx%) --------------------------
  const [statusModel, setStatusModel] = useState<string | null>(null);
  const [statusEffort, setStatusEffort] = useState<string | null>(null);
  const [ctxPct, setCtxPct] = useState<number | null>(null);
  const doneCount = events.reduce((n, ev) => (ev.done ? n + 1 : n), 0);
  const busy = busyMark !== null && doneCount === busyMark;
  useEffect(() => {
    let cancelled = false;
    getSessionStatus(sid)
      .then((s) => {
        if (cancelled) return;
        setStatusModel(s.model);
        setStatusEffort(s.effort ?? null);
        setCtxPct(s.context ? s.context.pct : null);
      })
      .catch(() => {
        if (!cancelled) {
          setStatusModel(null);
          setStatusEffort(null);
          setCtxPct(null);
        }
      });
    return () => {
      cancelled = true;
    };
  }, [sid, doneCount]);

  const pushRow = useCallback((row: Omit<TranscriptRow, "id">) => {
    setRows((current) => appendRow(current, { ...row, id: nextRowId(row.kind) }));
  }, []);

  // ---- send a turn ----------------------------------------------------------
  const submitText = useCallback(
    (content: string, attachments: TurnAttachment[] = []) => {
      // Optimistic transcript row: show the text plus a compact attachment
      // note (the server-side turn text carries the full attachment lines).
      const names = attachments
        .map((a) => (a.kind === "skill" ? `skill:${a.name}` : (a.name ?? a.path ?? "")))
        .filter(Boolean);
      const shown = names.length > 0 ? `${content}\n📎 ${names.join(", ")}` : content;
      pushRow({ kind: "user", content: shown });
      setBusyMark(doneCount);
      submitTurn(sid, content, attachments).catch((e) => {
        setBusyMark(null);
        const detail = e instanceof Error ? e.message : "unknown";
        pushRow({
          kind: "system",
          content: detail.startsWith("发送失败") ? detail : `发送失败: ${detail}`,
        });
      });
    },
    [sid, pushRow, doneCount],
  );

  // ---- resolve a HITL approval prompt (gateway pending machinery) ----------
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
      setRows((current) => current.map((r) => (r.id === row.id ? { ...r, resolved: true } : r)));
      pushRow({ kind: "user", content: `→ ${option.label}` });
      apiResolveApproval(sid, row.token, option.id).catch((e) => {
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

  // ---- interrupt the running turn (session kept) ----------------------------
  const interruptActive = useCallback(() => {
    apiInterruptSession(sid)
      .then(() => {
        pushRow({ kind: "system", content: "已中断当前 turn(会话保留)" });
      })
      .catch((e) => {
        pushRow({
          kind: "system",
          content: `中断失败: ${e instanceof Error ? e.message : "unknown"}`,
        });
      });
  }, [sid, pushRow]);

  // Terminal tab: only a claude session that owns a pane (protocol ≠
  // stream-json). A stream-json session has NO pane → the tab does not exist.
  const isStreamJson = session?.protocol === "stream-json";
  const canTerminal = session?.vendor === "claude" && !isStreamJson;
  const showTerminal = view === "terminal" && canTerminal;

  // Auto-scroll only when already near the bottom; 「回到最新」 appears when
  // the reader scrolled up (a streaming reply never yanks them down).
  const scrollRef = useRef<HTMLDivElement>(null);
  const atBottomRef = useRef(true);
  const [showJump, setShowJump] = useState(false);
  const onTranscriptScroll = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    const atBottom = el.scrollHeight - el.scrollTop - el.clientHeight < 120;
    atBottomRef.current = atBottom;
    setShowJump(!atBottom);
  }, []);
  const jumpToBottom = useCallback(() => {
    const el = scrollRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
    atBottomRef.current = true;
    setShowJump(false);
  }, []);
  useLayoutEffect(() => {
    if (!showTerminal && atBottomRef.current && scrollRef.current) {
      scrollRef.current.scrollTop = scrollRef.current.scrollHeight;
    }
  }, [rows, showTerminal]);

  // conv-head status dot: busy amber › connection state.
  const headDot = busy
    ? "dot busy"
    : gatewayUnavailable || lastError
      ? "dot err"
      : connected
        ? "dot on"
        : "dot off";

  const title = session ? railSessionLabel(session) : sid;
  const vendor = session?.vendor ?? "claude";
  const who = `${vendor} · ${sid}${statusModel ? ` · ${statusModel}` : ""}`;

  // The conversation composer reflects this session's FIXED spawn parameters
  // (locked: picking toasts; /model via the input still works).
  const lockedDraft: ComposerDraft = useMemo(
    () =>
      normalizeDraft({
        ...defaultDraft(),
        vendor: (["claude", "codex", "grok", "opencode", "kimi"].includes(vendor)
          ? vendor
          : "claude") as ComposerDraft["vendor"],
        model: statusModel ?? "",
        hitl: session?.permission_mode === "hitl",
        // Unknown/unreported effort reads 默认 (honest), not a made-up 中.
        effortKey: effortKeyOf(statusEffort) ?? "effDefault",
      }),
    [vendor, statusModel, statusEffort, session?.permission_mode],
  );

  return (
    <section className="view active" data-testid="conversation-view">
      <div className="conv-head">
        <span className={headDot} data-testid="conv-dot" />
        <span className="title" data-testid="conv-title">
          {title}
        </span>
        <div className="meta">
          <span className="chip sid">{sid}</span>
          {session ? <span className="chip">{session.project}</span> : null}
          <VendorChip vendor={vendor} />
          {session?.host && session.host !== "local" ? (
            <span className="chip">@ {session.host}</span>
          ) : null}
          {statusModel ? (
            <span className="chip" title="model · context window">
              {statusModel}
              {ctxPct !== null ? ` · ctx ${Math.round(ctxPct)}%` : ""}
            </span>
          ) : null}
        </div>
        <div className="tabs">
          <button
            type="button"
            className={`tab ${!showTerminal ? "active" : ""}`}
            onClick={() => setView("chat")}
          >
            {t("chatTab")}
          </button>
          {canTerminal ? (
            <button
              type="button"
              className={`tab ${showTerminal ? "active" : ""}`}
              onClick={() => setView("terminal")}
              data-testid="terminal-tab"
            >
              {t("terminal")}
            </button>
          ) : null}
        </div>
        <CostPill />
      </div>

      {showTerminal && session?.project ? (
        <div className="term-wrap">
          <TerminalView slug={session.project} sid={sid} className="flex-1 min-h-0" />
        </div>
      ) : (
        <>
          <div style={{ position: "relative", flex: 1, minHeight: 0, display: "flex" }}>
            <div
              ref={scrollRef}
              onScroll={onTranscriptScroll}
              className="chat-scroll"
              data-testid="chat-scroll"
            >
              <div className="chat-inner">
                {rows.map((row) => {
                  if (row.kind === "approval") {
                    return (
                      <div key={row.id} className="msg approval fade-in">
                        <span className="who">{t("approve")}</span>
                        <div className="bubble">
                          {row.content}
                          <div style={{ display: "flex", gap: 8, flexWrap: "wrap", marginTop: 10 }}>
                            {(row.options ?? []).map((opt, i) => (
                              <button
                                key={`${row.id}-${i}`}
                                type="button"
                                className="btn primary mini"
                                disabled={row.resolved}
                                style={row.resolved ? { opacity: 0.45 } : undefined}
                                onClick={() => resolveApproval(row, i)}
                              >
                                {opt.label}
                              </button>
                            ))}
                          </div>
                          {row.resolved ? (
                            <div style={{ marginTop: 6, fontSize: 11, color: "var(--text-faint)" }}>
                              已回应
                            </div>
                          ) : null}
                        </div>
                      </div>
                    );
                  }
                  if (row.kind === "system") {
                    return (
                      <div key={row.id} className="msg system fade-in">
                        <div className="bubble">{row.content}</div>
                      </div>
                    );
                  }
                  if (row.kind === "activity") {
                    return (
                      <div key={row.id} className="msg activity">
                        <div className="bubble">{row.content}</div>
                      </div>
                    );
                  }
                  if (row.kind === "user") {
                    return (
                      <div key={row.id} className="msg user fade-in">
                        <span className="who">you</span>
                        <div className="bubble">{row.content}</div>
                      </div>
                    );
                  }
                  if (row.kind === "tool") {
                    return (
                      <div key={row.id} className="msg tool fade-in">
                        <div className="bubble">{row.content}</div>
                      </div>
                    );
                  }
                  // assistant — full Markdown document (红线: never plain text).
                  return (
                    <div key={row.id} className="msg agent fade-in">
                      <span className="who">{who}</span>
                      <div className="bubble md">
                        <Markdown content={row.content} />
                      </div>
                    </div>
                  );
                })}
                {busy ? (
                  <div className="msg agent" aria-label="生成中" data-testid="streaming-cursor">
                    <div className="bubble">
                      <span className="cursor" />
                    </div>
                  </div>
                ) : null}
              </div>
            </div>
            {showJump ? (
              <button type="button" className="jump-latest" onClick={jumpToBottom}>
                <ArrowDown /> 回到最新
              </button>
            ) : null}
          </div>

          <div className="conv-composer-wrap">
            <div className="composer-group">
              <ChatComposer
                draftKey={sid}
                lang={lang}
                placeholderKey="convPh"
                busy={busy}
                onStop={interruptActive}
                onSend={submitText}
                draft={lockedDraft}
                onDraftChange={() => {}}
                locked
                isAdmin={isAdmin}
                modelLabel={statusModel ?? ""}
                uploadSlug={session?.project}
              />
            </div>
          </div>
        </>
      )}
    </section>
  );
}
