// The per-session CONTEXT panel — a right sidebar docked beside the chat in
// the conversation view (SessionView renders it inside `.conv-body`, next to
// `.conv-main`; ≤820px it becomes a right-edge overlay drawer via CSS). It
// shows what the session's context window is made of, mechanically, from
// `GET /api/v1/sessions/{sid}/context` — no model is asked.
//
// Lifecycle: mounted only while open (SessionView owns the open flag and
// remembers it per browser), so mount = fetch. Re-fetches when a turn ends on
// this session (`turnEnds` increments off the page's own SSE fold — no polling
// loop) and on the Refresh button (spinner + inline failure, never a silent
// no-op). Escape inside the panel closes it; focus moves into the panel only
// when the user opened it (not when the docked state is restored on load).
//
// Honest states: `live:false` is labelled a snapshot with its age;
// `breakdown:null` explains which sessions have no breakdown instead of
// erroring; every sub-section renders only when the server sent it.

import { Fragment, useCallback, useEffect, useRef, useState, type KeyboardEvent } from "react";
import { ChevronDown, ChevronRight, RefreshCw, X } from "lucide-react";
import {
  getSessionContext,
  type ContextBreakdown,
  type ContextCategory,
  type ContextItem,
  type ContextLastRequest,
  type ContextMessages,
  type SessionContextResponse,
} from "../lib/contextApi";
import {
  attachmentRows,
  barSegments,
  cacheHitRatio,
  categoryLabel,
  categorySlot,
  formatPct,
  formatTokensCompact,
  formatTokensExact,
  hasItems,
  messageSplit,
  messagesTotal,
  pctOf,
  relativeTime,
  sortedItems,
  splitCategories,
  tableRows,
  thresholdPct,
  toolRows,
  truncateMiddle,
  usedPct,
  type MessageSplitKey,
} from "../lib/contextBreakdown";
import { makeT, type Lang } from "../lib/i18n";

type T = (key: string) => string;

export interface ContextPanelProps {
  sid: string;
  lang: Lang;
  onClose: () => void;
  /** Identity of the newest turn-ending frame on this session; every change
   *  re-fetches the breakdown (the window changes at turn edges). */
  refreshKey?: string | number | null;
  /** Move keyboard focus into the panel on mount — true when the user just
   *  opened it, false when the docked state was restored from storage. */
  focusOnMount?: boolean;
}

export function ContextPanel({ sid, lang, onClose, refreshKey, focusOnMount = false }: ContextPanelProps) {
  const t = makeT(lang);
  const [data, setData] = useState<SessionContextResponse | null>(null);
  // Mount = a fetch is already in flight, so the first paint is the loading
  // state; later fetches set this from their own event handler.
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [fetchedAt, setFetchedAt] = useState(0);
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const requestRef = useRef(0);
  const panelRef = useRef<HTMLElement>(null);

  // One fetch path for mount, turn ends and the Refresh button. A superseded
  // request (newer one started) never writes; a failure is SHOWN next to
  // whatever data is already on screen.
  const load = useCallback(() => {
    const request = ++requestRef.current;
    getSessionContext(sid)
      .then((response) => {
        if (request !== requestRef.current) return;
        setData(response);
        setFetchedAt(Date.now());
        setError(null);
        setLoading(false);
      })
      .catch((e) => {
        if (request !== requestRef.current) return;
        setError(e instanceof Error ? e.message : String(e));
        setLoading(false);
      });
  }, [sid]);

  useEffect(() => {
    load();
  }, [load, refreshKey]);

  // Abandon any in-flight request on unmount.
  useEffect(
    () => () => {
      requestRef.current += 1;
    },
    [],
  );

  useEffect(() => {
    if (focusOnMount) panelRef.current?.focus();
  }, [focusOnMount]);

  const refresh = useCallback(() => {
    setLoading(true);
    load();
  }, [load]);

  const onKeyDown = useCallback(
    (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      onClose();
    },
    [onClose],
  );

  const toggle = useCallback((id: string) => {
    setExpanded((current) => ({ ...current, [id]: !current[id] }));
  }, []);

  return (
    <>
      <button
        type="button"
        className="ctx-backdrop"
        aria-label={t("ctxClosePanel")}
        data-testid="ctx-backdrop"
        onClick={onClose}
      />
      <aside
        ref={panelRef}
        className="ctx-panel"
        role="complementary"
        aria-label={t("ctxPanelTitle")}
        tabIndex={-1}
        data-testid="context-panel"
        onKeyDown={onKeyDown}
      >
        <div className="ctx-head">
          <span className="ctx-title">{t("ctxPanelTitle")}</span>
          <button
            type="button"
            className="icon-btn sm"
            aria-label={loading ? t("ctxRefreshing") : t("ctxRefresh")}
            title={t("ctxRefresh")}
            aria-busy={loading}
            disabled={loading}
            data-testid="ctx-refresh"
            onClick={refresh}
          >
            <RefreshCw className={loading ? "ctx-spin" : undefined} />
          </button>
          <button
            type="button"
            className="icon-btn sm"
            aria-label={t("ctxClosePanel")}
            title={t("ctxClosePanel")}
            data-testid="ctx-close"
            onClick={onClose}
          >
            <X />
          </button>
        </div>
        <div className="ctx-body">
          {error ? (
            <div className="ctx-error" role="alert" data-testid="ctx-error">
              <span>
                {t("ctxLoadFailed")}: {error}
              </span>
              <button type="button" className="btn ghost mini" onClick={refresh} disabled={loading}>
                {t("retry")}
              </button>
            </div>
          ) : null}
          {loading && !data && !error ? (
            <div className="ctx-loading" data-testid="ctx-loading">
              {t("ctxLoading")}
            </div>
          ) : null}
          {data ? (
            data.breakdown ? (
              <Breakdown
                breakdown={data.breakdown}
                live={data.live}
                fetchedAt={fetchedAt}
                lang={lang}
                t={t}
                expanded={expanded}
                onToggle={toggle}
              />
            ) : (
              <div className="ctx-empty" role="status" data-testid="ctx-none">
                <p>{t("ctxNoBreakdown")}</p>
                <p className="ctx-note">{t("ctxNoBreakdownWhy")}</p>
              </div>
            )
          ) : null}
        </div>
      </aside>
    </>
  );
}

function Breakdown({
  breakdown,
  live,
  fetchedAt,
  lang,
  t,
  expanded,
  onToggle,
}: {
  breakdown: ContextBreakdown;
  live: boolean;
  fetchedAt: number;
  lang: Lang;
  t: T;
  expanded: Record<string, boolean>;
  onToggle: (id: string) => void;
}) {
  const { deferred } = splitCategories(breakdown);
  const rows = tableRows(breakdown);
  return (
    <>
      <Header breakdown={breakdown} live={live} fetchedAt={fetchedAt} lang={lang} t={t} />
      <WindowBar breakdown={breakdown} lang={lang} t={t} />
      {rows.length > 0 ? (
        <section className="ctx-sec">
          <h4>{t("ctxCategories")}</h4>
          <CategoryTable
            rows={rows}
            windowTokens={breakdown.window_tokens}
            counted
            lang={lang}
            t={t}
            expanded={expanded}
            onToggle={onToggle}
            testId="ctx-categories"
          />
        </section>
      ) : null}
      {deferred.length > 0 ? (
        <section className="ctx-sec ctx-muted" data-testid="ctx-deferred">
          <h4>{t("ctxDeferred")}</h4>
          <p className="ctx-note">{t("ctxDeferredNote")}</p>
          <CategoryTable
            rows={deferred}
            windowTokens={breakdown.window_tokens}
            counted={false}
            lang={lang}
            t={t}
            expanded={expanded}
            onToggle={onToggle}
            testId="ctx-deferred-table"
          />
        </section>
      ) : null}
      {breakdown.messages ? <MessagesSection messages={breakdown.messages} t={t} /> : null}
      {breakdown.last_request ? <LastRequest last={breakdown.last_request} t={t} /> : null}
    </>
  );
}

function Header({
  breakdown,
  live,
  fetchedAt,
  lang,
  t,
}: {
  breakdown: ContextBreakdown;
  live: boolean;
  fetchedAt: number;
  lang: Lang;
  t: T;
}) {
  const pct = usedPct(breakdown);
  const locale = lang === "en" ? "en-US" : "zh-CN";
  const captured = breakdown.captured_at ? new Date(breakdown.captured_at) : null;
  const capturedText =
    captured && !Number.isNaN(captured.getTime()) ? captured.toLocaleString(locale) : null;
  const age = live ? null : relativeTime(breakdown.captured_at, fetchedAt, lang);
  return (
    <div className="ctx-meta" data-testid="ctx-meta">
      {breakdown.model ? (
        <span className="chip" title={t("ctxModel")} data-testid="ctx-model">
          {breakdown.model}
        </span>
      ) : null}
      <span
        className="ctx-usage"
        data-testid="ctx-usage"
        title={`${formatTokensExact(breakdown.used_tokens)} / ${formatTokensExact(breakdown.window_tokens)} ${t("ctxTokens")}`}
      >
        {formatTokensCompact(breakdown.used_tokens)} / {formatTokensCompact(breakdown.window_tokens)}
        {pct !== null ? ` · ${formatPct(pct)}` : ""}
      </span>
      {live ? (
        <span className="ctx-badge live" data-testid="ctx-live">
          {t("ctxLive")}
        </span>
      ) : (
        <span
          className="ctx-badge snapshot"
          data-testid="ctx-snapshot"
          title={capturedText ? `${t("ctxCapturedAt")} ${capturedText}` : undefined}
        >
          {t("ctxSnapshot")}
          {age ? ` · ${age}` : ""}
        </span>
      )}
    </div>
  );
}

function WindowBar({ breakdown, lang, t }: { breakdown: ContextBreakdown; lang: Lang; t: T }) {
  const segments = barSegments(breakdown);
  if (segments.length === 0) return null;
  const threshold = thresholdPct(breakdown);
  return (
    <div className="ctx-meter-wrap">
      <div className="ctx-meter" role="img" aria-label={t("ctxBarLabel")} data-testid="ctx-meter">
        {segments.map((segment) => {
          const label = categoryLabel(segment, lang);
          const style =
            segment.kind === "free"
              ? undefined
              : {
                  flexBasis: `${segment.pct}%`,
                  ...(segment.kind === "used" && segment.slot
                    ? { background: `var(--ctx-${segment.slot})` }
                    : {}),
                };
          return (
            <span
              key={segment.id}
              className={`ctx-seg ${segment.kind}${segment.kind === "used" && !segment.slot ? " other" : ""}`}
              data-testid={`ctx-seg-${segment.id}`}
              data-kind={segment.kind}
              style={style}
              title={`${label} · ${formatTokensExact(segment.tokens)} · ${formatPct(segment.pct)}`}
            />
          );
        })}
      </div>
      {threshold !== null ? (
        <span
          className="ctx-threshold"
          style={{ left: `${threshold}%` }}
          data-testid="ctx-threshold"
          title={`${t("ctxAutoCompactAt")} ${formatTokensCompact(breakdown.auto_compact_threshold)}`}
        />
      ) : null}
    </div>
  );
}

function Swatch({ slot, kind }: { slot: number | null; kind: string }) {
  if (kind === "buffer") return <span className="ctx-sw buffer" aria-hidden />;
  if (kind === "free") return <span className="ctx-sw free" aria-hidden />;
  if (kind !== "used") return <span className="ctx-sw none" aria-hidden />;
  return (
    <span
      className={`ctx-sw${slot ? "" : " other"}`}
      style={slot ? { background: `var(--ctx-${slot})` } : undefined}
      aria-hidden
    />
  );
}

function CategoryTable({
  rows,
  windowTokens,
  counted,
  lang,
  t,
  expanded,
  onToggle,
  testId,
}: {
  rows: ContextCategory[];
  windowTokens: number | null | undefined;
  /** false = these do not occupy the window (deferred): the % column is a dash. */
  counted: boolean;
  lang: Lang;
  t: T;
  expanded: Record<string, boolean>;
  onToggle: (id: string) => void;
  testId: string;
}) {
  return (
    <table className="ctx-table" data-testid={testId}>
      <thead>
        <tr>
          <th>{t("ctxColName")}</th>
          <th className="num">{t("ctxColTokens")}</th>
          <th className="num">{t("ctxColPct")}</th>
        </tr>
      </thead>
      <tbody>
        {rows.map((category) => {
          const expandable = hasItems(category);
          const isOpen = expanded[category.id] === true;
          const slot = category.kind === "used" ? categorySlot(category.id) : null;
          const label = categoryLabel(category, lang);
          const share = counted ? formatPct(pctOf(category.tokens, windowTokens)) : "—";
          return (
            <Fragment key={category.id}>
              <tr className={`ctx-cat ${category.kind}`} data-testid={`ctx-cat-${category.id}`}>
                <td className="name">
                  {expandable ? (
                    <button
                      type="button"
                      className="ctx-row-btn"
                      aria-expanded={isOpen}
                      aria-label={`${isOpen ? t("collapse") : t("expand")} ${label}`}
                      data-testid={`ctx-expand-${category.id}`}
                      onClick={() => onToggle(category.id)}
                    >
                      {isOpen ? <ChevronDown /> : <ChevronRight />}
                      <Swatch slot={slot} kind={category.kind} />
                      <span className="name" title={label}>
                        {label}
                      </span>
                      <span className="ctx-count">{category.items?.length ?? 0}</span>
                    </button>
                  ) : (
                    <span className="ctx-row-static">
                      <Swatch slot={slot} kind={category.kind} />
                      <span className="name" title={label}>
                        {label}
                      </span>
                    </span>
                  )}
                </td>
                <td className="num" title={formatTokensExact(category.tokens)}>
                  {formatTokensCompact(category.tokens)}
                </td>
                <td className="num">{share}</td>
              </tr>
              {expandable && isOpen
                ? sortedItems(category).map((item, index) => (
                    <ItemRow
                      key={`${category.id}:${item.name}:${index}`}
                      item={item}
                      windowTokens={windowTokens}
                      counted={counted}
                      t={t}
                    />
                  ))
                : null}
            </Fragment>
          );
        })}
      </tbody>
    </table>
  );
}

function ItemRow({
  item,
  windowTokens,
  counted,
  t,
}: {
  item: ContextItem;
  windowTokens: number | null | undefined;
  counted: boolean;
  t: T;
}) {
  const notLoaded = item.loaded === false;
  const tooltip = [item.name, item.detail].filter((part): part is string => !!part).join("\n");
  return (
    <tr className={`ctx-item${notLoaded ? " not-loaded" : ""}`} data-testid="ctx-item">
      <td className="name" title={tooltip}>
        <span className="ctx-item-name">{truncateMiddle(item.name)}</span>
        {item.source ? <span className="ctx-tag">{item.source}</span> : null}
        {notLoaded ? (
          <span className="ctx-tag warn" data-testid="ctx-not-loaded">
            {t("ctxNotLoaded")}
          </span>
        ) : null}
        {item.detail ? <div className="ctx-item-detail">{truncateMiddle(item.detail, 56)}</div> : null}
      </td>
      <td className="num" title={formatTokensExact(item.tokens)}>
        {formatTokensCompact(item.tokens)}
      </td>
      <td className="num">{counted && !notLoaded ? formatPct(pctOf(item.tokens, windowTokens)) : "—"}</td>
    </tr>
  );
}

const SPLIT_LABEL_KEY: Record<MessageSplitKey, string> = {
  tool_call: "ctxSplitToolCalls",
  tool_result: "ctxSplitToolResults",
  attachment: "ctxSplitAttachments",
  assistant: "ctxSplitAssistant",
  user: "ctxSplitUser",
  other: "ctxSplitOther",
};

function MessagesSection({ messages, t }: { messages: ContextMessages; t: T }) {
  const split = messageSplit(messages);
  const total = messagesTotal(messages);
  const tools = toolRows(messages);
  const attachments = attachmentRows(messages);
  if (split.length === 0 && tools.length === 0 && attachments.length === 0) return null;
  return (
    <section className="ctx-sec" data-testid="ctx-messages">
      <h4>
        {t("ctxMessages")}
        {total > 0 ? (
          <span className="ctx-h-num" title={formatTokensExact(total)}>
            {formatTokensCompact(total)}
          </span>
        ) : null}
      </h4>
      {split.length > 0 ? (
        <table className="ctx-table" data-testid="ctx-split">
          <tbody>
            {split.map((row) => {
              const share = pctOf(row.tokens, total);
              return (
                <tr key={row.key} data-testid={`ctx-split-${row.key}`}>
                  <td className="name">
                    <span className="name">{t(SPLIT_LABEL_KEY[row.key])}</span>
                    <span className="ctx-mini" aria-hidden>
                      <span style={{ width: `${Math.max(0, Math.min(100, share ?? 0))}%` }} />
                    </span>
                  </td>
                  <td className="num" title={formatTokensExact(row.tokens)}>
                    {formatTokensCompact(row.tokens)}
                  </td>
                  <td className="num">{formatPct(share)}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
      ) : null}
      {tools.length > 0 ? (
        <>
          <h5>{t("ctxTools")}</h5>
          <table className="ctx-table ctx-tools" data-testid="ctx-tools">
            <thead>
              <tr>
                <th>{t("ctxColTool")}</th>
                <th className="num">{t("ctxColCall")}</th>
                <th className="num">{t("ctxColResult")}</th>
                <th className="num">{t("ctxColTotal")}</th>
              </tr>
            </thead>
            <tbody>
              {tools.map((tool) => (
                <tr key={tool.name} data-testid="ctx-tool-row" data-tool={tool.name}>
                  <td className="name mono" title={tool.name}>
                    {tool.name}
                  </td>
                  <td className="num" title={formatTokensExact(tool.call)}>
                    {formatTokensCompact(tool.call)}
                  </td>
                  <td className="num" title={formatTokensExact(tool.result)}>
                    {formatTokensCompact(tool.result)}
                  </td>
                  <td className="num total" title={formatTokensExact(tool.total)}>
                    {formatTokensCompact(tool.total)}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      ) : null}
      {attachments.length > 0 ? (
        <>
          <h5>{t("ctxAttachments")}</h5>
          <table className="ctx-table" data-testid="ctx-attachments">
            <thead>
              <tr>
                <th>{t("ctxColType")}</th>
                <th className="num">{t("ctxColTokens")}</th>
                <th className="num">{t("ctxColOfMessages")}</th>
              </tr>
            </thead>
            <tbody>
              {attachments.map((attachment) => (
                <tr key={attachment.name} data-testid="ctx-attachment-row" data-attachment={attachment.name}>
                  <td className="name mono" title={attachment.name}>
                    {attachment.name}
                  </td>
                  <td className="num" title={formatTokensExact(attachment.tokens)}>
                    {formatTokensCompact(attachment.tokens)}
                  </td>
                  <td className="num">{formatPct(pctOf(attachment.tokens, total))}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      ) : null}
    </section>
  );
}

function LastRequest({ last, t }: { last: ContextLastRequest; t: T }) {
  const ratio = cacheHitRatio(last);
  return (
    <section className="ctx-sec" data-testid="ctx-last-request">
      <h4>{t("ctxLastRequest")}</h4>
      <div className="ctx-cache">
        {ratio !== null ? (
          <div data-testid="ctx-cache-hit">
            {t("ctxCacheHit")} <b>{formatPct(ratio * 100)}</b>
          </div>
        ) : null}
        <div>
          <span title={formatTokensExact(last.cache_read_tokens)}>
            {t("ctxCacheRead")} {formatTokensCompact(last.cache_read_tokens)}
          </span>
          {" · "}
          <span title={formatTokensExact(last.cache_creation_tokens)}>
            {t("ctxCacheWrite")} {formatTokensCompact(last.cache_creation_tokens)}
          </span>
          {" · "}
          <span title={formatTokensExact(last.input_tokens)}>
            {t("ctxUncached")} {formatTokensCompact(last.input_tokens)}
          </span>
          {" · "}
          <span title={formatTokensExact(last.output_tokens)}>
            {t("ctxOutput")} {formatTokensCompact(last.output_tokens)}
          </span>
        </div>
      </div>
    </section>
  );
}
