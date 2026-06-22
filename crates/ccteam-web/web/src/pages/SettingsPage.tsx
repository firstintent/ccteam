// v0.8.8 F4 — Settings page: configure IM credentials (Telegram + Lark)
// from the web UI. Backend SoT: `crates/ccteam-web/src/routes/im_config.rs`,
// client: `lib/configApi.ts`.
//
// Shape mirrors SessionsListPage's four-state top level (loading / error /
// empty / success) keyed off `getImConfig()`. Each provider gets a section
// that shows its masked status when configured and an edit form to (re)set
// the secret.
//
// 红线(red lines):
//   - Secrets are NEVER pre-filled or echoed: `getImConfig` carries only
//     last-4 fingerprints (configApi has no plaintext field), and the
//     <input> for a token/secret always starts empty. Re-configuring shows
//     "(set, …wxyz)" + a fresh blank field, never the value.
//   - Web-token rides along automatically (same-origin fetch via configApi).
//   - Overwriting an already-configured secret is destructive → an inline
//     two-step confirm (NOT window.confirm, which clashes with the dark
//     SPA chrome).
//   - Theme: surface-*/brand-*/status-error only (no bare amber-*/red-*).
//
// Telegram `chat_id` capture is async: after the token saves we tell the
// operator to DM the bot, fire `startTelegramChatId()`, then poll
// `pollTelegramChatId()` every 1.5s. The polling timer is owned by a
// `useEffect` keyed on a `pollNonce` and is ALWAYS cleared on unmount /
// re-run (cleanup `clearTimeout`) so navigating away never leaks a timer.

import { useCallback, useEffect, useRef, useState } from "react";
import {
  getImConfig,
  pollTelegramChatId,
  saveLark,
  saveTelegramToken,
  startTelegramChatId,
  type ChatIdPollStatus,
  type ImConfigStatus,
} from "../lib/configApi";
import { toastBus } from "../lib/toastBus";
import { createUser, deleteUser, listUsers, type TenantView } from "../lib/usersApi";

/** Poll interval for the async Telegram `chat_id` capture. */
const CHAT_ID_POLL_MS = 1500;

// Shared field/control class strings — kept in one place so both sections
// stay visually identical and on-theme (brand focus ring, no bare colors).
const FIELD_CLASS =
  "w-full px-3 py-2 bg-surface-900 border border-surface-700/60 rounded-lg " +
  "text-text-primary text-sm font-mono placeholder:text-text-dim " +
  "focus:outline-none focus:ring-2 focus:ring-brand-600 focus:border-transparent " +
  "disabled:opacity-50 transition-colors";
const PRIMARY_BTN_CLASS =
  "px-3 py-1.5 bg-brand-600 hover:bg-brand-700 text-white text-xs font-medium " +
  "rounded-lg transition-colors disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer";
const GHOST_BTN_CLASS =
  "px-3 py-1.5 border border-surface-700/60 text-text-secondary hover:text-text-primary " +
  "hover:bg-surface-800 text-xs font-medium rounded-lg transition-colors cursor-pointer " +
  "disabled:opacity-50 disabled:cursor-not-allowed";

export default function SettingsPage() {
  const [config, setConfig] = useState<ImConfigStatus | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Re-fetch helper so a successful save can refresh the masked status.
  const reload = useCallback(() => {
    getImConfig()
      .then((c) => setConfig(c))
      .catch((err) => {
        const msg = err instanceof Error ? err.message : String(err);
        if (msg !== "UNAUTHENTICATED") setError(msg);
      });
  }, []);

  useEffect(() => {
    let cancelled = false;
    getImConfig()
      .then((c) => {
        if (!cancelled) setConfig(c);
      })
      .catch((err) => {
        if (cancelled) return;
        const msg = err instanceof Error ? err.message : String(err);
        if (msg !== "UNAUTHENTICATED") setError(msg);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  if (error) {
    return (
      <div
        data-testid="settings-error"
        className="p-4 text-xs text-status-error font-mono"
        role="alert"
      >
        failed to load settings: {error}
      </div>
    );
  }
  if (config === null) {
    return (
      <div
        data-testid="settings-loading"
        className="p-4 text-xs text-text-dim font-mono"
      >
        loading settings…
      </div>
    );
  }

  return (
    <div
      data-testid="settings-page"
      className="p-4 max-w-md mx-auto flex flex-col gap-5"
    >
      <header className="flex flex-col gap-1">
        <h1 className="text-sm font-medium text-text-primary">IM Credentials</h1>
        <p className="text-xs text-text-dim font-mono">
          Connect a chat transport so ccteam can reach you. Changes apply on
          the next restart.
        </p>
      </header>

      {config.transport_warning ? (
        <div
          data-testid="settings-transport-warning"
          role="status"
          className="text-[11px] font-mono text-brand-400 bg-brand-600/10 border border-brand-600/30 rounded-lg px-3 py-2"
        >
          {config.transport_warning}
        </div>
      ) : null}

      <TelegramSection status={config.telegram} onSaved={reload} />
      <LarkSection status={config.lark} onSaved={reload} />
      <UserManagementSection />
    </div>
  );
}

// --------------------------------------------------------------------------
// v0.8.18 档1 — multi-user management (web-first). The admin/owner mints a
// per-user tenant; each gets a one-time personal link (?token=ccteam:<hex>)
// that signs them in as themselves and scopes the session list to their own.
// Backend SoT: `crates/ccteam-web/src/routes/users.rs`; client `lib/usersApi`.
// Admin-gated: a non-admin caller 403s → we show the read-only note instead of
// the management form. There is deliberately NO `ccteam user` CLI — runtime
// user writes live here on the web.
// --------------------------------------------------------------------------

function UserManagementSection() {
  const [users, setUsers] = useState<TenantView[] | null>(null);
  const [forbidden, setForbidden] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [handle, setHandle] = useState("");
  const [creating, setCreating] = useState(false);
  // The one-time personal link surfaced after a create (token shown once).
  const [newLink, setNewLink] = useState<{ handle: string; link: string } | null>(null);
  const [confirmDelete, setConfirmDelete] = useState<string | null>(null);

  const load = useCallback(() => {
    listUsers()
      .then((list) => {
        setUsers(list);
        setForbidden(false);
        setError(null);
      })
      .catch((err) => {
        const msg = err instanceof Error ? err.message : String(err);
        if (msg === "UNAUTHENTICATED") return;
        if (msg === "FORBIDDEN") {
          setForbidden(true);
          setUsers([]);
          return;
        }
        setError(msg);
      });
  }, []);

  useEffect(() => {
    load();
  }, [load]);

  async function onCreate(e: React.FormEvent) {
    e.preventDefault();
    const h = handle.trim();
    if (creating || h.length === 0) return;
    setCreating(true);
    try {
      const res = await createUser(h);
      const origin =
        typeof window !== "undefined" && window.location ? window.location.origin : "";
      setNewLink({ handle: res.tenant.handle, link: `${origin}${res.personal_link}` });
      setHandle("");
      load();
    } catch (err) {
      if (err instanceof Error && err.message === "UNAUTHENTICATED") return;
      toastBus.handler?.error(err instanceof Error ? err.message : "create failed");
    } finally {
      setCreating(false);
    }
  }

  function onDelete(id: string) {
    deleteUser(id)
      .then(() => {
        setConfirmDelete(null);
        load();
      })
      .catch((err) => {
        if (err instanceof Error && err.message === "UNAUTHENTICATED") return;
        toastBus.handler?.error(err instanceof Error ? err.message : "delete failed");
      });
  }

  function copyLink(link: string) {
    if (typeof navigator !== "undefined" && navigator.clipboard) {
      navigator.clipboard
        .writeText(link)
        .then(() => toastBus.handler?.info("链接已复制 / Link copied"), () => {});
    }
  }

  return (
    <Section
      testId="settings-users"
      title="用户管理 · Users"
      subtitle="给每个人开独立 web 入口:添加用户 → 复制个人链接发给他;他打开即以自己的身份登录,只看自己的 session。"
    >
      {forbidden ? (
        <p
          data-testid="settings-users-forbidden"
          className="text-[11px] font-mono text-text-dim leading-relaxed"
        >
          仅 <b className="text-text-primary">管理员(owner token)</b> 可管理用户;你当前以普通用户身份登录。
        </p>
      ) : (
        <div className="flex flex-col gap-3">
          <form onSubmit={onCreate} className="flex flex-col gap-2">
            <label
              htmlFor="settings-user-handle"
              className="text-[11px] text-text-muted font-medium"
            >
              新用户名 / Handle
            </label>
            <div className="flex items-center gap-2">
              <input
                id="settings-user-handle"
                type="text"
                autoComplete="off"
                value={handle}
                onChange={(e) => setHandle(e.target.value)}
                disabled={creating}
                spellCheck={false}
                placeholder="alice"
                className={FIELD_CLASS}
              />
              <button
                type="submit"
                disabled={creating || handle.trim().length === 0}
                className={`${PRIMARY_BTN_CLASS} whitespace-nowrap`}
              >
                {creating ? "…" : "添加用户"}
              </button>
            </div>
          </form>

          {newLink ? (
            <div
              data-testid="settings-user-newlink"
              className="flex flex-col gap-1.5 rounded-lg border border-brand-600/30 bg-brand-600/10 px-3 py-2"
            >
              <p className="text-[11px] font-mono text-brand-400">
                <b className="text-text-primary">{newLink.handle}</b>{" "}
                的个人链接(只显示这一次,复制发给他):
              </p>
              <div className="flex items-center gap-2">
                <code className="flex-1 text-[10px] text-text-secondary break-all bg-surface-900 rounded px-2 py-1 border border-surface-700/60">
                  {newLink.link}
                </code>
                <button
                  type="button"
                  onClick={() => copyLink(newLink.link)}
                  className={GHOST_BTN_CLASS}
                >
                  复制
                </button>
              </div>
              <button
                type="button"
                onClick={() => setNewLink(null)}
                className="text-[10px] text-text-dim hover:text-text-secondary self-start"
              >
                知道了,关闭
              </button>
            </div>
          ) : null}

          {error ? (
            <p className="text-[11px] font-mono text-status-error">加载失败: {error}</p>
          ) : users === null ? (
            <p className="text-[11px] font-mono text-text-dim">loading…</p>
          ) : users.length === 0 ? (
            <p className="text-[11px] font-mono text-text-dim">还没有用户。添加一个吧。</p>
          ) : (
            <ul className="flex flex-col gap-1">
              {users.map((u) => (
                <li
                  key={u.id}
                  className="flex items-center justify-between gap-2 text-[11px] font-mono rounded-lg border border-surface-700/40 bg-surface-900/60 px-3 py-1.5"
                >
                  <span className="flex flex-col">
                    <span className="text-text-primary">{u.handle}</span>
                    <span className="text-text-dim text-[10px]">
                      {u.linked_chat ? `IM: ${u.linked_chat}` : "未绑定 IM"}
                    </span>
                  </span>
                  {confirmDelete === u.id ? (
                    <span className="flex items-center gap-1.5">
                      <span className="text-status-error">删除?</span>
                      <button
                        type="button"
                        onClick={() => onDelete(u.id)}
                        className={GHOST_BTN_CLASS}
                      >
                        确认
                      </button>
                      <button
                        type="button"
                        onClick={() => setConfirmDelete(null)}
                        className={GHOST_BTN_CLASS}
                      >
                        取消
                      </button>
                    </span>
                  ) : (
                    <button
                      type="button"
                      onClick={() => setConfirmDelete(u.id)}
                      className={GHOST_BTN_CLASS}
                    >
                      删除
                    </button>
                  )}
                </li>
              ))}
            </ul>
          )}

          <p className="text-[10px] font-mono text-text-dim leading-relaxed">
            诚实边界:同一台机器、同一个 OS 账号下是
            <b className="text-text-secondary">软隔离(UX)</b>、不是安全边界。
          </p>
        </div>
      )}
    </Section>
  );
}

// --------------------------------------------------------------------------
// Section chrome
// --------------------------------------------------------------------------

function Section({
  testId,
  title,
  subtitle,
  children,
}: {
  testId: string;
  title: string;
  subtitle: string;
  children: React.ReactNode;
}) {
  return (
    <section
      data-testid={testId}
      className="flex flex-col gap-3 bg-surface-850/60 border border-surface-700/40 rounded-lg p-4"
    >
      <div className="flex flex-col gap-0.5">
        <h2 className="text-xs font-medium text-text-primary uppercase tracking-wide">
          {title}
        </h2>
        <p className="text-[11px] text-text-dim font-mono">{subtitle}</p>
      </div>
      {children}
    </section>
  );
}

/** A masked "already configured" summary line. */
function MaskedLine({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-center justify-between gap-2 text-[11px] font-mono">
      <span className="text-text-muted">{label}</span>
      <span className="text-text-secondary">{value}</span>
    </div>
  );
}

// --------------------------------------------------------------------------
// Telegram
// --------------------------------------------------------------------------

export function TelegramSection({
  status,
  onSaved,
}: {
  status: ImConfigStatus["telegram"];
  onSaved: () => void;
}) {
  const configured = status?.configured ?? false;
  const [token, setToken] = useState("");
  const [pending, setPending] = useState(false);
  // Two-step confirm gate when overwriting an existing token.
  const [confirming, setConfirming] = useState(false);

  // Async chat_id capture. `pollNonce` bumps to (re)start the poll loop; the
  // useEffect below owns the timer + its cleanup.
  const [chatIdStatus, setChatIdStatus] = useState<ChatIdPollStatus | null>(null);
  const [chatIdLast4, setChatIdLast4] = useState<string | null>(null);
  const [pollNonce, setPollNonce] = useState(0);

  // Drive the chat_id poll loop. The recursive setTimeout is stored in a ref
  // and ALWAYS cleared by the cleanup, so unmount / re-run can't leak it.
  // We stop scheduling on any terminal status (captured/timeout/error).
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => {
    if (pollNonce === 0) return; // not started yet
    let cancelled = false;

    const tick = () => {
      pollTelegramChatId()
        .then((res) => {
          if (cancelled) return;
          setChatIdStatus(res.status);
          if (res.chat_id_last4) setChatIdLast4(res.chat_id_last4);
          if (res.status === "pending") {
            timerRef.current = setTimeout(tick, CHAT_ID_POLL_MS);
          } else if (res.status === "captured") {
            // Terminal-good: the masked status changed → refresh it.
            onSaved();
          }
          // captured / timeout / error → stop scheduling.
        })
        .catch((err) => {
          if (cancelled) return;
          const msg = err instanceof Error ? err.message : String(err);
          if (msg !== "UNAUTHENTICATED") setChatIdStatus("error");
        });
    };
    // First tick fires ~immediately; subsequent ones every CHAT_ID_POLL_MS.
    timerRef.current = setTimeout(tick, CHAT_ID_POLL_MS);

    return () => {
      cancelled = true;
      if (timerRef.current) {
        clearTimeout(timerRef.current);
        timerRef.current = null;
      }
    };
    // onSaved is stable (useCallback). Re-run only when a new capture starts.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [pollNonce]);

  async function persistToken() {
    setPending(true);
    setConfirming(false);
    try {
      const res = await saveTelegramToken(token.trim());
      toastBus.handler?.info(
        `Telegram saved (@${res.bot_username}). Now DM the bot to bind your chat.`,
      );
      setToken("");
      onSaved();
      // Kick off the async chat_id capture.
      setChatIdStatus("pending");
      setChatIdLast4(null);
      await startTelegramChatId();
      setPollNonce((n) => n + 1);
    } catch (err) {
      if (err instanceof Error && err.message === "UNAUTHENTICATED") return;
      const msg = err instanceof Error ? err.message : "save failed";
      toastBus.handler?.error(msg);
    } finally {
      setPending(false);
    }
  }

  function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    if (pending || token.trim().length === 0) return;
    // Overwriting an existing token is destructive → require confirm first.
    if (configured && !confirming) {
      setConfirming(true);
      return;
    }
    void persistToken();
  }

  function retryCapture() {
    setChatIdStatus("pending");
    setChatIdLast4(null);
    startTelegramChatId()
      .then(() => setPollNonce((n) => n + 1))
      .catch((err) => {
        if (err instanceof Error && err.message === "UNAUTHENTICATED") return;
        toastBus.handler?.error(
          err instanceof Error ? err.message : "could not restart capture",
        );
        setChatIdStatus("error");
      });
  }

  return (
    <Section
      testId="settings-telegram"
      title="Telegram"
      subtitle="Bot token from @BotFather; you DM the bot to bind your chat."
    >
      {configured && status ? (
        <div className="flex flex-col gap-1 pb-1">
          <MaskedLine label="bot token" value={`(set, ${status.bot_token_last4})`} />
          <MaskedLine label="bound chats" value={String(status.chat_id_count)} />
          {status.chat_id_count === 0 ? (
            <p className="text-[11px] font-mono text-status-error pt-1">
              No chat bound yet — set the token and DM the bot below.
            </p>
          ) : null}
        </div>
      ) : (
        <p className="text-[11px] font-mono text-text-dim pb-1">Not configured.</p>
      )}

      <form onSubmit={handleSubmit} className="flex flex-col gap-2">
        <label
          htmlFor="settings-telegram-token"
          className="text-[11px] text-text-muted font-medium"
        >
          {configured ? "Replace bot token" : "Bot token"}
        </label>
        <input
          id="settings-telegram-token"
          type="password"
          autoComplete="off"
          value={token}
          onChange={(e) => {
            setToken(e.target.value);
            if (confirming) setConfirming(false);
          }}
          disabled={pending}
          spellCheck={false}
          placeholder="123456:ABC-DEF…"
          className={FIELD_CLASS}
        />
        <div className="flex items-center gap-2 justify-end">
          {confirming ? (
            <>
              <span className="text-[11px] font-mono text-status-error mr-auto">
                Overwrite the existing token?
              </span>
              <button
                type="button"
                onClick={() => setConfirming(false)}
                disabled={pending}
                className={GHOST_BTN_CLASS}
              >
                Cancel
              </button>
              <button type="submit" disabled={pending} className={PRIMARY_BTN_CLASS}>
                {pending ? "Saving…" : "Confirm overwrite"}
              </button>
            </>
          ) : (
            <button
              type="submit"
              disabled={pending || token.trim().length === 0}
              className={PRIMARY_BTN_CLASS}
            >
              {pending ? "Saving…" : configured ? "Replace token" : "Save token"}
            </button>
          )}
        </div>
      </form>

      <ChatIdCapture
        status={chatIdStatus}
        chatIdLast4={chatIdLast4}
        onRetry={retryCapture}
      />
    </Section>
  );
}

/** The async chat_id capture banner — reflects the poll state machine. */
function ChatIdCapture({
  status,
  chatIdLast4,
  onRetry,
}: {
  status: ChatIdPollStatus | null;
  chatIdLast4: string | null;
  onRetry: () => void;
}) {
  if (status === null || status === "idle") return null;
  return (
    <div
      data-testid="settings-telegram-chatid"
      className="text-[11px] font-mono rounded-lg border border-surface-700/40 bg-surface-900/60 px-3 py-2 flex flex-col gap-1"
    >
      {status === "pending" ? (
        <span className="text-brand-400">
          Waiting for your DM — open Telegram and send the bot any message…
        </span>
      ) : null}
      {status === "captured" ? (
        <>
          <span className="text-status-running">
            Chat bound{chatIdLast4 ? ` (${chatIdLast4})` : ""}.
          </span>
          <span className="text-text-dim">
            Restart ccteam (`ccteam stop && ccteam start`) to apply.
          </span>
        </>
      ) : null}
      {status === "timeout" ? (
        <>
          <span className="text-status-error">
            No message captured yet — did you DM the bot?
          </span>
          <button
            type="button"
            onClick={onRetry}
            className={`${GHOST_BTN_CLASS} self-start mt-1`}
          >
            Retry capture
          </button>
        </>
      ) : null}
      {status === "error" ? (
        <>
          <span className="text-status-error">Capture failed.</span>
          <button
            type="button"
            onClick={onRetry}
            className={`${GHOST_BTN_CLASS} self-start mt-1`}
          >
            Retry capture
          </button>
        </>
      ) : null}
    </div>
  );
}

// --------------------------------------------------------------------------
// Lark / Feishu
// --------------------------------------------------------------------------

/** Split a textarea blob into a trimmed `open_id` list (comma OR newline). */
function parseUserIds(raw: string): string[] {
  return raw
    .split(/[\n,]/)
    .map((s) => s.trim())
    .filter((s) => s.length > 0);
}

export function LarkSection({
  status,
  onSaved,
}: {
  status: ImConfigStatus["lark"];
  onSaved: () => void;
}) {
  const configured = status?.configured ?? false;
  const [appId, setAppId] = useState("");
  const [appSecret, setAppSecret] = useState("");
  const [useFeishu, setUseFeishu] = useState(status?.use_feishu ?? true);
  const [userIdsRaw, setUserIdsRaw] = useState("");
  const [pending, setPending] = useState(false);
  const [confirming, setConfirming] = useState(false);

  const userIds = parseUserIds(userIdsRaw);
  const canSubmit =
    !pending && appId.trim().length > 0 && appSecret.trim().length > 0;

  async function persist() {
    setPending(true);
    setConfirming(false);
    try {
      const res = await saveLark({
        app_id: appId.trim(),
        app_secret: appSecret.trim(),
        allowed_user_ids: userIds,
        use_feishu: useFeishu,
      });
      toastBus.handler?.info(res.note || "Lark saved. Restart to apply.");
      setAppId("");
      setAppSecret("");
      setUserIdsRaw("");
      onSaved();
    } catch (err) {
      if (err instanceof Error && err.message === "UNAUTHENTICATED") return;
      const msg = err instanceof Error ? err.message : "save failed";
      toastBus.handler?.error(msg);
    } finally {
      setPending(false);
    }
  }

  function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    if (!canSubmit) return;
    if (configured && !confirming) {
      setConfirming(true);
      return;
    }
    void persist();
  }

  return (
    <Section
      testId="settings-lark"
      title="Lark / Feishu"
      subtitle="App ID + secret; allowlist open_ids (empty = no one can reach the bot)."
    >
      {configured && status ? (
        <div className="flex flex-col gap-1 pb-1">
          <MaskedLine label="app id" value={`(set, ${status.app_id_last4})`} />
          <MaskedLine
            label="region"
            value={status.use_feishu ? "Feishu (CN)" : "Lark (intl)"}
          />
          <MaskedLine
            label="allowed users"
            value={String(status.allowed_user_id_count)}
          />
        </div>
      ) : (
        <p className="text-[11px] font-mono text-text-dim pb-1">Not configured.</p>
      )}

      <form onSubmit={handleSubmit} className="flex flex-col gap-2">
        <label
          htmlFor="settings-lark-appid"
          className="text-[11px] text-text-muted font-medium"
        >
          App ID
        </label>
        <input
          id="settings-lark-appid"
          type="text"
          autoComplete="off"
          value={appId}
          onChange={(e) => {
            setAppId(e.target.value);
            if (confirming) setConfirming(false);
          }}
          disabled={pending}
          spellCheck={false}
          placeholder="cli_…"
          className={FIELD_CLASS}
        />

        <label
          htmlFor="settings-lark-secret"
          className="text-[11px] text-text-muted font-medium"
        >
          App secret
        </label>
        <input
          id="settings-lark-secret"
          type="password"
          autoComplete="off"
          value={appSecret}
          onChange={(e) => {
            setAppSecret(e.target.value);
            if (confirming) setConfirming(false);
          }}
          disabled={pending}
          spellCheck={false}
          placeholder="••••••••"
          className={FIELD_CLASS}
        />

        <fieldset className="flex flex-col gap-1.5 pt-1">
          <legend className="text-[11px] text-text-muted font-medium pb-1">
            Region
          </legend>
          <label className="flex items-center gap-2 text-[11px] font-mono text-text-secondary cursor-pointer">
            <input
              type="radio"
              name="settings-lark-region"
              checked={useFeishu}
              onChange={() => setUseFeishu(true)}
              disabled={pending}
              className="accent-brand-600"
            />
            Feishu (CN)
          </label>
          <label className="flex items-center gap-2 text-[11px] font-mono text-text-secondary cursor-pointer">
            <input
              type="radio"
              name="settings-lark-region"
              checked={!useFeishu}
              onChange={() => setUseFeishu(false)}
              disabled={pending}
              className="accent-brand-600"
            />
            Lark (international)
          </label>
        </fieldset>

        <label
          htmlFor="settings-lark-users"
          className="text-[11px] text-text-muted font-medium pt-1"
        >
          Allowed open_ids (comma or newline separated)
        </label>
        <textarea
          id="settings-lark-users"
          value={userIdsRaw}
          onChange={(e) => setUserIdsRaw(e.target.value)}
          disabled={pending}
          rows={3}
          spellCheck={false}
          placeholder="ou_abc…, ou_def…"
          className={`${FIELD_CLASS} resize-y`}
        />
        {userIds.length === 0 ? (
          <p className="text-[11px] font-mono text-status-error">
            Empty allowlist = fail-closed: the bot will answer no one.
          </p>
        ) : (
          <p className="text-[11px] font-mono text-text-dim">
            {userIds.length} user{userIds.length === 1 ? "" : "s"} allowed.
          </p>
        )}

        <div className="flex items-center gap-2 justify-end pt-1">
          {confirming ? (
            <>
              <span className="text-[11px] font-mono text-status-error mr-auto">
                Overwrite the existing Lark credentials?
              </span>
              <button
                type="button"
                onClick={() => setConfirming(false)}
                disabled={pending}
                className={GHOST_BTN_CLASS}
              >
                Cancel
              </button>
              <button type="submit" disabled={!canSubmit} className={PRIMARY_BTN_CLASS}>
                {pending ? "Saving…" : "Confirm overwrite"}
              </button>
            </>
          ) : (
            <button type="submit" disabled={!canSubmit} className={PRIMARY_BTN_CLASS}>
              {pending ? "Saving…" : configured ? "Replace credentials" : "Save"}
            </button>
          )}
        </div>
      </form>
    </Section>
  );
}
