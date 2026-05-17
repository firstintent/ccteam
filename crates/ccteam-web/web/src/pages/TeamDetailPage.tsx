// V0.5.0 F96 — `/teams/:name` detail page.
//
// Header (name + member count + cost) plus 3 stacked panels:
// Topology / TaskBoard / Mailbox. Each panel is independently
// data-driven so a fetch failure on one doesn't blank out the others.
//
// SSE wiring: subscribes to `/api/v1/teams/{name}/events`. Each F95
// event variant updates the relevant cached slice:
//   - team_member_joined / team_member_left → refetch config
//   - team_message_sent → refetch inbox (last 100)
//   - team_task_created / team_task_completed → refetch tasks
//
// We refetch (not patch) because the volume is low (a few events per
// minute) and the wire payload may be a truncated preview; the API
// route reads fresh files. Keeps the SPA simple and the data
// authoritative.
//
// SSE disconnect → header banner "reconnecting…" with exponential
// backoff matching `useProgressStream` (1s → 30s, 7 retries).

import { useCallback, useEffect, useRef, useState } from "react";
import { useParams } from "react-router-dom";
import {
  fetchTeamDetail,
  fetchTeamInbox,
  fetchTeamTasks,
  teamEventsUrl,
  type InboxMessage,
  type TeamDetailResponse,
  type TeamTask,
} from "../lib/teamsApi";
import { TeamTopology } from "../panels/TeamTopology";
import { TaskBoard } from "../panels/TaskBoard";
import { MailboxStream } from "../panels/MailboxStream";

const IDLE_WINDOW_MS = 30_000;
const RETRY_BASE_MS = 1000;
const RETRY_CAP_MS = 30_000;
const MAX_RETRIES = 7;

interface State {
  detail: TeamDetailResponse | null;
  tasks: TeamTask[];
  inbox: InboxMessage[];
  error: string | null;
}

export default function TeamDetailPage() {
  const { name = "" } = useParams<{ name: string }>();
  const [state, setState] = useState<State>({
    detail: null,
    tasks: [],
    inbox: [],
    error: null,
  });
  const [connected, setConnected] = useState(false);
  const [idleSet, setIdleSet] = useState<Set<string>>(new Set());

  const refetchDetail = useCallback(async () => {
    try {
      const d = await fetchTeamDetail(name);
      setState((prev) => ({ ...prev, detail: d, error: null }));
    } catch (err) {
      setState((prev) => ({
        ...prev,
        error: err instanceof Error ? err.message : String(err),
      }));
    }
  }, [name]);

  const refetchTasks = useCallback(async () => {
    try {
      const t = await fetchTeamTasks(name);
      setState((prev) => ({ ...prev, tasks: t }));
    } catch {
      // fall through — error shown via header banner if detail also failed
    }
  }, [name]);

  const refetchInbox = useCallback(async () => {
    try {
      const i = await fetchTeamInbox(name);
      setState((prev) => ({ ...prev, inbox: i }));
    } catch {
      // ditto
    }
  }, [name]);

  useEffect(() => {
    if (!name) return;
    void refetchDetail();
    void refetchTasks();
    void refetchInbox();
  }, [name, refetchDetail, refetchTasks, refetchInbox]);

  // SSE — exponential-backoff EventSource on /api/v1/teams/<name>/events.
  const esRef = useRef<EventSource | null>(null);
  const retryRef = useRef(0);
  const retryTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => {
    if (!name) return;
    let cancelled = false;
    const connect = () => {
      if (cancelled) return;
      const es = new EventSource(teamEventsUrl(name));
      esRef.current = es;
      es.addEventListener("open", () => {
        if (cancelled) return;
        retryRef.current = 0;
        setConnected(true);
      });
      es.addEventListener("progress", (ev) => {
        if (cancelled) return;
        let payload: Record<string, unknown> = {};
        try {
          payload = JSON.parse((ev as MessageEvent).data);
        } catch {
          return;
        }
        const eventName =
          typeof payload.event === "string" ? payload.event : "";
        switch (eventName) {
          case "team_member_joined":
          case "team_member_left":
            void refetchDetail();
            break;
          case "team_task_created":
          case "team_task_completed":
            void refetchTasks();
            break;
          case "team_message_sent":
            void refetchInbox();
            break;
          case "team_teammate_idle": {
            const who =
              typeof payload.teammate_name === "string"
                ? payload.teammate_name
                : "";
            if (who) {
              setIdleSet((prev) => {
                const next = new Set(prev);
                next.add(who);
                return next;
              });
              setTimeout(() => {
                setIdleSet((prev) => {
                  const next = new Set(prev);
                  next.delete(who);
                  return next;
                });
              }, IDLE_WINDOW_MS);
            }
            break;
          }
        }
      });
      const onError = () => {
        if (cancelled) return;
        es.close();
        setConnected(false);
        if (retryRef.current >= MAX_RETRIES) return;
        retryRef.current += 1;
        const delay = Math.min(
          RETRY_CAP_MS,
          RETRY_BASE_MS * 2 ** (retryRef.current - 1),
        );
        retryTimerRef.current = setTimeout(connect, delay);
      };
      es.addEventListener("error", onError);
    };
    connect();
    return () => {
      cancelled = true;
      if (retryTimerRef.current) clearTimeout(retryTimerRef.current);
      retryTimerRef.current = null;
      esRef.current?.close();
      esRef.current = null;
      retryRef.current = 0;
      setConnected(false);
    };
  }, [name, refetchDetail, refetchTasks, refetchInbox]);

  if (!name) {
    return (
      <div className="p-4 text-xs text-text-dim font-mono">
        no team name in URL
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full min-h-0 overflow-auto">
      <header
        data-testid="team-header"
        className="px-4 py-3 border-b border-surface-700/40 flex flex-wrap items-center gap-3"
      >
        <h2 className="font-mono text-sm text-text-primary">
          team: <span className="text-accent-600">{name}</span>
        </h2>
        {state.detail && (
          <>
            <span className="text-[11px] text-text-dim font-mono">
              members: {state.detail.config.members.length}
            </span>
            <span className="text-[11px] text-text-dim font-mono">
              tasks: {state.detail.task_count.pending +
                state.detail.task_count.in_progress +
                state.detail.task_count.completed}
            </span>
          </>
        )}
        <span
          data-testid="sse-status"
          className={`text-[10px] font-mono px-1.5 py-0.5 rounded ${
            connected
              ? "bg-emerald-500/15 text-emerald-300"
              : "bg-amber-500/15 text-amber-300"
          }`}
        >
          {connected ? "live" : "reconnecting…"}
        </span>
        {state.error && (
          <span className="text-[10px] font-mono text-status-error">
            {state.error}
          </span>
        )}
      </header>
      <section
        data-testid="topology-section"
        className="border-b border-surface-700/40"
      >
        {state.detail ? (
          <TeamTopology
            config={state.detail.config}
            idleTeammates={idleSet}
            recentMessages={state.detail.recent_messages}
          />
        ) : (
          <div className="p-4 text-xs text-text-dim font-mono">loading…</div>
        )}
      </section>
      <section
        data-testid="taskboard-section"
        className="border-b border-surface-700/40"
      >
        <TaskBoard tasks={state.tasks} config={state.detail?.config ?? null} />
      </section>
      <section data-testid="mailbox-section">
        <MailboxStream
          messages={state.inbox}
          config={state.detail?.config ?? null}
        />
      </section>
    </div>
  );
}
