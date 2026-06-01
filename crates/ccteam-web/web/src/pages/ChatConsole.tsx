import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { MessageSquare, Plus, RefreshCw, Send, Terminal } from "lucide-react";
import { TerminalView } from "../components/TerminalView";
import { CHAT_SUBPROTOCOL, chatUrlFor } from "../lib/terminalConfig";

type SessionItem = {
  project: string;
  session: string | null;
  vendor: string | null;
  current: boolean;
};

type ServerFrame =
  | { type: "turn_started"; session: string; vendor: string }
  | { type: "assistant_delta"; text: string }
  | { type: "tool"; name: string; summary: string }
  | { type: "reply"; content: string }
  | { type: "turn_done"; session: string }
  | { type: "sessions"; items: SessionItem[] }
  | { type: "lag"; behind: number };

type ClientFrame =
  | { type: "text"; content: string; id?: string }
  | { type: "switch"; project?: string; session?: string };

type TranscriptRow = {
  id: string;
  role: "user" | "assistant" | "tool" | "system";
  content: string;
};

const WEB_CHAT_ID = "web-chat";
const WEB_USER_ID = "web-user";

function nextId(prefix: string) {
  return `${prefix}-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

export default function ChatConsole() {
  const wsRef = useRef<WebSocket | null>(null);
  const [connected, setConnected] = useState(false);
  const [sessions, setSessions] = useState<SessionItem[]>([]);
  const [selectedProject, setSelectedProject] = useState<string | null>(null);
  const [selectedSession, setSelectedSession] = useState<string | null>(null);
  const [rows, setRows] = useState<TranscriptRow[]>([]);
  const [draft, setDraft] = useState("");
  const [view, setView] = useState<"chat" | "terminal">("chat");

  const projects = useMemo(
    () => Array.from(new Set(sessions.map((item) => item.project))).sort(),
    [sessions],
  );
  const visibleSessions = useMemo(
    () => sessions.filter((item) => item.project === selectedProject),
    [selectedProject, sessions],
  );

  const appendRow = useCallback((row: Omit<TranscriptRow, "id">) => {
    setRows((current) => [...current, { ...row, id: nextId(row.role) }]);
  }, []);

  const sendFrame = useCallback((frame: ClientFrame) => {
    const ws = wsRef.current;
    if (ws?.readyState !== WebSocket.OPEN) return false;
    ws.send(JSON.stringify(frame));
    return true;
  }, []);

  const applySessions = useCallback(
    (items: SessionItem[]) => {
      setSessions(items);
      setSelectedProject((current) => current ?? items[0]?.project ?? null);
      setSelectedSession((current) => {
        if (current && items.some((item) => item.session === current)) return current;
        return items.find((item) => item.project === selectedProject)?.session ?? null;
      });
    },
    [selectedProject],
  );

  useEffect(() => {
    let closed = false;
    const ws = new WebSocket(chatUrlFor(WEB_CHAT_ID, WEB_USER_ID), [CHAT_SUBPROTOCOL]);
    wsRef.current = ws;
    ws.onopen = () => {
      if (!closed) setConnected(true);
    };
    ws.onclose = () => {
      if (!closed) setConnected(false);
    };
    ws.onerror = () => {
      if (!closed) setConnected(false);
    };
    ws.onmessage = (event) => {
      if (typeof event.data !== "string") return;
      const frame = JSON.parse(event.data) as ServerFrame;
      switch (frame.type) {
        case "sessions":
          applySessions(frame.items);
          break;
        case "reply":
          appendRow({ role: "assistant", content: frame.content });
          break;
        case "assistant_delta":
          appendRow({ role: "assistant", content: frame.text });
          break;
        case "tool":
          appendRow({ role: "tool", content: `${frame.name}: ${frame.summary}` });
          break;
        case "turn_started":
          appendRow({
            role: "system",
            content: `${frame.vendor} ${frame.session} started`,
          });
          break;
        case "turn_done":
          appendRow({ role: "system", content: `${frame.session} done` });
          break;
        case "lag":
          appendRow({ role: "system", content: `Lagged ${frame.behind} frames` });
          break;
      }
    };
    return () => {
      closed = true;
      ws.close();
    };
  }, [appendRow, applySessions]);

  const switchTo = useCallback(
    (project: string, session: string | null) => {
      setSelectedProject(project);
      setSelectedSession(session);
      sendFrame({
        type: "switch",
        project,
        ...(session ? { session } : {}),
      });
    },
    [sendFrame],
  );

  const submit = useCallback(() => {
    const content = draft.trim();
    if (!content) return;
    if (sendFrame({ type: "text", content, id: nextId("client") })) {
      appendRow({ role: "user", content });
      setDraft("");
    }
  }, [appendRow, draft, sendFrame]);

  const createSession = useCallback(() => {
    const content = "/new claude assistant";
    if (sendFrame({ type: "text", content, id: nextId("client") })) {
      appendRow({ role: "user", content });
    }
  }, [appendRow, sendFrame]);

  const terminalProject = selectedProject ?? projects[0] ?? null;

  return (
    <div className="h-full min-h-0 flex flex-col bg-surface-900">
      <div className="h-11 shrink-0 border-b border-surface-700/40 px-4 flex items-center justify-between">
        <div className="flex items-center gap-2 min-w-0">
          <MessageSquare className="h-4 w-4 text-amber-400 shrink-0" />
          <span className="text-sm font-medium truncate">Web Chat</span>
          <span
            className={`h-2 w-2 rounded-full ${connected ? "bg-green-400" : "bg-zinc-500"}`}
          />
        </div>
        <div className="flex items-center gap-1 rounded-md bg-surface-800 p-0.5">
          <button
            type="button"
            onClick={() => setView("chat")}
            className={`h-7 px-2 rounded text-xs flex items-center gap-1 ${
              view === "chat" ? "bg-surface-700 text-text-primary" : "text-text-dim"
            }`}
          >
            <MessageSquare className="h-3.5 w-3.5" />
            Chat
          </button>
          <button
            type="button"
            onClick={() => setView("terminal")}
            className={`h-7 px-2 rounded text-xs flex items-center gap-1 ${
              view === "terminal" ? "bg-surface-700 text-text-primary" : "text-text-dim"
            }`}
          >
            <Terminal className="h-3.5 w-3.5" />
            Terminal
          </button>
        </div>
      </div>

      <div className="flex flex-1 min-h-0">
        <aside className="w-64 shrink-0 border-r border-surface-700/40 p-3 overflow-y-auto">
          <div className="flex items-center justify-between mb-3">
            <div className="text-xs font-mono uppercase text-text-dim">Sessions</div>
            <button
              type="button"
              onClick={createSession}
              className="h-7 w-7 rounded-md bg-surface-800 hover:bg-surface-700 grid place-items-center"
              title="New session"
            >
              <Plus className="h-4 w-4" />
            </button>
          </div>
          <div className="space-y-3">
            {projects.map((project) => (
              <div key={project}>
                <button
                  type="button"
                  onClick={() => switchTo(project, null)}
                  className={`w-full text-left px-2 py-1.5 rounded text-sm ${
                    selectedProject === project && !selectedSession
                      ? "bg-surface-800 text-text-primary"
                      : "text-text-secondary hover:bg-surface-800/70"
                  }`}
                >
                  {project}
                </button>
                <div className="mt-1 space-y-1">
                  {sessions
                    .filter((item) => item.project === project && item.session)
                    .map((item) => (
                      <button
                        key={`${item.project}:${item.session}`}
                        type="button"
                        onClick={() => switchTo(item.project, item.session)}
                        className={`w-full text-left px-3 py-1 rounded text-xs ${
                          selectedSession === item.session
                            ? "bg-surface-700 text-text-primary"
                            : "text-text-dim hover:bg-surface-800/70"
                        }`}
                      >
                        {item.session}
                        {item.vendor ? (
                          <span className="ml-2 text-[10px] uppercase text-text-dim">
                            {item.vendor}
                          </span>
                        ) : null}
                      </button>
                    ))}
                </div>
              </div>
            ))}
            {projects.length === 0 ? (
              <div className="text-xs text-text-dim leading-5">No sessions reported yet.</div>
            ) : null}
          </div>
        </aside>

        <main className="flex-1 min-w-0 min-h-0">
          {view === "terminal" ? (
            terminalProject ? (
              <TerminalView
                slug={terminalProject}
                sid={selectedSession ?? undefined}
                className="h-full"
              />
            ) : (
              <div className="h-full grid place-items-center text-sm text-text-dim">
                Select a project before opening the terminal.
              </div>
            )
          ) : (
            <div className="h-full min-h-0 flex flex-col">
              <div className="flex-1 min-h-0 overflow-y-auto p-4 space-y-3">
                {rows.map((row) => (
                  <div
                    key={row.id}
                    className={`max-w-[760px] rounded-md px-3 py-2 text-sm leading-6 ${
                      row.role === "user"
                        ? "ml-auto bg-amber-500/15 border border-amber-500/20"
                        : row.role === "tool"
                          ? "bg-surface-800/70 border border-surface-700/50 text-text-secondary"
                          : row.role === "system"
                            ? "bg-transparent text-xs text-text-dim"
                            : "bg-surface-800 border border-surface-700/40"
                    }`}
                  >
                    {row.content}
                  </div>
                ))}
                {rows.length === 0 ? (
                  <div className="h-full grid place-items-center text-sm text-text-dim">
                    Messages will appear here.
                  </div>
                ) : null}
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
                    className="min-h-11 max-h-32 flex-1 resize-y rounded-md bg-surface-800 border border-surface-700 px-3 py-2 text-sm outline-none focus:border-amber-500"
                    placeholder="@assistant fix the failing test"
                  />
                  <button
                    type="button"
                    onClick={submit}
                    className="h-11 w-11 shrink-0 rounded-md bg-amber-500 text-surface-950 hover:bg-amber-400 grid place-items-center"
                    title="Send"
                  >
                    {connected ? <Send className="h-4 w-4" /> : <RefreshCw className="h-4 w-4" />}
                  </button>
                </div>
                {visibleSessions.length > 0 ? (
                  <div className="mt-2 text-xs text-text-dim">
                    {visibleSessions.length} session{visibleSessions.length === 1 ? "" : "s"} in{" "}
                    {selectedProject}
                  </div>
                ) : null}
              </div>
            </div>
          )}
        </main>
      </div>
    </div>
  );
}
