// v0.8.3 — standalone web chat console.
//
// Re-planned from the embedded dashboard `/chat` panel into a chat-first
// app shell (App.tsx renders this route without the dashboard chrome):
//
//   N1 standalone shell  — own app bar (status + cost link + Dashboard ↗),
//                          no Projects/Teams/Sessions nav.
//   N2 all sessions      — left rail lists every project's sessions
//                          (vendor·role·sid), grouped, active highlighted.
//   N3 new session       — modal picks project + code agent + role and
//                          submits `/cd <p>` + `/new <vendor> <role>`.
//   N4 continuous stream — one timeline; switching focus inserts a marker
//                          instead of clearing; transcript persists in
//                          localStorage so refresh/reconnect keep history.
//   N5 vendor terminal   — the terminal tab is enabled only for a Claude
//                          (tmux) session; Codex (app-server) has no PTY.
//   P1-4 reconnect       — the chat socket auto-reconnects with backoff.
//
// Red lines: the chat view reads structured turn frames (never scrapes a
// pane); the terminal view is the existing `ccteam-pty.v1` byte relay.

import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { Link } from "react-router-dom";
import { MessageSquare, Plus, Send, Terminal } from "lucide-react";
import { TerminalView } from "../components/TerminalView";
import { CHAT_SUBPROTOCOL, chatUrlFor } from "../lib/terminalConfig";

type SessionItem = {
  project: string;
  session: string | null;
  vendor: string | null;
  role: string | null;
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

type RowKind = "user" | "assistant" | "tool" | "system" | "marker";
type TranscriptRow = { id: string; kind: RowKind; content: string; from?: string };

type Focus = {
  project: string;
  session: string | null;
  vendor: string | null;
  role: string | null;
};

type CreateRequest =
  | { kind: "existing"; project: string; vendor: string; role: string }
  | { kind: "new"; slug: string; path: string; vendor: string; role: string };

// All tabs share one chat id so they observe the same conversation; the
// backend broadcasts each outbound reply to every matching socket.
const WEB_CHAT_ID = "web-chat";
const WEB_USER_ID = "web-user";

const ROWS_KEY = "ccteam.chat.rows.v1";
const FOCUS_KEY = "ccteam.chat.focus.v1";
const ROWS_CAP = 400;
const ROLE_SUGGESTIONS = ["assistant", "reviewer", "api", "ui", "qa", "docs"];

function nextId(prefix: string) {
  return `${prefix}-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

function loadRows(): TranscriptRow[] {
  try {
    const parsed = JSON.parse(localStorage.getItem(ROWS_KEY) ?? "[]");
    return Array.isArray(parsed) ? parsed : [];
  } catch {
    return [];
  }
}

function loadFocus(): Focus | null {
  try {
    return JSON.parse(localStorage.getItem(FOCUS_KEY) ?? "null");
  } catch {
    return null;
  }
}

function focusLabel(focus: Focus | null): string | undefined {
  if (!focus) return undefined;
  const parts = [focus.vendor, focus.role].filter(Boolean);
  return parts.length ? parts.join(" · ") : focus.project;
}

function focusOf(item: SessionItem): Focus {
  return {
    project: item.project,
    session: item.session,
    vendor: item.vendor,
    role: item.role,
  };
}

export default function ChatConsole() {
  const wsRef = useRef<WebSocket | null>(null);
  const [connected, setConnected] = useState(false);
  const [reconnectAttempt, setReconnectAttempt] = useState(0);
  const [sessions, setSessions] = useState<SessionItem[]>([]);
  const [rows, setRows] = useState<TranscriptRow[]>(() => {
    const stored = loadRows();
    return stored.length
      ? stored
      : [
          {
            id: nextId("system"),
            kind: "system",
            content: "— 会话开始 · 历史在本机保留,刷新/重连不清屏 —",
          },
        ];
  });
  const [focus, setFocus] = useState<Focus | null>(() => loadFocus());
  const [draft, setDraft] = useState("");
  const [view, setView] = useState<"chat" | "terminal">("chat");
  const [modalOpen, setModalOpen] = useState(false);

  // Refs so the long-lived socket handler reads current values.
  const focusRef = useRef<Focus | null>(focus);
  const pendingCreateRef = useRef<Focus | null>(null);
  // A new-project create in flight: we wait for /newproject to succeed before
  // sending /cd + /new, so a failed scaffold doesn't cascade into a confusing
  // "unknown project" from /cd.
  const pendingNewProjectRef = useRef<{ slug: string; vendor: string; role: string } | null>(null);
  // Lets handleFrame (defined before sendText) send follow-up commands without
  // a definition-order / dependency cycle.
  const sendTextRef = useRef<((content: string, echo: boolean) => boolean) | null>(null);
  useEffect(() => {
    focusRef.current = focus;
  }, [focus]);

  // Persist transcript + focus so a refresh resumes where we left off.
  useEffect(() => {
    try {
      localStorage.setItem(ROWS_KEY, JSON.stringify(rows.slice(-ROWS_CAP)));
    } catch {
      // storage full / disabled — in-memory transcript still works.
    }
  }, [rows]);
  useEffect(() => {
    try {
      localStorage.setItem(FOCUS_KEY, JSON.stringify(focus));
    } catch {
      // ignore
    }
  }, [focus]);

  const pushRow = useCallback((row: Omit<TranscriptRow, "id">) => {
    setRows((current) => [...current, { ...row, id: nextId(row.kind) }]);
  }, []);

  const projects = useMemo(
    () => Array.from(new Set(sessions.map((item) => item.project))).sort(),
    [sessions],
  );
  const roleOptions = useMemo(() => {
    const seen = new Set(ROLE_SUGGESTIONS);
    sessions.forEach((item) => item.role && seen.add(item.role));
    return Array.from(seen);
  }, [sessions]);

  const handleFrame = useCallback(
    (frame: ServerFrame) => {
      switch (frame.type) {
        case "sessions": {
          // Merge frames: the disk frame carries all projects (as markers),
          // the gateway `/sessions` frame carries all chat sessions (cross-
          // entry). Chat sessions (with an id) win and are deduped; a project
          // marker is kept only when that project has no session.
          setSessions((prev) => {
            const merged: SessionItem[] = [];
            const seenSession = new Set<string>();
            for (const item of [...frame.items, ...prev]) {
              if (!item.session) continue;
              const key = `${item.project}:${item.session}`;
              if (seenSession.has(key)) continue;
              seenSession.add(key);
              merged.push(item);
            }
            const withSession = new Set(merged.map((s) => s.project));
            const seenMarker = new Set<string>();
            for (const item of [...frame.items, ...prev]) {
              if (item.session) continue;
              if (withSession.has(item.project) || seenMarker.has(item.project)) continue;
              seenMarker.add(item.project);
              merged.push(item);
            }
            return merged;
          });
          // Adopt a default focus once we know the sessions, but never
          // clobber a focus the user already chose.
          setFocus((current) => {
            if (current) return current;
            const first = frame.items.find((item) => item.session);
            const next = first ? focusOf(first) : null;
            focusRef.current = next;
            return next;
          });
          break;
        }
        case "reply": {
          // New-project sequencing: once the project is scaffolded, cd in +
          // spawn the session. On failure, drop the pending create (the error
          // bubble already shows) so /cd never runs against a missing project.
          const np = pendingNewProjectRef.current;
          if (np) {
            const created = new RegExp(`created project ${np.slug}\\b`).test(frame.content);
            const failed = /^gateway error/.test(frame.content);
            if (created) {
              pendingNewProjectRef.current = null;
              pendingCreateRef.current = {
                project: np.slug,
                session: null,
                vendor: np.vendor,
                role: np.role,
              };
              sendTextRef.current?.(`/cd ${np.slug}`, true);
              sendTextRef.current?.(`/new ${np.vendor} ${np.role}`, true);
              sendTextRef.current?.("/sessions", false);
            } else if (failed) {
              pendingNewProjectRef.current = null;
            }
          }
          const sid = /created session (s\d+)/.exec(frame.content)?.[1];
          const pending = pendingCreateRef.current;
          if (sid && pending) {
            pendingCreateRef.current = null;
            const next: Focus = { ...pending, session: sid };
            setFocus(next);
            focusRef.current = next;
            pushRow({
              kind: "marker",
              content: `→ 新建 ${next.vendor} · ${next.role} (${sid}) · /cd ${next.project} · 焦点已切到这里`,
            });
          }
          pushRow({ kind: "assistant", content: frame.content, from: focusLabel(focusRef.current) });
          break;
        }
        case "assistant_delta":
          pushRow({ kind: "assistant", content: frame.text, from: focusLabel(focusRef.current) });
          break;
        case "tool":
          pushRow({ kind: "tool", content: `${frame.name}: ${frame.summary}` });
          break;
        case "lag":
          pushRow({ kind: "system", content: `滞后 ${frame.behind} 帧(已跳到最新)` });
          break;
        case "turn_started":
        case "turn_done":
          // Reserved streaming frames — not surfaced as timeline noise.
          break;
      }
    },
    [pushRow],
  );

  // Reconnecting chat socket (P1-4). Lives for the component's lifetime;
  // closes only on unmount. Backoff caps at 8s, retries indefinitely.
  useEffect(() => {
    let disposed = false;
    let attempt = 0;
    let timer: ReturnType<typeof setTimeout> | null = null;

    const connect = () => {
      if (disposed) return;
      const ws = new WebSocket(chatUrlFor(WEB_CHAT_ID, WEB_USER_ID), [CHAT_SUBPROTOCOL]);
      wsRef.current = ws;
      ws.onopen = () => {
        if (disposed) return;
        attempt = 0;
        setReconnectAttempt(0);
        setConnected(true);
        // Pull the gateway's global session list (cross-entry); the disk
        // frame the server sends on connect doesn't include chat sessions.
        try {
          ws.send(JSON.stringify({ type: "text", content: "/sessions", id: nextId("sys") }));
        } catch {
          // socket raced closed — the reconnect path will retry.
        }
      };
      ws.onmessage = (event) => {
        if (typeof event.data !== "string") return;
        try {
          handleFrame(JSON.parse(event.data) as ServerFrame);
        } catch {
          // ignore unparseable server frame
        }
      };
      ws.onclose = () => {
        if (disposed) return;
        setConnected(false);
        attempt += 1;
        setReconnectAttempt(attempt);
        const delay = Math.min(8000, 1000 * 1.5 ** (attempt - 1));
        timer = setTimeout(connect, delay);
      };
      ws.onerror = () => {
        // onclose follows and owns the reconnect schedule.
        ws.close();
      };
    };

    connect();
    return () => {
      disposed = true;
      if (timer) clearTimeout(timer);
      wsRef.current?.close();
      wsRef.current = null;
    };
  }, [handleFrame]);

  const sendFrame = useCallback((frame: ClientFrame) => {
    const ws = wsRef.current;
    if (ws?.readyState !== WebSocket.OPEN) return false;
    ws.send(JSON.stringify(frame));
    return true;
  }, []);

  const sendText = useCallback(
    (content: string, echo: boolean) => {
      const ok = sendFrame({ type: "text", content, id: nextId("client") });
      if (ok && echo) pushRow({ kind: "user", content });
      return ok;
    },
    [pushRow, sendFrame],
  );
  useEffect(() => {
    sendTextRef.current = sendText;
  }, [sendText]);

  const submit = useCallback(() => {
    const content = draft.trim();
    if (!content) return;
    if (sendText(content, true)) setDraft("");
  }, [draft, sendText]);

  // Switch focus WITHOUT clearing the timeline — drop a marker and keep
  // the same continuous conversation (N4).
  const switchTo = useCallback(
    (item: SessionItem) => {
      const next = focusOf(item);
      setFocus(next);
      focusRef.current = next;
      sendFrame({
        type: "switch",
        project: item.project,
        ...(item.session ? { session: item.session } : {}),
      });
      const who = [item.vendor, item.role].filter(Boolean).join(" · ") || item.project;
      const sid = item.session ? ` (${item.session})` : "";
      pushRow({
        kind: "marker",
        content: `→ 焦点切到 ${who}${sid} · /cd ${item.project} · 会话流不清屏`,
      });
    },
    [pushRow, sendFrame],
  );

  // Focus a project itself (no specific session) — clicking a project
  // header /cd's into it; the next message spawns a session there. This
  // is the entry point for projects that have no chat session yet.
  const switchToProject = useCallback(
    (project: string) => {
      const next: Focus = { project, session: null, vendor: null, role: null };
      setFocus(next);
      focusRef.current = next;
      sendFrame({ type: "switch", project });
      pushRow({
        kind: "marker",
        content: `→ 焦点切到项目 ${project} · /cd ${project} · 发消息会在此自动起 session`,
      });
    },
    [pushRow, sendFrame],
  );

  const createSession = useCallback(
    (req: CreateRequest) => {
      if (req.kind === "new") {
        // Scaffold + register the project first. Only cd + spawn once
        // /newproject confirms (handled in the reply case), so a bad path /
        // name doesn't cascade into "unknown project" from a premature /cd.
        pendingNewProjectRef.current = { slug: req.slug, vendor: req.vendor, role: req.role };
        sendText(`/newproject ${req.slug} ${req.path}`, true);
      } else {
        pendingCreateRef.current = {
          project: req.project,
          session: null,
          vendor: req.vendor,
          role: req.role,
        };
        sendText(`/cd ${req.project}`, true);
        sendText(`/new ${req.vendor} ${req.role}`, true);
        sendText("/sessions", false);
      }
      setModalOpen(false);
    },
    [sendText],
  );

  const canTerminal = focus?.vendor === "claude" && !!focus.session;
  // Derive (don't store) the active pane: a remembered "terminal" choice
  // falls back to chat whenever the focus can't host a PTY (e.g. Codex,
  // or no session) without an extra render-triggering effect.
  const showTerminal = view === "terminal" && canTerminal;

  // Auto-scroll the transcript to the newest message — but only when the user
  // is already near the bottom, so reading scrollback isn't interrupted.
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

  return (
    <div className="h-full min-h-0 flex flex-col bg-surface-900 text-text-primary">
      {/* N1 — standalone app bar */}
      <header className="h-12 shrink-0 border-b border-surface-700/40 px-4 flex items-center gap-3">
        <MessageSquare className="h-4 w-4 text-amber-400 shrink-0" />
        <span className="text-sm font-semibold">
          ccteam <span className="text-amber-400">chat</span>
        </span>
        <span className="hidden sm:inline text-[11px] font-mono text-text-dim px-1.5 py-0.5 rounded bg-surface-800">
          独立应用 · ccteam-chat.v1
        </span>
        <span className="flex items-center gap-1.5 text-xs text-text-dim">
          <span className={`h-2 w-2 rounded-full ${connected ? "bg-green-400" : "bg-amber-500"}`} />
          {connected ? "已连接" : reconnectAttempt > 0 ? `重连中… (${reconnectAttempt})` : "连接中…"}
        </span>
        <span className="flex-1" />
        <Link to="/" className="text-xs text-text-dim hover:text-text-primary transition-colors">
          Dashboard ↗
        </Link>
      </header>

      <div className="flex flex-1 min-h-0">
        {/* N2 — all sessions, grouped by project */}
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
              const items = sessions.filter(
                (item) => item.project === project && item.session,
              );
              const projectActive = focus?.project === project && !focus?.session;
              return (
                <div key={project}>
                  <button
                    type="button"
                    onClick={() => switchToProject(project)}
                    title={`聚焦项目 ${project}(/cd;发消息自动起 session)`}
                    className={`w-full text-left px-1.5 py-1 rounded text-[11px] font-mono flex items-center gap-1 ${
                      projectActive
                        ? "bg-surface-800 text-text-primary"
                        : "text-text-dim hover:bg-surface-800/60 hover:text-text-secondary"
                    }`}
                  >
                    {project}
                    {items.length === 0 ? (
                      <span className="text-text-dim/60">· 无 session,点此聚焦</span>
                    ) : null}
                  </button>
                  <div className="space-y-0.5">
                    {items.map((item) => {
                      const active =
                        focus?.project === item.project && focus?.session === item.session;
                      const isClaude = item.vendor === "claude";
                      return (
                        <button
                          key={`${item.project}:${item.session}`}
                          type="button"
                          onClick={() => switchTo(item)}
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
                              isClaude
                                ? "bg-amber-500/15 text-amber-300"
                                : "bg-sky-500/15 text-sky-300"
                            }`}
                          >
                            {item.vendor ?? "?"}
                          </span>
                          <span className="truncate flex-1">{item.role ?? "—"}</span>
                          <span className="text-text-dim font-mono">{item.session}</span>
                        </button>
                      );
                    })}
                  </div>
                </div>
              );
            })}
            {projects.length === 0 ? (
              <div className="px-2 py-3 text-xs text-text-dim leading-5">
                还没有 session。点「＋ 新建」选 项目 / code agent / role 创建。
              </div>
            ) : null}
          </div>
        </aside>

        {/* main: focus crumb + view toggle + transcript/terminal + composer */}
        <main className="flex-1 min-w-0 min-h-0 flex flex-col">
          <div className="h-10 shrink-0 px-4 flex items-center gap-3 border-b border-surface-700/30">
            <span className="text-xs text-text-dim shrink-0">对话焦点 →</span>
            {focus ? (
              <span className="flex items-center gap-2 text-xs min-w-0">
                <span className="text-green-400">●</span>
                <span className="font-semibold truncate">{focus.project}</span>
                <span className="text-text-dim">/</span>
                <span className={focus.vendor === "claude" ? "text-amber-300" : "text-sky-300"}>
                  {[focus.vendor, focus.role].filter(Boolean).join(" · ") || "—"}
                </span>
                {focus.session ? (
                  <span className="font-mono text-text-dim">{focus.session}</span>
                ) : null}
              </span>
            ) : (
              <span className="text-xs text-text-dim">未选择 session</span>
            )}
            <span className="flex-1" />
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

          {showTerminal && focus?.session ? (
            <TerminalView slug={focus.project} sid={focus.session} className="flex-1 min-h-0" />
          ) : (
            <>
              <div
                ref={scrollRef}
                onScroll={onTranscriptScroll}
                className="flex-1 min-h-0 overflow-y-auto p-4 space-y-3"
              >
                {rows.map((row) => {
                  if (row.kind === "marker") {
                    return (
                      <div key={row.id} className="flex justify-center">
                        <span className="text-[11px] font-mono text-sky-300 bg-surface-800 border border-surface-700/60 rounded-full px-3 py-1">
                          {row.content}
                        </span>
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
                      {row.kind === "assistant" && row.from ? (
                        <div className="text-[10px] font-mono text-text-dim mb-1">{row.from}</div>
                      ) : null}
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
                    className="min-h-11 max-h-32 flex-1 resize-y rounded-md bg-surface-800 border border-surface-700 px-3 py-2 text-sm outline-none focus:border-amber-500"
                    placeholder="发消息 / @bot / 命令(/new /use /cd /compact /review)…"
                  />
                  <button
                    type="button"
                    onClick={submit}
                    disabled={!connected}
                    className="h-11 w-11 shrink-0 rounded-md bg-amber-500 text-surface-950 hover:bg-amber-400 disabled:opacity-40 grid place-items-center"
                    title={connected ? "发送" : "未连接"}
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
          defaultProject={
            focus?.project && projects.includes(focus.project)
              ? focus.project
              : (projects[0] ?? "__new")
          }
          onCancel={() => setModalOpen(false)}
          onCreate={createSession}
        />
      ) : null}
    </div>
  );
}

// N3 + new-project: a real <select> for the project (existing ones +
// "＋ 新建项目…"), code agent, and role. Choosing "＋ 新建项目…" reveals a
// name + path so a brand-new project can be scaffolded at any directory.
const NEW_PROJECT = "__new";

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
  onCreate: (req: CreateRequest) => void;
}) {
  const [project, setProject] = useState(defaultProject);
  const [newName, setNewName] = useState("");
  const [newPath, setNewPath] = useState("");
  const [vendor, setVendor] = useState<"claude" | "codex">("claude");
  const [role, setRole] = useState("");

  const isNew = project === NEW_PROJECT;
  const effectiveRole = role.trim() || "assistant";
  const slug = newName.trim();
  const path = newPath.trim();
  // The gateway requires an absolute (or ~) path; validate here so a bad path
  // can't be submitted (which the backend would reject after the fact).
  const pathOk = path.startsWith("/") || path.startsWith("~");
  const slugOk = /^[a-z0-9-]+$/.test(slug);
  const ready = isNew ? slugOk && pathOk : project.length > 0;

  const submit = () => {
    if (!ready) return;
    if (isNew) {
      onCreate({ kind: "new", slug, path, vendor, role: effectiveRole });
    } else {
      onCreate({ kind: "existing", project, vendor, role: effectiveRole });
    }
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
            <option value={NEW_PROJECT}>＋ 新建项目…</option>
          </select>

          {isNew ? (
            <div className="space-y-3 rounded-md border border-surface-700/60 bg-surface-800/40 p-3">
              <div>
                <label className="block text-xs text-text-dim mb-1">新项目名 (slug)</label>
                <input
                  value={newName}
                  onChange={(event) => setNewName(event.target.value)}
                  placeholder="payments-core(小写、数字、连字符)"
                  className="w-full h-9 rounded-md bg-surface-800 border border-surface-700 px-3 text-sm outline-none focus:border-amber-500"
                />
                {slug.length > 0 && !slugOk ? (
                  <div className="mt-1 text-[10px] text-red-400 leading-4">
                    只能用小写字母、数字、连字符。
                  </div>
                ) : null}
              </div>
              <div>
                <label className="block text-xs text-text-dim mb-1">项目路径</label>
                <input
                  value={newPath}
                  onChange={(event) => setNewPath(event.target.value)}
                  placeholder="/home/you/code/myrepo 或 ~/code/myrepo"
                  className="w-full h-9 rounded-md bg-surface-800 border border-surface-700 px-3 text-sm font-mono outline-none focus:border-amber-500"
                />
                <div
                  className={`mt-1 text-[10px] leading-4 ${
                    path.length > 0 && !pathOk ? "text-red-400" : "text-text-dim"
                  }`}
                >
                  {path.length > 0 && !pathOk
                    ? "路径必须以 / 或 ~ 开头(绝对路径)。"
                    : "已有仓库就地接管(不动你的代码);空目录会被创建。需绝对路径或 ~ 开头。"}
                </div>
              </div>
            </div>
          ) : null}

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
            placeholder="reviewer / api / payments-core …"
            className="w-full h-9 rounded-md bg-surface-800 border border-surface-700 px-3 text-sm outline-none focus:border-amber-500"
          />
          <datalist id="ccteam-chat-roles">
            {roleOptions.map((item) => (
              <option key={item} value={item} />
            ))}
          </datalist>

          <div className="text-[11px] font-mono text-text-dim leading-5">
            {isNew ? (
              <>
                → <span className="text-text-secondary">/newproject {slug || "<slug>"} {path || "<path>"}</span>
                <br />→{" "}
                <span className="text-text-secondary">
                  /cd {slug || "<slug>"} + /new {vendor} {effectiveRole}
                </span>
              </>
            ) : (
              <>
                → <span className="text-text-secondary">/cd {project || "<project>"}</span>
                {" + "}
                <span className="text-text-secondary">
                  /new {vendor} {effectiveRole}
                </span>
              </>
            )}
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
