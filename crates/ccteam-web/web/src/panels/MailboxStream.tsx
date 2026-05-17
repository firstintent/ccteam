// V0.5.0 F96 — Mailbox Stream panel.
//
// Timeline of every message in the team's inboxes/, merged + sorted.
// Renders:
//   - from → to per row, sender's color
//   - read: false rows get a left-border highlight (PRD §F96.3)
//   - filter by teammate (from OR to)
//   - text search (case-insensitive substring)
//   - time asc / desc toggle
//   - idle_notification messages are HIDDEN (PRD §F96 3.分流)
//
// Read state is read-only — the panel never POSTs back to Anthropic
// (red line). Marking as read requires native Claude Code attach.

import { memo, useMemo, useState } from "react";
import type { InboxMessage, TeamConfig } from "../lib/teamsApi";
import { colorClasses } from "../lib/teamsApi";

interface Props {
  messages: InboxMessage[];
  config: TeamConfig | null;
}

export const MailboxStream = memo(function MailboxStream({
  messages,
  config,
}: Props) {
  const [filter, setFilter] = useState<string>("");
  const [search, setSearch] = useState<string>("");
  const [order, setOrder] = useState<"asc" | "desc">("desc");

  const teammates = useMemo(() => {
    if (!config) return [];
    return config.members.map((m) => m.name).sort();
  }, [config]);

  const visible = useMemo(() => {
    const out = filterMessages(messages, { teammate: filter, search });
    return order === "desc" ? out.slice().reverse() : out;
  }, [messages, filter, search, order]);

  const unread = useMemo(
    () => messages.filter((m) => !m.read && !m.is_idle_notification).length,
    [messages],
  );

  return (
    <div data-testid="mailbox-panel" className="flex flex-col gap-2 p-4 min-h-0">
      <header className="flex flex-wrap items-center gap-2">
        <h3 className="font-mono text-xs uppercase tracking-wider text-text-secondary">
          Mailbox
        </h3>
        <span
          data-testid="unread-badge"
          className="text-[10px] font-mono px-1.5 py-0.5 rounded bg-rose-500/15 text-rose-300 border border-rose-400/30"
        >
          {unread} unread
        </span>
        <div className="flex-1" />
        <select
          data-testid="mailbox-filter"
          value={filter}
          onChange={(e) => setFilter(e.target.value)}
          className="bg-surface-800 text-text-secondary text-xs font-mono border border-surface-700/40 rounded px-2 py-1"
        >
          <option value="">all teammates</option>
          {teammates.map((t) => (
            <option key={t} value={t}>
              {t}
            </option>
          ))}
        </select>
        <input
          data-testid="mailbox-search"
          type="search"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="search"
          className="bg-surface-800 text-text-primary text-xs font-mono border border-surface-700/40 rounded px-2 py-1 w-32"
        />
        <button
          type="button"
          data-testid="mailbox-order"
          onClick={() => setOrder((o) => (o === "asc" ? "desc" : "asc"))}
          className="text-xs font-mono px-2 py-1 rounded border border-surface-700/40 text-text-secondary hover:bg-surface-800 cursor-pointer"
        >
          {order === "desc" ? "newest first" : "oldest first"}
        </button>
      </header>
      <div className="flex flex-col gap-1 overflow-y-auto">
        {visible.length === 0 ? (
          <p className="text-[11px] text-text-dim italic">no messages match</p>
        ) : (
          visible.map((m, i) => (
            <MessageRow key={`${m.timestamp}-${m.from}-${i}`} msg={m} />
          ))
        )}
      </div>
    </div>
  );
});

interface RowProps {
  msg: InboxMessage;
}

function MessageRow({ msg }: RowProps) {
  const colors = colorClasses(msg.color);
  const isUnread = !msg.read;
  return (
    <div
      data-testid="mailbox-row"
      data-unread={isUnread ? "true" : "false"}
      className={`rounded p-2 bg-surface-800/40 border-l-2 ${
        isUnread ? "border-l-rose-400" : "border-l-surface-700/30"
      } flex flex-col gap-0.5`}
    >
      <div className="flex items-center gap-1 text-[11px] font-mono">
        <span
          className={`shrink-0 w-4 h-4 rounded-full flex items-center justify-center text-[9px] ${colors.bg} ${colors.text}`}
        >
          {msg.from.slice(0, 1).toUpperCase()}
        </span>
        <span className="text-text-primary truncate">{msg.from}</span>
        <span className="text-text-dim">→</span>
        <span className="text-text-secondary truncate">{msg.to}</span>
        <span className="flex-1" />
        <span className="text-text-dim">{msg.timestamp}</span>
      </div>
      {msg.summary && (
        <div className="text-[11px] text-text-secondary italic">{msg.summary}</div>
      )}
      <div className="text-[11px] text-text-primary whitespace-pre-wrap break-words">
        {msg.text}
      </div>
    </div>
  );
}

/** Pure filter helper — unit-testable without React. Skips
 *  idle_notification messages, then narrows by teammate (from/to)
 *  + case-insensitive substring search over text + summary. */
export function filterMessages(
  msgs: InboxMessage[],
  opts: { teammate?: string; search?: string },
): InboxMessage[] {
  const teammate = opts.teammate?.trim() ?? "";
  const search = (opts.search ?? "").trim().toLowerCase();
  return msgs.filter((m) => {
    if (m.is_idle_notification) return false;
    if (teammate && m.from !== teammate && m.to !== teammate) return false;
    if (search) {
      const hay =
        `${m.text}\n${m.summary ?? ""}\n${m.from}\n${m.to}`.toLowerCase();
      if (!hay.includes(search)) return false;
    }
    return true;
  });
}
