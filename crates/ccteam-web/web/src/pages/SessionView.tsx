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
import { ArrowDown, Clock, Pencil, X } from "lucide-react";
import { ChatComposer } from "../components/ChatComposer";
import { InlineRename } from "../components/InlineRename";
import type { TurnAttachment } from "../lib/attachmentsApi";
import CostPill from "../components/CostPill";
import { Markdown } from "../components/Markdown";
import { TerminalView } from "../components/TerminalView";
import { VendorChip } from "../components/VendorChip";
import { useSessionEvents } from "../hooks/useSessionEvents";
import { makeT, type Lang } from "../lib/i18n";
import { defaultDraft, normalizeDraft, vendorSpec, type ComposerDraft } from "../lib/vendors";
import {
  getHistory,
  getDaemonTimezone,
  getSessionStatus,
  listScheduled,
  createScheduled,
  cancelScheduled,
  interruptSession as apiInterruptSession,
  projectUploadUrl,
  resolveApproval as apiResolveApproval,
  submitTurn,
  type OutboundAttachmentRef,
  type SessionView as SessionSummary,
  type ScheduledItem,
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

function formatAttachmentSize(size: number): string {
  if (size < 1024) return `${size} B`;
  if (size < 1024 * 1024) return `${(size / 1024).toFixed(1)} KB`;
  return `${(size / (1024 * 1024)).toFixed(1)} MB`;
}

/** A row's server-side timestamp (WEB-TS-1, `row.ts` — RFC 3339) rendered as
 *  local `HH:MM`; the full date-time rides the tooltip. Sits on its own line
 *  under the bubble, so it never squeezes the bubble on narrow viewports.
 *  Absent/unparseable `ts` (old daemons, rows persisted before this field)
 *  renders nothing. */
function RowTime({ ts, lang }: { ts?: string; lang: Lang }) {
  if (!ts) return null;
  const when = new Date(ts);
  if (Number.isNaN(when.getTime())) return null;
  const locale = lang === "en" ? "en-US" : "zh-CN";
  return (
    <time className="msg-time" dateTime={ts} title={when.toLocaleString(locale)}>
      {when.toLocaleTimeString(locale, { hour: "2-digit", minute: "2-digit", hour12: false })}
    </time>
  );
}

function OutboundAttachments({
  project,
  attachments,
}: {
  project?: string;
  attachments?: OutboundAttachmentRef[];
}) {
  if (!project || !attachments || attachments.length === 0) return null;
  return (
    <div className="outbound-attachments" data-testid="outbound-attachments">
      {attachments.map((attachment) => {
        const url = projectUploadUrl(project, attachment.id);
        return attachment.kind === "image" ? (
          <a
            key={attachment.id}
            className="outbound-attachment image"
            href={url}
            aria-label={attachment.name}
          >
            <img loading="lazy" src={url} alt={attachment.name} />
            <span>{attachment.name}</span>
          </a>
        ) : (
          <a
            key={attachment.id}
            className="outbound-attachment file"
            href={url}
            download={attachment.name}
          >
            <span>📎 {attachment.name}</span>
            <small>{formatAttachmentSize(attachment.size)}</small>
          </a>
        );
      })}
    </div>
  );
}

export default function SessionView({
  sid,
  session,
  lang = "zh",
  onRename,
}: {
  sid: string;
  session: SessionSummary | null;
  lang?: Lang;
  /** Rename this session's title (the shell owns the request + toast, so the
   *  rail and this header stay in lockstep). Omitted ⇒ the title is plain
   *  text, no edit affordance. */
  onRename?: (sid: string, title: string) => void | Promise<void>;
}) {
  const t = makeT(lang);
  const [view, setView] = useState<"chat" | "terminal">("chat");
  // Inline title edit in the header. `pendingTitle` is the optimistic value
  // shown only WHILE the shell's rename is in flight — without it the header
  // visibly snaps back to the old title for a beat after Enter. It is cleared
  // the moment the request settles (the shell refreshes the session list
  // before resolving), so a server-side truncation — or a failure — always
  // wins over what the user typed.
  const [editingTitle, setEditingTitle] = useState(false);
  const [pendingTitle, setPendingTitle] = useState<string | null>(null);
  const [busyMark, setBusyMark] = useState<number | null>(null);
  const [rows, setRows] = useState<TranscriptRow[]>(() => loadRows(sid));

  const { events, connected, lastError, gatewayUnavailable, connectionEpoch } =
    useSessionEvents(sid);

  // The SSE buffer survives reconnects. Keep both the fold cursor and a live
  // view of its current length so an authoritative history reseed can install
  // a precise barrier: everything already buffered is represented by history;
  // only frames arriving after the reseed append from then on.
  const foldedRef = useRef(0);
  const eventsRef = useRef(events);
  const historyRequestRef = useRef(0);
  const [historyPage, setHistoryPage] = useState({
    hasMore: false,
    nextBefore: null as string | null,
    loadingEarlier: false,
  });
  useEffect(() => {
    eventsRef.current = events;
  }, [events]);

  // ---- authoritative seed: mirrored history (mount-scoped) -----------------
  useEffect(() => {
    let cancelled = false;
    const request = ++historyRequestRef.current;
    getHistory(sid)
      .then((h) => {
        if (cancelled || request !== historyRequestRef.current) return;
        const seeded = historyToRows(h.events);
        if (seeded.length > 0) setRows(seeded);
        setHistoryPage({
          hasMore: h.has_more === true,
          nextBefore: h.next_before ?? null,
          loadingEarlier: false,
        });
      })
      .catch(() => {
        /* best-effort — keep the localStorage rows (or empty) on error */
      });
    return () => {
      cancelled = true;
    };
  }, [sid]);

  // The first successful open races the mount seed above, so epoch 1 needs no
  // second fetch. Every later open follows a real disconnect: the server's
  // turns.jsonl mirror is authoritative even when the SSE replay ring no
  // longer contains every missed answer.
  useEffect(() => {
    if (connectionEpoch <= 1) return;
    let cancelled = false;
    const request = ++historyRequestRef.current;
    getHistory(sid)
      .then((h) => {
        if (cancelled || request !== historyRequestRef.current) return;
        foldedRef.current = eventsRef.current.length;
        setRows(historyToRows(h.events));
        setHistoryPage({
          hasMore: h.has_more === true,
          nextBefore: h.next_before ?? null,
          loadingEarlier: false,
        });
      })
      .catch(() => {
        /* best-effort — keep the current transcript on reseed failure */
      });
    return () => {
      cancelled = true;
    };
  }, [sid, connectionEpoch]);

  const loadEarlier = useCallback(() => {
    if (!historyPage.hasMore || !historyPage.nextBefore || historyPage.loadingEarlier) return;
    const before = historyPage.nextBefore;
    const request = historyRequestRef.current;
    setHistoryPage((current) => ({ ...current, loadingEarlier: true }));
    getHistory(sid, { before })
      .then((history) => {
        if (request !== historyRequestRef.current) return;
        const earlier = historyToRows(history.events);
        if (earlier.length > 0) setRows((current) => [...earlier, ...current]);
        setHistoryPage({
          hasMore: history.has_more === true,
          nextBefore: history.next_before ?? null,
          loadingEarlier: false,
        });
      })
      .catch(() => {
        if (request !== historyRequestRef.current) return;
        setHistoryPage((current) => ({ ...current, loadingEarlier: false }));
      });
  }, [sid, historyPage]);

  // ---- live SSE → append into this sid's transcript ------------------------
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

  // ---- delayed user-message queue ------------------------------------------
  const [scheduled, setScheduled] = useState<ScheduledItem[]>([]);
  const [daemonTimezone, setDaemonTimezone] = useState("local");
  const scheduledRevision = events.reduce(
    (count, event) => count + (event.kind === "scheduled_changed" ? 1 : 0),
    0,
  );
  const reloadScheduled = useCallback(() => {
    listScheduled(sid)
      .then(setScheduled)
      .catch(() => {
        /* best-effort: retain the last authoritative queue */
      });
  }, [sid]);
  useEffect(() => {
    reloadScheduled();
  }, [reloadScheduled, scheduledRevision]);
  useEffect(() => {
    getDaemonTimezone().then(setDaemonTimezone).catch(() => setDaemonTimezone("local"));
  }, []);

  const scheduleText = useCallback(
    (content: string, when: string) => {
      createScheduled(sid, content, when)
        .then(() => reloadScheduled())
        .catch((error) => {
          pushRow({
            kind: "system",
            content: `${t("scheduleFailed")}: ${error instanceof Error ? error.message : "unknown"}`,
          });
        });
    },
    [sid, reloadScheduled, pushRow, t],
  );

  const removeScheduled = useCallback(
    (id: string) => {
      cancelScheduled(sid, id)
        .then(() => reloadScheduled())
        .catch((error) => {
          pushRow({
            kind: "system",
            content: `${t("scheduleCancelFailed")}: ${error instanceof Error ? error.message : "unknown"}`,
          });
        });
    },
    [sid, reloadScheduled, pushRow, t],
  );

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

  const serverTitle = session ? railSessionLabel(session) : sid;
  const title = pendingTitle ?? serverTitle;
  const vendor = session?.vendor ?? "claude";
  const who = `${vendor} · ${sid}${statusModel ? ` · ${statusModel}` : ""}`;

  // The conversation composer reflects this session's FIXED spawn parameters
  // (locked: picking toasts; /model via the input still works). What the live
  // session REPORTS rides the display props (`modelLabel` / `effortLabel`)
  // rather than the draft, because the draft is menu state — normalizeDraft
  // holds it to rows the menu offers, while the pill must print the session's
  // own tokens verbatim (unreported ⇒ 默认 / `default`, never a made-up 中).
  const lockedDraft: ComposerDraft = useMemo(
    () =>
      normalizeDraft({
        ...defaultDraft(),
        vendor: vendorSpec(vendor).id,
        model: statusModel ?? "",
        hitl: session?.permission_mode === "hitl",
        effort: statusEffort ?? "",
      }),
    [vendor, statusModel, statusEffort, session?.permission_mode],
  );

  return (
    <section className="view active" data-testid="conversation-view">
      <div className="conv-head">
        <span className={headDot} data-testid="conv-dot" />
        {editingTitle && onRename ? (
          <InlineRename
            className="title rename-input"
            initial={title}
            ariaLabel={`${t("renameTip")} ${sid}`}
            onSubmit={(next) => {
              setEditingTitle(false);
              setPendingTitle(next);
              void Promise.resolve(onRename(sid, next)).finally(() => setPendingTitle(null));
            }}
            onCancel={() => setEditingTitle(false)}
          />
        ) : (
          <span
            className="title"
            data-testid="conv-title"
            title={onRename ? `${t("renameTip")} · ${t("renameHint")}` : undefined}
            onDoubleClick={() => {
              if (onRename) setEditingTitle(true);
            }}
          >
            {title}
          </span>
        )}
        {onRename && !editingTitle ? (
          <button
            type="button"
            className="icon-btn sm rename-title"
            data-testid="conv-rename"
            title={t("renameTip")}
            aria-label={`${t("renameTip")} ${sid}`}
            onClick={() => setEditingTitle(true)}
          >
            <Pencil />
          </button>
        ) : null}
        <div className="meta">
          <span className="chip sid">{sid}</span>
          {session ? <span className="chip">{session.project}</span> : null}
          {session ? (
            <span className="chip" data-testid="session-role">
              {session.role || t("noRole")}
            </span>
          ) : null}
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
                {historyPage.hasMore ? (
                  <button
                    type="button"
                    className="btn ghost mini self-center"
                    data-testid="load-earlier"
                    disabled={historyPage.loadingEarlier}
                    onClick={loadEarlier}
                  >
                    {historyPage.loadingEarlier
                      ? lang === "en"
                        ? "Loading…"
                        : "加载中…"
                      : lang === "en"
                        ? "Load earlier"
                        : "加载更早消息"}
                  </button>
                ) : null}
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
                        <RowTime ts={row.ts} lang={lang} />
                      </div>
                    );
                  }
                  if (row.kind === "system") {
                    return (
                      <div key={row.id} className="msg system fade-in">
                        <div className="bubble">{row.content}</div>
                        <RowTime ts={row.ts} lang={lang} />
                      </div>
                    );
                  }
                  if (row.kind === "activity") {
                    return (
                      <div key={row.id} className="msg activity">
                        <div className="bubble">{row.content}</div>
                        <RowTime ts={row.ts} lang={lang} />
                      </div>
                    );
                  }
                  if (row.kind === "user") {
                    return (
                      <div key={row.id} className="msg user fade-in">
                        <span className="who">you</span>
                        <div className="bubble">{row.content}</div>
                        <RowTime ts={row.ts} lang={lang} />
                      </div>
                    );
                  }
                  if (row.kind === "tool") {
                    return (
                      <div key={row.id} className="msg tool fade-in">
                        <div className="bubble">{row.content}</div>
                        <RowTime ts={row.ts} lang={lang} />
                      </div>
                    );
                  }
                  // assistant — full Markdown document (红线: never plain text).
                  return (
                    <div key={row.id} className="msg agent fade-in">
                      <span className="who">{who}</span>
                      <div className="bubble md">
                        {row.content ? <Markdown content={row.content} /> : null}
                        <OutboundAttachments
                          project={session?.project}
                          attachments={row.attachments}
                        />
                      </div>
                      <RowTime ts={row.ts} lang={lang} />
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
              {scheduled.length > 0 ? (
                <div className="scheduled-queue" data-testid="scheduled-queue">
                  <div className="scheduled-head">
                    <Clock />
                    <span>{t("scheduleQueue")}</span>
                  </div>
                  {scheduled.map((item) => (
                    <div key={item.id} className={`scheduled-row ${item.status}`}>
                      <time
                        dateTime={item.send_at}
                        title={`${t("scheduleLocalTime")}: ${item.send_at}`}
                      >
                        {new Date(item.send_at).toLocaleString(lang === "en" ? "en-US" : "zh-CN", {
                          month: "2-digit",
                          day: "2-digit",
                          hour: "2-digit",
                          minute: "2-digit",
                        })}
                      </time>
                      <span className="scheduled-preview" title={item.text}>
                        {item.text.replace(/\s+/g, " ").trim().slice(0, 80)}
                      </span>
                      {item.status === "failed" ? (
                        <span className="scheduled-error" title={item.fail_reason ?? ""}>
                          {t("scheduleFailed")}
                        </span>
                      ) : null}
                      <button
                        type="button"
                        aria-label={`${t("scheduleCancel")} ${item.id}`}
                        title={`${t("scheduleCancel")} ${item.id}`}
                        onClick={() => removeScheduled(item.id)}
                      >
                        <X />
                      </button>
                    </div>
                  ))}
                </div>
              ) : null}
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
                modelLabel={statusModel ?? ""}
                effortLabel={statusEffort ?? ""}
                uploadSlug={session?.project}
                onSchedule={scheduleText}
                scheduleTimezone={daemonTimezone}
              />
            </div>
          </div>
        </>
      )}
    </section>
  );
}
