// v0.8.8 F4 — Settings panels: the admin's GLOBAL IM credentials
// (Telegram + Lark), the tenant's self-serve 「我的 IM bot」, and the
// admin-only user management table. SettingsView places them: credentials
// under 设置→接入 (admin), MyImSection under 设置→接入 (tenant), and
// UserManagementSection on the standalone 管理员 · Admin tab.
// Backend SoT: `crates/ccteam-web/src/routes/im_config.rs` (+ `users.rs`);
// clients: `lib/configApi.ts` + `lib/usersApi.ts`.
//
// v0.8.19 W3b — rewritten from a cramped `max-w-md` single column of three
// hand-rolled sections into a card-based layout on the shared primitive
// library (`components/ui`): each IM provider is a `Card` (header = name +
// status `Badge` + actions, body = form, optional right-side label→mono
// status read-out, footer = "下次重启生效"); user management is a real
// semantic `<table>`. The `FIELD_CLASS` / `*_BTN_CLASS` constants are gone —
// `Input` / `Label` / `Button` / `Badge` carry the styling now (so it reads
// correctly in BOTH dark and light theme via the `@theme` tokens).
//
// 红线(red lines) — UNCHANGED behavior:
//   - Secrets are NEVER pre-filled or echoed: `getImConfig` carries only
//     last-4 fingerprints (configApi has no plaintext field), and the
//     <input> for a token/secret always starts empty. Re-configuring shows
//     "(set, …wxyz)" + a fresh blank field, never the value.
//   - Web-token rides along automatically (same-origin fetch via configApi).
//   - Overwriting an already-configured secret is destructive → an inline
//     two-step confirm (NOT window.confirm, which clashes with the dark/light
//     SPA chrome).
//   - Admin-gate: this panel is admin-only (global IM creds are GLOBAL
//     daemon config). A tenant gets their self-serve MyImSection instead.
//   - Theme tokens only (surface-*/brand-*/text-*/status-*), no bare colors.
//
// NOTE — no "测试连接 / Test connection" button: configApi exposes no
// getMe-only endpoint (the only Telegram validation rides INSIDE
// `saveTelegramToken`, which validates via getMe server-side and echoes the
// bot @username on save). Per the W3b brief we do NOT invent a backend, so
// the prototype's standalone test button is intentionally omitted.
//
// Telegram `chat_id` capture is async: after the token saves we tell the
// operator to DM the bot, fire `startTelegramChatId()`, then poll
// `pollTelegramChatId()` every 1.5s. The polling timer is owned by a
// `useEffect` keyed on a `pollNonce` and is ALWAYS cleared on unmount /
// re-run (cleanup `clearTimeout`) so navigating away never leaks a timer.

import { useCallback, useEffect, useRef, useState } from "react";
import { Link2, MessageSquare, Send, Users } from "lucide-react";
import {
  pollTelegramChatId,
  saveLark,
  saveTelegramToken,
  startTelegramChatId,
  type ChatIdPollStatus,
  type ImConfigStatus,
} from "../lib/configApi";
import { copyText } from "../lib/clipboard";
import { toastBus } from "../lib/toastBus";
import {
  createUser,
  deleteUser,
  getMyLarkOpenIdCandidates,
  getMyTelegramChatIdCandidates,
  getUserLink,
  listUsers,
  putMyIm,
  putMyLarkAllowedUsers,
  putMyTelegramAllowedChats,
  type SenderCandidate,
  type TenantView,
} from "../lib/usersApi";
import { makeT, type Lang } from "../lib/i18n";
import {
  Badge,
  Button,
  Card,
  CardContent,
  CardFooter,
  CardHeader,
  CardTitle,
  Input,
  Label,
  Textarea,
} from "../components/ui";

/** Poll interval for the async Telegram `chat_id` capture. */
const CHAT_ID_POLL_MS = 1500;

// --------------------------------------------------------------------------
// Shared chrome
// --------------------------------------------------------------------------

/** One row of the right-side label→mono status read-out. `ok` greens it. */
function ReadoutRow({ label, value, ok }: { label: string; value: string; ok?: boolean }) {
  return (
    <div className="flex items-center justify-between gap-2">
      <span className="text-text-dim">{label}</span>
      <span
        className={`font-mono text-right ${ok ? "text-status-running" : "text-text-secondary"}`}
      >
        {value}
      </span>
    </div>
  );
}

/** Compact label→value status stack shared by the collapsed summaries. */
function Readout({ children }: { children: React.ReactNode }) {
  return <div className="flex flex-col gap-2 text-[11px]">{children}</div>;
}

// --------------------------------------------------------------------------
// Telegram
// --------------------------------------------------------------------------

export function TelegramSection({
  lang = "zh",
  status,
  onSaved,
}: {
  lang?: Lang;
  status: ImConfigStatus["telegram"];
  onSaved: () => void;
}) {
  const t = makeT(lang);
  const configured = status?.configured ?? false;
  const [editing, setEditing] = useState(!configured);
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
      setEditing(false);
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
        toastBus.handler?.error(err instanceof Error ? err.message : "could not restart capture");
        setChatIdStatus("error");
      });
  }

  const bound = (status?.chat_id_count ?? 0) > 0;

  function cancelEdit() {
    setToken("");
    setConfirming(false);
    setEditing(false);
  }

  return (
    <Card data-testid="settings-telegram">
      <CardHeader>
        <Send className="text-text-secondary" />
        <CardTitle className="flex-1">Telegram</CardTitle>
        {configured ? (
          <Badge variant="running">已连接</Badge>
        ) : (
          <Badge variant="idle">未配置</Badge>
        )}
      </CardHeader>

      {editing ? (
        <CardContent className="flex flex-col gap-3">
          <form onSubmit={handleSubmit} className="flex flex-col gap-2">
            <Label htmlFor="settings-telegram-token">
              {configured ? "重置 bot token" : "Bot token"}
            </Label>
            {/* Red line: the password field always starts EMPTY — the
                fingerprint is text-only, never the input value. */}
            <Input
              id="settings-telegram-token"
              data-testid="settings-telegram-token"
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
              className="font-mono"
            />
            <div className="flex items-center gap-2 justify-end">
              {confirming ? (
                <>
                  <span className="text-[11px] font-mono text-status-error mr-auto">
                    覆盖已配置的 token?
                  </span>
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => setConfirming(false)}
                    disabled={pending}
                  >
                    取消
                  </Button>
                  <Button type="submit" size="sm" variant="destructive" disabled={pending}>
                    {pending ? "保存中…" : "确认覆盖"}
                  </Button>
                </>
              ) : (
                <>
                  {configured ? (
                    <Button variant="ghost" size="sm" onClick={cancelEdit} disabled={pending}>
                      取消
                    </Button>
                  ) : null}
                  <Button
                    type="submit"
                    size="sm"
                    disabled={pending || token.trim().length === 0}
                  >
                    {pending ? "保存中…" : configured ? "重置 token" : "保存 token"}
                  </Button>
                </>
              )}
            </div>
          </form>
          <p className="text-[10px] font-mono text-text-dim leading-relaxed">
            token 永不回显,仅显末 4 位;重置走两步确认(破坏性)。从 @BotFather 取 token,保存后 DM 机器人绑定你的 chat。
          </p>
        </CardContent>
      ) : (
        <CardContent data-testid="settings-telegram-summary" className="flex flex-col gap-3">
          <Readout>
            <ReadoutRow
              label="bot token"
              value={configured && status ? `(set, ${status.bot_token_last4})` : "—"}
              ok={configured}
            />
            <ReadoutRow
              label="bound chats"
              value={configured && status ? String(status.chat_id_count) : "—"}
              ok={bound}
            />
            {configured && status && status.chat_id_count === 0 ? (
              <p className="text-[11px] font-mono text-status-error leading-relaxed">
                尚未绑定 chat —— 设好 token 后在下方 DM 机器人。
              </p>
            ) : null}
          </Readout>
          <Button
            variant="outline"
            size="sm"
            className="self-end"
            onClick={() => setEditing(true)}
          >
            {t("accessResetCredentials")}
          </Button>
        </CardContent>
      )}

      <ChatIdCapture status={chatIdStatus} chatIdLast4={chatIdLast4} onRetry={retryCapture} />

      <CardFooter>{configured ? "下次 daemon 重启生效" : "未配置"}</CardFooter>
    </Card>
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
      className="text-[11px] font-mono border-t border-surface-800 bg-surface-950/40 px-4 py-2 flex flex-col gap-1"
    >
      {status === "pending" ? (
        <span className="text-brand-400">
          等待你的 DM —— 打开 Telegram 给机器人发任意消息…
        </span>
      ) : null}
      {status === "captured" ? (
        <>
          <span className="text-status-running">
            已绑定 chat{chatIdLast4 ? ` (${chatIdLast4})` : ""}。
          </span>
          <span className="text-text-dim">
            重启 ccteam(`ccteam stop && ccteam start`)生效。
          </span>
        </>
      ) : null}
      {status === "timeout" ? (
        <>
          <span className="text-status-error">还没收到消息 —— 你 DM 机器人了吗?</span>
          <Button variant="ghost" size="sm" onClick={onRetry} className="self-start mt-1">
            重试捕获
          </Button>
        </>
      ) : null}
      {status === "error" ? (
        <>
          <span className="text-status-error">捕获失败。</span>
          <Button variant="ghost" size="sm" onClick={onRetry} className="self-start mt-1">
            重试捕获
          </Button>
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
  lang = "zh",
  status,
  onSaved,
}: {
  lang?: Lang;
  status: ImConfigStatus["lark"];
  onSaved: () => void;
}) {
  const t = makeT(lang);
  const configured = status?.configured ?? false;
  const [editing, setEditing] = useState(!configured);
  const [appId, setAppId] = useState("");
  const [appSecret, setAppSecret] = useState("");
  const [useFeishu, setUseFeishu] = useState(status?.use_feishu ?? true);
  const [userIdsRaw, setUserIdsRaw] = useState("");
  const [pending, setPending] = useState(false);
  const [confirming, setConfirming] = useState(false);

  const userIds = parseUserIds(userIdsRaw);
  const canSubmit = !pending && appId.trim().length > 0 && appSecret.trim().length > 0;

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
      setEditing(false);
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

  function cancelEdit() {
    setAppId("");
    setAppSecret("");
    setUserIdsRaw("");
    setUseFeishu(status?.use_feishu ?? true);
    setConfirming(false);
    setEditing(false);
  }

  return (
    <Card data-testid="settings-lark">
      <CardHeader>
        <MessageSquare className="text-text-secondary" />
        <CardTitle className="flex-1">Lark / 飞书</CardTitle>
        {configured ? (
          <Badge variant="running">已连接</Badge>
        ) : (
          <Badge variant="idle">未配置</Badge>
        )}
      </CardHeader>

      {editing ? (
        <CardContent>
          <form onSubmit={handleSubmit} className="flex flex-col gap-3">
            <div className="grid gap-3 sm:grid-cols-2">
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="settings-lark-appid">App ID</Label>
                <Input
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
                  className="font-mono"
                />
              </div>

              <div className="flex flex-col gap-1.5">
                {/* Red line: app secret field always starts EMPTY. */}
                <Label htmlFor="settings-lark-secret">App secret</Label>
                <Input
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
                  placeholder="(永不回显)"
                  className="font-mono"
                />
              </div>
            </div>

            <fieldset className="flex flex-col gap-1.5">
              <legend className="text-xs font-medium text-text-dim pb-1">区域 · Region</legend>
              <div
                data-testid="settings-lark-region"
                className="grid grid-cols-2 rounded-md border border-surface-700 bg-surface-950 p-0.5"
              >
                <button
                  type="button"
                  aria-pressed={useFeishu}
                  onClick={() => setUseFeishu(true)}
                  disabled={pending}
                  className={`rounded px-3 py-1.5 text-[11px] font-mono transition-colors ${
                    useFeishu
                      ? "bg-brand-500/15 text-brand-400"
                      : "text-text-dim hover:bg-surface-800 hover:text-text-secondary"
                  }`}
                >
                  Feishu CN
                </button>
                <button
                  type="button"
                  aria-pressed={!useFeishu}
                  onClick={() => setUseFeishu(false)}
                  disabled={pending}
                  className={`rounded px-3 py-1.5 text-[11px] font-mono transition-colors ${
                    !useFeishu
                      ? "bg-brand-500/15 text-brand-400"
                      : "text-text-dim hover:bg-surface-800 hover:text-text-secondary"
                  }`}
                >
                  Lark intl
                </button>
              </div>
            </fieldset>

            <div className="flex flex-col gap-1.5">
              <Label htmlFor="settings-lark-users">允许的 open_id(逗号或换行分隔)</Label>
              <Textarea
                id="settings-lark-users"
                value={userIdsRaw}
                onChange={(e) => setUserIdsRaw(e.target.value)}
                disabled={pending}
                rows={2}
                spellCheck={false}
                placeholder="ou_abc…, ou_def…"
                className="font-mono"
              />
              {userIds.length === 0 ? (
                <p className="text-[11px] font-mono text-status-error">
                  空 allowlist = fail-closed:机器人谁也不回。
                </p>
              ) : (
                <p className="text-[11px] font-mono text-text-dim">
                  {userIds.length} user{userIds.length === 1 ? "" : "s"} allowed.
                </p>
              )}
            </div>

            <div className="flex items-center gap-2 justify-end">
              {confirming ? (
                <>
                  <span className="text-[11px] font-mono text-status-error mr-auto">
                    覆盖已配置的 Lark 凭据?
                  </span>
                  <Button
                    variant="ghost"
                    size="sm"
                    onClick={() => setConfirming(false)}
                    disabled={pending}
                  >
                    取消
                  </Button>
                  <Button type="submit" size="sm" variant="destructive" disabled={!canSubmit}>
                    {pending ? "保存中…" : "确认覆盖"}
                  </Button>
                </>
              ) : (
                <>
                  {configured ? (
                    <Button variant="ghost" size="sm" onClick={cancelEdit} disabled={pending}>
                      取消
                    </Button>
                  ) : null}
                  <Button type="submit" size="sm" disabled={!canSubmit}>
                    {pending ? "保存中…" : configured ? "重置凭据" : "保存"}
                  </Button>
                </>
              )}
            </div>
          </form>
        </CardContent>
      ) : (
        <CardContent data-testid="settings-lark-summary" className="flex flex-col gap-3">
          <Readout>
            <ReadoutRow
              label="app id"
              value={configured && status ? `(set, ${status.app_id_last4})` : "—"}
              ok={configured}
            />
            <ReadoutRow
              label="region"
              value={
                configured && status
                  ? status.use_feishu
                    ? "Feishu (CN)"
                    : "Lark (intl)"
                  : useFeishu
                    ? "Feishu (CN)"
                    : "Lark (intl)"
              }
            />
            <ReadoutRow
              label="allowed users"
              value={configured && status ? String(status.allowed_user_id_count) : "—"}
            />
          </Readout>
          <Button
            variant="outline"
            size="sm"
            className="self-end"
            onClick={() => setEditing(true)}
          >
            {t("accessResetCredentials")}
          </Button>
        </CardContent>
      )}

      <CardFooter>app secret 永不回显;重配显「(set, ····wxyz)」+ 空白框 · 下次重启生效</CardFooter>
    </Card>
  );
}

// --------------------------------------------------------------------------
// v0.8.18 档1 — multi-user management (web-first). The admin/owner mints a
// per-user tenant; each gets a one-time personal link (?token=ccteam:<hex>)
// that signs them in as themselves and scopes the session list to their own.
// Backend SoT: `crates/ccteam-web/src/routes/users.rs`; client `lib/usersApi`.
// Admin-gated: a non-admin caller 403s → we show the read-only note instead of
// the management table. There is deliberately NO `ccteam user` CLI — runtime
// user writes live here on the web. SettingsView renders this as the whole
// content of the admin-only 管理员 · Admin tab (never for tenants).
// --------------------------------------------------------------------------

// --------------------------------------------------------------------------
// v0.8.20 F2 — a tenant's OWN IM bot (self-serve). The owner's bot is the
// global one (admin Settings); a per-user tenant runs its own bot so its
// Telegram/Lark drives ONLY its sessions, not a shared admin bot.
//
// v0.9.13 — rebuilt as two independent guided cards (Telegram / Lark), each a
// numbered two-step flow: ① save the credential (its own small save button,
// `putMyIm` only carries that provider) → ② bind who the bot answers (the
// capture auto-starts right after a save, so the next action is never a
// guess). The old single form-wide 保存 button — which mixed the token, the
// Lark credential and the allowlist into one ambiguous submit next to two
// 开始 buttons — is gone.
// --------------------------------------------------------------------------

/** Poll `fetchCandidates(since)` every CHAT_ID_POLL_MS while `since` is set.
 *  One timer per card, always cleared on unmount / restart. */
function useSenderCapture(
  since: number | null,
  fetchCandidates: (since: number) => Promise<{ candidates: SenderCandidate[] }>,
  errorLabel: string,
) {
  const [candidates, setCandidates] = useState<SenderCandidate[]>([]);
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(() => {
    if (since === null) return;
    let cancelled = false;
    const tick = () => {
      fetchCandidates(since)
        .then((res) => {
          if (cancelled) return;
          setCandidates(res.candidates);
          timerRef.current = setTimeout(tick, CHAT_ID_POLL_MS);
        })
        .catch((err) => {
          if (cancelled) return;
          if (!(err instanceof Error) || err.message !== "UNAUTHENTICATED") {
            toastBus.handler?.error(err instanceof Error ? err.message : errorLabel);
          }
          timerRef.current = setTimeout(tick, CHAT_ID_POLL_MS);
        });
    };
    timerRef.current = setTimeout(tick, 300);
    return () => {
      cancelled = true;
      if (timerRef.current) {
        clearTimeout(timerRef.current);
        timerRef.current = null;
      }
    };
    // fetchCandidates / errorLabel are stable module-level fns + literals.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [since]);
  // Render-side gating (instead of a reset-setState in the effect): an
  // inactive capture always reads as empty; a restart repopulates on the
  // first poll tick.
  return since === null ? [] : candidates;
}

/** Step number chip + title for the guided cards. */
function StepHead({ n, title, done }: { n: number; title: string; done?: boolean }) {
  return (
    <div className="flex items-center gap-2">
      <span
        className={`flex size-4.5 shrink-0 items-center justify-center rounded-full text-[10px] font-semibold ${
          done ? "bg-status-running/20 text-status-running" : "bg-brand-500/15 text-brand-400"
        }`}
        aria-hidden="true"
      >
        {done ? "✓" : n}
      </span>
      <span className="text-[11px] font-medium text-text-secondary">{title}</span>
    </div>
  );
}

export function MyImSection() {
  return (
    <section data-testid="settings-my-im" className="flex flex-col gap-4">
      <div className="flex flex-col gap-1">
        <div className="flex items-center gap-2">
          <MessageSquare className="size-4 text-text-secondary" />
          <h2 className="text-sm font-semibold text-text-primary">我的 IM bot · My bot</h2>
          <Badge variant="accent">自助</Badge>
        </div>
        <p className="text-[11px] text-text-muted leading-relaxed">
          配置你自己的 Telegram / Lark 机器人 —— 它只驱动你自己的 session(不共用管理员的全局
          bot)。两边各自独立配置,互不影响;都<b className="text-text-secondary">只回被你允许的人</b>
          ,绑定前谁也不回。保存后即时生效。
        </p>
      </div>

      <MyTelegramCard />
      <MyLarkCard />
    </section>
  );
}

function MyTelegramCard() {
  const [token, setToken] = useState("");
  const [pending, setPending] = useState(false);
  const [tokenSaved, setTokenSaved] = useState(false);
  const [captureSince, setCaptureSince] = useState<number | null>(null);
  const [allowed, setAllowed] = useState<string[]>([]);
  const candidates = useSenderCapture(
    captureSince,
    getMyTelegramChatIdCandidates,
    "chat_id capture failed",
  );

  function startCapture() {
    setCaptureSince(Math.floor(Date.now() / 1000) - 2);
  }

  async function saveToken(e: React.FormEvent) {
    e.preventDefault();
    const tok = token.trim();
    if (pending || !tok) return;
    setPending(true);
    try {
      const res = await putMyIm({ telegram_bot_token: tok });
      setToken("");
      setTokenSaved(true);
      if (res.telegram_unbound) {
        // Fail-closed: an unbound bot answers nobody — walk straight into ②.
        startCapture();
        toastBus.handler?.info("token 已保存 —— 现在私聊 bot 完成第 2 步绑定,绑定前它不回任何人");
      } else {
        toastBus.handler?.info("token 已保存,原有绑定继续生效");
      }
    } catch (err) {
      if (err instanceof Error && err.message === "UNAUTHENTICATED") return;
      toastBus.handler?.error(err instanceof Error ? err.message : "save failed");
    } finally {
      setPending(false);
    }
  }

  async function bindChat(chatId: string) {
    if (pending) return;
    const normalized = Array.from(new Set([...allowed, chatId.trim()].filter(Boolean))).sort();
    setPending(true);
    try {
      const res = await putMyTelegramAllowedChats(normalized);
      setAllowed(normalized);
      setCaptureSince(null);
      toastBus.handler?.info(res.note || "chat_id 已保存,bot 现在只回你");
    } catch (err) {
      if (err instanceof Error && err.message === "UNAUTHENTICATED") return;
      toastBus.handler?.error(err instanceof Error ? err.message : "save failed");
    } finally {
      setPending(false);
    }
  }

  return (
    <Card data-testid="my-im-telegram" className="p-4">
      <div className="flex flex-col gap-3">
        <div className="flex items-center gap-2">
          <Send className="size-4 text-text-secondary" />
          <h3 className="text-[13px] font-semibold text-text-primary">Telegram</h3>
        </div>

        <form onSubmit={saveToken} className="flex flex-col gap-1.5">
          <StepHead n={1} title="保存 bot token" done={tokenSaved} />
          <div className="flex items-center gap-2">
            <Input
              id="my-im-telegram-token"
              aria-label="Telegram bot token"
              type="password"
              autoComplete="off"
              value={token}
              onChange={(e) => setToken(e.target.value)}
              disabled={pending}
              spellCheck={false}
              placeholder="123456:ABC-DEF…"
              className="font-mono"
            />
            <Button
              type="submit"
              size="sm"
              data-testid="my-im-telegram-save"
              disabled={pending || token.trim().length === 0}
            >
              {pending ? "保存中…" : "保存 token"}
            </Button>
          </div>
          <p className="text-[10px] text-text-dim">
            从 @BotFather 拿一个新 bot 的 token(每个 bot 的 token 唯一,保存前会校验)。
          </p>
        </form>

        <div
          className="flex flex-col gap-2 rounded-md border border-surface-800 bg-surface-950/40 p-2"
          data-testid="my-im-telegram-bind"
        >
          <div className="flex items-center justify-between gap-2">
            <StepHead n={2} title="绑定你的 chat" done={allowed.length > 0} />
            {captureSince === null ? (
              <Button
                type="button"
                variant="ghost"
                size="sm"
                data-testid="my-im-telegram-capture"
                onClick={startCapture}
                disabled={pending}
              >
                {allowed.length > 0 ? "再绑一个" : "开始绑定"}
              </Button>
            ) : null}
          </div>
          <p className="text-[10px] text-status-error">
            你的 bot 只回被绑定的 chat。绑定前它不回任何人 —— 否则任何找到 bot
            的人都会以你的身份驱动你的 session。
          </p>
          {allowed.length > 0 ? (
            <p className="text-[10px] font-mono text-status-running">
              已绑定 {allowed.length} 个 chat:{allowed.join(", ")}
            </p>
          ) : null}
          {captureSince !== null ? (
            <p className="text-[10px] text-text-dim">
              现在私聊这个 bot 发一条消息;消息会被拒绝,只用于显示你的 chat_id。
            </p>
          ) : null}
          {candidates.length > 0 ? (
            <div className="flex flex-col gap-1">
              {candidates.map((candidate) => (
                <button
                  key={`${candidate.sender_id}:${candidate.message_id}`}
                  type="button"
                  data-testid={`my-im-telegram-candidate-${candidate.sender_id}`}
                  onClick={() => void bindChat(candidate.sender_id)}
                  disabled={pending}
                  className="flex items-center justify-between gap-2 rounded border border-surface-800 px-2 py-1 text-left text-[11px] font-mono text-text-secondary hover:border-brand-500 hover:text-text-primary"
                >
                  <span>{candidate.sender_id}</span>
                  <span className="text-[10px] text-text-dim">绑定并保存</span>
                </button>
              ))}
            </div>
          ) : captureSince !== null ? (
            <p className="text-[10px] font-mono text-text-dim">等待消息…</p>
          ) : null}
        </div>
      </div>
    </Card>
  );
}

function MyLarkCard() {
  const [appId, setAppId] = useState("");
  const [appSecret, setAppSecret] = useState("");
  const [useFeishu, setUseFeishu] = useState(true);
  const [pending, setPending] = useState(false);
  const [credsSaved, setCredsSaved] = useState(false);
  const [usersRaw, setUsersRaw] = useState("");
  const [allowlistSaved, setAllowlistSaved] = useState(false);
  const [captureSince, setCaptureSince] = useState<number | null>(null);
  const candidates = useSenderCapture(
    captureSince,
    getMyLarkOpenIdCandidates,
    "open_id capture failed",
  );
  const userIds = parseUserIds(usersRaw);

  function startCapture() {
    setCaptureSince(Math.floor(Date.now() / 1000) - 2);
  }

  async function saveCreds(e: React.FormEvent) {
    e.preventDefault();
    const app = appId.trim();
    const secret = appSecret.trim();
    if (pending || !app || !secret) return;
    setPending(true);
    try {
      await putMyIm({
        lark: { app_id: app, app_secret: secret, allowed_user_ids: userIds, use_feishu: useFeishu },
      });
      setAppId("");
      setAppSecret("");
      setCredsSaved(true);
      // Fail-closed: an empty allowlist answers nobody — walk straight into ②.
      if (userIds.length === 0) startCapture();
      toastBus.handler?.info("Lark 凭据已保存 —— 现在完成第 2 步,允许你自己的 open_id");
    } catch (err) {
      if (err instanceof Error && err.message === "UNAUTHENTICATED") return;
      toastBus.handler?.error(err instanceof Error ? err.message : "save failed");
    } finally {
      setPending(false);
    }
  }

  async function saveAllowlist(ids: string[]) {
    if (pending) return;
    const normalized = Array.from(new Set(ids.map((id) => id.trim()).filter(Boolean))).sort();
    setPending(true);
    try {
      const res = await putMyLarkAllowedUsers(normalized);
      setUsersRaw(normalized.join("\n"));
      setAllowlistSaved(true);
      setCaptureSince(null);
      toastBus.handler?.info(res.note || "open_id 已保存到 allowlist");
    } catch (err) {
      if (err instanceof Error && err.message === "UNAUTHENTICATED") return;
      toastBus.handler?.error(err instanceof Error ? err.message : "save failed");
    } finally {
      setPending(false);
    }
  }

  return (
    <Card data-testid="my-im-lark" className="p-4">
      <div className="flex flex-col gap-3">
        <div className="flex items-center gap-2">
          <MessageSquare className="size-4 text-text-secondary" />
          <h3 className="text-[13px] font-semibold text-text-primary">Lark / 飞书</h3>
          <Badge variant="idle">可选</Badge>
        </div>

        <form onSubmit={saveCreds} className="flex flex-col gap-2">
          <StepHead n={1} title="保存 App 凭据" done={credsSaved} />
          <div className="grid gap-2 sm:grid-cols-2">
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="my-im-lark-id">App ID</Label>
              <Input
                id="my-im-lark-id"
                value={appId}
                onChange={(e) => setAppId(e.target.value)}
                disabled={pending}
                spellCheck={false}
                placeholder="cli_…"
                className="font-mono"
              />
            </div>
            <div className="flex flex-col gap-1.5">
              <Label htmlFor="my-im-lark-secret">App Secret</Label>
              <Input
                id="my-im-lark-secret"
                type="password"
                autoComplete="off"
                value={appSecret}
                onChange={(e) => setAppSecret(e.target.value)}
                disabled={pending}
                spellCheck={false}
                placeholder="(永不回显)"
                className="font-mono"
              />
            </div>
          </div>
          <div className="flex items-center justify-between gap-2">
            <label className="flex items-center gap-2 text-[11px] text-text-secondary">
              <input
                type="checkbox"
                checked={useFeishu}
                onChange={(e) => setUseFeishu(e.target.checked)}
                disabled={pending}
                className="accent-brand-500"
              />
              飞书(CN);取消勾选 = Lark intl
            </label>
            <Button
              type="submit"
              size="sm"
              data-testid="my-im-lark-save"
              disabled={pending || appId.trim().length === 0 || appSecret.trim().length === 0}
            >
              {pending ? "保存中…" : "保存凭据"}
            </Button>
          </div>
        </form>

        <div
          className="flex flex-col gap-2 rounded-md border border-surface-800 bg-surface-950/40 p-2"
          data-testid="my-im-lark-bind"
        >
          <div className="flex items-center justify-between gap-2">
            <StepHead n={2} title="允许 open_id(发现或手填)" done={allowlistSaved} />
            {captureSince === null ? (
              <Button
                type="button"
                variant="ghost"
                size="sm"
                data-testid="my-im-lark-capture"
                onClick={startCapture}
                disabled={pending}
              >
                发现 open_id
              </Button>
            ) : null}
          </div>
          <p className="text-[10px] text-status-error">
            空 allowlist = fail-closed:bot 谁也不回。私聊 bot 或群里 @ 它即可发现你的 open_id。
          </p>
          {captureSince !== null ? (
            <p className="text-[10px] text-text-dim">
              现在私聊这个 Lark / 飞书 bot,或在群里 @ bot;消息会被拒绝,只用于显示 sender
              open_id。
            </p>
          ) : null}
          {candidates.length > 0 ? (
            <div className="flex flex-col gap-1">
              {candidates.map((c) => (
                <button
                  key={`${c.open_id}:${c.message_id}`}
                  type="button"
                  data-testid={`my-im-lark-candidate-${c.open_id}`}
                  onClick={() => void saveAllowlist([...userIds, c.open_id])}
                  disabled={pending}
                  className="flex items-center justify-between gap-2 rounded border border-surface-800 px-2 py-1 text-left text-[11px] font-mono text-text-secondary hover:border-brand-500 hover:text-text-primary"
                >
                  <span>{c.open_id}</span>
                  <span className="text-[10px] text-text-dim">绑定并保存</span>
                </button>
              ))}
            </div>
          ) : captureSince !== null ? (
            <p className="text-[10px] font-mono text-text-dim">等待消息…</p>
          ) : null}
          <div className="flex items-end gap-2">
            <div className="flex flex-1 flex-col gap-1.5">
              <Label htmlFor="my-im-lark-users">允许的 open_id(逗号或换行分隔)</Label>
              <Textarea
                id="my-im-lark-users"
                value={usersRaw}
                onChange={(e) => setUsersRaw(e.target.value)}
                disabled={pending}
                rows={2}
                spellCheck={false}
                placeholder="ou_abc…"
                className="font-mono"
              />
            </div>
            <Button
              type="button"
              variant="outline"
              size="sm"
              data-testid="my-im-lark-allowlist-save"
              onClick={() => void saveAllowlist(userIds)}
              disabled={pending || userIds.length === 0}
            >
              保存 allowlist
            </Button>
          </div>
        </div>
      </div>
    </Card>
  );
}

export function UserManagementSection() {
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
    void copyText(link).then((ok) =>
      ok
        ? toastBus.handler?.info("链接已复制 / Link copied")
        : toastBus.handler?.error("复制失败,请手动复制 / Copy failed, please copy manually"),
    );
  }

  // v0.8.20 F3: re-reveal a tenant's personal link on demand (admin only) and
  // copy it — so the owner can re-send a link without re-creating the user.
  function revealAndCopy(id: string) {
    getUserLink(id)
      .then((res) => {
        const origin =
          typeof window !== "undefined" && window.location ? window.location.origin : "";
        copyLink(`${origin}${res.personal_link}`);
      })
      .catch((err) => {
        if (err instanceof Error && err.message === "UNAUTHENTICATED") return;
        toastBus.handler?.error(err instanceof Error ? err.message : "reveal failed");
      });
  }

  return (
    <section data-testid="settings-users" className="flex flex-col gap-4">
      <div className="flex flex-col gap-1">
        <div className="flex items-center gap-2">
          <Users className="size-4 text-text-secondary" />
          <h2 className="text-sm font-semibold text-text-primary">用户管理 · Users</h2>
          <Badge variant="idle">管理员</Badge>
        </div>
        <p className="text-[11px] text-text-muted">
          给每个人开独立 web 入口:添加用户 → 复制个人链接发给他;他打开即以自己的身份登录,只看自己的 session。
        </p>
      </div>

      {forbidden ? (
        <p
          data-testid="settings-users-forbidden"
          className="text-[11px] font-mono text-text-dim leading-relaxed"
        >
          仅 <b className="text-text-primary">管理员(owner token)</b> 可管理用户;你当前以普通用户身份登录。
        </p>
      ) : (
        <div className="flex flex-col gap-3">
          {newLink ? (
            <div
              data-testid="settings-user-newlink"
              className="flex flex-col gap-1.5 rounded-lg border border-brand-500/30 bg-brand-500/10 px-3 py-2"
            >
              <p className="text-[11px] font-mono text-brand-400">
                <b className="text-text-primary">{newLink.handle}</b> 的个人链接(只显示这一次,复制发给他):
              </p>
              <div className="flex items-center gap-2">
                <code className="flex-1 text-[10px] text-text-secondary break-all bg-surface-950 rounded px-2 py-1 border border-surface-700/60">
                  {newLink.link}
                </code>
                <Button variant="outline" size="sm" onClick={() => copyLink(newLink.link)}>
                  复制
                </Button>
              </div>
              <Button
                variant="link"
                size="sm"
                onClick={() => setNewLink(null)}
                className="self-start px-0 text-[10px] text-text-dim no-underline hover:text-text-secondary"
              >
                知道了,关闭
              </Button>
            </div>
          ) : null}

          <Card className="p-0">
            <table className="w-full border-collapse text-[12px]">
              <thead>
                <tr className="border-b border-surface-700">
                  <th className="px-3 py-2 text-left text-[10px] font-semibold uppercase tracking-wider text-text-dim">
                    用户
                  </th>
                  <th className="px-3 py-2 text-left text-[10px] font-semibold uppercase tracking-wider text-text-dim">
                    角色
                  </th>
                  <th className="px-3 py-2 text-left text-[10px] font-semibold uppercase tracking-wider text-text-dim">
                    创建
                  </th>
                  <th className="px-3 py-2 text-right text-[10px] font-semibold uppercase tracking-wider text-text-dim">
                    操作
                  </th>
                </tr>
              </thead>
              <tbody>
                {error ? (
                  <tr>
                    <td colSpan={4} className="px-3 py-3 text-[11px] font-mono text-status-error">
                      加载失败: {error}
                    </td>
                  </tr>
                ) : users === null ? (
                  <tr>
                    <td colSpan={4} className="px-3 py-3 text-[11px] font-mono text-text-dim">
                      loading…
                    </td>
                  </tr>
                ) : users.length === 0 ? (
                  <tr>
                    <td colSpan={4} className="px-3 py-3 text-[11px] font-mono text-text-dim">
                      还没有用户。添加一个吧。
                    </td>
                  </tr>
                ) : (
                  users.map((u) => (
                    <tr
                      key={u.id}
                      className="border-b border-surface-800 last:border-b-0 hover:bg-surface-800/40"
                    >
                      <td className="px-3 py-2.5">
                        <div className="flex flex-col">
                          <span className="font-medium text-text-primary">{u.handle}</span>
                          <span className="font-mono text-[10px] text-text-dim">
                            {u.linked_chat ? `IM: ${u.linked_chat}` : "未绑定 IM"}
                          </span>
                        </div>
                      </td>
                      <td className="px-3 py-2.5">
                        <Badge variant="accent">tenant</Badge>
                      </td>
                      <td className="px-3 py-2.5 font-mono text-[11px] text-text-dim">
                        {u.created_at ? u.created_at.slice(0, 10) : "—"}
                      </td>
                      <td className="px-3 py-2.5 text-right">
                        {confirmDelete === u.id ? (
                          <span className="inline-flex items-center gap-1.5">
                            <span className="text-[11px] font-mono text-status-error">删除?</span>
                            <Button
                              variant="destructive"
                              size="sm"
                              onClick={() => onDelete(u.id)}
                            >
                              确认
                            </Button>
                            <Button
                              variant="ghost"
                              size="sm"
                              onClick={() => setConfirmDelete(null)}
                            >
                              取消
                            </Button>
                          </span>
                        ) : (
                          <span className="inline-flex items-center gap-1.5">
                            <Button
                              variant="ghost"
                              size="sm"
                              onClick={() => revealAndCopy(u.id)}
                            >
                              复制链接
                            </Button>
                            <Button
                              variant="ghost"
                              size="sm"
                              onClick={() => setConfirmDelete(u.id)}
                            >
                              删除
                            </Button>
                          </span>
                        )}
                      </td>
                    </tr>
                  ))
                )}
              </tbody>
            </table>

            <div className="border-t border-surface-800 px-3 py-2.5">
              <form onSubmit={onCreate} className="flex items-end gap-2">
                <div className="flex flex-1 flex-col gap-1.5">
                  <Label htmlFor="settings-user-handle">新用户名 / Handle</Label>
                  <Input
                    id="settings-user-handle"
                    type="text"
                    autoComplete="off"
                    value={handle}
                    onChange={(e) => setHandle(e.target.value)}
                    disabled={creating}
                    spellCheck={false}
                    placeholder="alice"
                    className="font-mono"
                  />
                </div>
                <Button
                  type="submit"
                  size="default"
                  disabled={creating || handle.trim().length === 0}
                >
                  {creating ? "…" : "添加用户"}
                </Button>
              </form>
            </div>
          </Card>

          <p className="flex items-start gap-1.5 text-[10px] font-mono text-text-dim leading-relaxed">
            <Link2 className="size-3.5 shrink-0 translate-y-0.5" />
            <span>
              新增用户铸一次性链接 <span className="text-text-secondary">?token=ccteam:&lt;hex&gt;</span>{" "}
              —— 只显一次,复制给本人;列表永不回 token。诚实边界:同一台机器、同一个 OS 账号下是
              <b className="text-text-secondary">软隔离(UX)</b>、不是安全边界。
            </span>
          </p>
        </div>
      )}
    </section>
  );
}
