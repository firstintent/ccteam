// v0.8.8 F4 — Settings page: configure IM credentials (Telegram + Lark) +
// per-user (tenant) management from the web UI.
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
//   - Admin-gate: this page is admin-only (IM creds + user management are
//     GLOBAL daemon config). A tenant gets a pointer to the avatar menu.
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
  getImConfig,
  pollTelegramChatId,
  saveLark,
  saveTelegramToken,
  startTelegramChatId,
  type ChatIdPollStatus,
  type ImConfigStatus,
} from "../lib/configApi";
import { toastBus } from "../lib/toastBus";
import {
  createUser,
  deleteUser,
  getMyLarkOpenIdCandidates,
  getUserLink,
  listUsers,
  putMyIm,
  putMyLarkAllowedUsers,
  type LarkOpenIdCandidate,
  type PutMyImForm,
  type TenantView,
} from "../lib/usersApi";
import { useMe } from "../hooks/useMe";
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

/**
 * Legacy clipboard copy via a temporary off-screen <textarea> +
 * `document.execCommand("copy")`. Used as a fallback when `navigator.clipboard`
 * is unavailable — e.g. the daemon served over plain http:// on a remote IP
 * (non-secure context). Returns `true` on success.
 */
function legacyCopy(text: string): boolean {
  if (typeof document === "undefined") return false;
  const ta = document.createElement("textarea");
  ta.value = text;
  // Keep it off-screen and unfocusable visually, but still selectable.
  ta.setAttribute("readonly", "");
  ta.style.position = "fixed";
  ta.style.top = "-9999px";
  ta.style.left = "-9999px";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  try {
    ta.select();
    ta.setSelectionRange(0, text.length); // iOS / mobile Safari
    return document.execCommand("copy");
  } catch {
    return false;
  } finally {
    document.body.removeChild(ta);
  }
}

export default function SettingsPage() {
  // v0.8.18 档1 — this page is admin-only (IM credentials + user management are
  // GLOBAL daemon config); a tenant gets a pointer to the avatar menu instead.
  const { me } = useMe();
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
    // Tenants never load the global IM config (the endpoint 403s for them).
    if (!me?.is_admin) return;
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
  }, [me]);

  // While the identity loads (and the test's never-resolving fetch), show the
  // loading placeholder.
  if (me === null) {
    return (
      <div data-testid="settings-loading" className="p-4 text-xs text-text-dim font-mono">
        loading settings…
      </div>
    );
  }
  // v0.8.20 F2 — a per-user tenant's Settings page is its self-serve "我的 IM
  // bot" (admin-only IM credentials / user management are not shown). Personal
  // display settings stay in the top-right avatar menu.
  if (!me.is_admin) {
    return (
      <div
        data-testid="settings-tenant"
        className="p-4 sm:p-6 max-w-3xl mx-auto flex flex-col gap-6"
      >
        <MyImSection />
        <p className="text-[11px] font-mono text-text-dim leading-relaxed">
          你的个人设置(显示名 / 头像 / 界面语言 / 登出)在
          <b className="text-text-secondary">侧栏底部头像菜单</b>里（设置→账号将收编同一入口）。
        </p>
      </div>
    );
  }
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
      <div data-testid="settings-loading" className="p-4 text-xs text-text-dim font-mono">
        loading settings…
      </div>
    );
  }

  return (
    <div data-testid="settings-page" className="p-4 sm:p-6 max-w-3xl mx-auto flex flex-col gap-6">
      {/* v0.8.24 A4 — Settings secondary nav (六子页收编：主机/市场/Status/IM/通用/账号). */}
      <nav
        data-testid="settings-tabs"
        className="flex flex-wrap gap-1 border-b border-surface-700/40 pb-2"
      >
        {[
          { href: "/settings", label: "IM 接入" },
          { href: "/hosts", label: "主机" },
          { href: "/marketplace", label: "插件市场" },
          { href: "/status", label: "Status" },
        ].map((t) => (
          <a
            key={t.href}
            href={t.href}
            className={`h-8 px-3 rounded-md text-xs font-medium ${
              t.href === "/settings"
                ? "bg-surface-700 text-text-primary"
                : "text-text-secondary hover:bg-surface-800"
            }`}
          >
            {t.label}
          </a>
        ))}
      </nav>

      <section className="flex flex-col gap-3 rounded-lg border border-surface-700/50 p-4">
        <SectionHeading
          title="通用 · 账号"
          badge="侧栏头像"
          subtitle="语言 / 主题 / 头像 / 昵称 / 登出 — 使用侧栏底部头像菜单（与原型设置→通用/账号同入口）。"
        />
      </section>

      <section className="flex flex-col gap-4">
        <SectionHeading
          title="IM 凭据 · Credentials"
          badge="管理员 · 全局"
          subtitle="连一个聊天通道,ccteam 才能找到你。下次重启生效。"
        />

        {config.transport_warning ? (
          <div
            data-testid="settings-transport-warning"
            role="status"
            className="text-[11px] font-mono text-brand-400 bg-brand-500/10 border border-brand-500/30 rounded-lg px-3 py-2"
          >
            {config.transport_warning}
          </div>
        ) : null}

        <TelegramSection status={config.telegram} onSaved={reload} />
        <LarkSection status={config.lark} onSaved={reload} />
      </section>

      <UserManagementSection />
    </div>
  );
}

// --------------------------------------------------------------------------
// Shared chrome
// --------------------------------------------------------------------------

/** A section heading: title + an admin-scope badge + a one-line lede. */
function SectionHeading({
  title,
  badge,
  subtitle,
}: {
  title: string;
  badge: string;
  subtitle: string;
}) {
  return (
    <div className="flex flex-col gap-1">
      <div className="flex items-center gap-2">
        <h2 className="text-sm font-semibold text-text-primary">{title}</h2>
        <Badge variant="idle">{badge}</Badge>
      </div>
      <p className="text-[11px] text-text-muted">{subtitle}</p>
    </div>
  );
}

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

/** The right-side read-out column (a hairline-separated label→value stack). */
function Readout({ children }: { children: React.ReactNode }) {
  return (
    <div className="flex flex-col gap-2 text-[11px] sm:border-l sm:border-surface-800 sm:pl-4">
      {children}
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
        toastBus.handler?.error(err instanceof Error ? err.message : "could not restart capture");
        setChatIdStatus("error");
      });
  }

  const bound = (status?.chat_id_count ?? 0) > 0;

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

      <CardContent className="grid gap-5 sm:grid-cols-[1fr_220px]">
        <div className="flex flex-col gap-3">
          <form onSubmit={handleSubmit} className="flex flex-col gap-2">
            <Label htmlFor="settings-telegram-token">
              {configured ? "重置 bot token" : "Bot token"}
            </Label>
            {/* Red line: the password field always starts EMPTY — the
                fingerprint is text-only, never the input value. */}
            <Input
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
                <Button
                  type="submit"
                  size="sm"
                  disabled={pending || token.trim().length === 0}
                >
                  {pending ? "保存中…" : configured ? "重置 token" : "保存 token"}
                </Button>
              )}
            </div>
          </form>
          <p className="text-[10px] font-mono text-text-dim leading-relaxed">
            token 永不回显,仅显末 4 位;重置走两步确认(破坏性)。从 @BotFather 取 token,保存后 DM 机器人绑定你的 chat。
          </p>
        </div>

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
      </CardContent>

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

      <CardContent className="grid gap-5 sm:grid-cols-[1fr_220px]">
        <form onSubmit={handleSubmit} className="flex flex-col gap-3">
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

          <fieldset className="flex flex-col gap-1.5">
            <legend className="text-xs font-medium text-text-dim pb-1">区域 · Region</legend>
            <label className="flex items-center gap-2 text-[11px] font-mono text-text-secondary cursor-pointer">
              <input
                type="radio"
                name="settings-lark-region"
                checked={useFeishu}
                onChange={() => setUseFeishu(true)}
                disabled={pending}
                className="accent-brand-500"
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
                className="accent-brand-500"
              />
              Lark (international)
            </label>
          </fieldset>

          <div className="flex flex-col gap-1.5">
            <Label htmlFor="settings-lark-users">允许的 open_id(逗号或换行分隔)</Label>
            <Textarea
              id="settings-lark-users"
              value={userIdsRaw}
              onChange={(e) => setUserIdsRaw(e.target.value)}
              disabled={pending}
              rows={3}
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
              <Button type="submit" size="sm" disabled={!canSubmit}>
                {pending ? "保存中…" : configured ? "重置凭据" : "保存"}
              </Button>
            )}
          </div>
        </form>

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
      </CardContent>

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
// user writes live here on the web.
// --------------------------------------------------------------------------

// --------------------------------------------------------------------------
// v0.8.20 F2 — a tenant's OWN IM bot (self-serve). The owner's bot is the
// global one (admin Settings); a per-user tenant runs its own bot so its
// Telegram/Lark drives ONLY its sessions, not a shared admin bot.
// --------------------------------------------------------------------------

function MyImSection() {
  const [telegram, setTelegram] = useState("");
  const [larkOpen, setLarkOpen] = useState(false);
  const [larkAppId, setLarkAppId] = useState("");
  const [larkSecret, setLarkSecret] = useState("");
  const [larkUsersRaw, setLarkUsersRaw] = useState("");
  const [useFeishu, setUseFeishu] = useState(true);
  const [larkCaptureSince, setLarkCaptureSince] = useState<number | null>(null);
  const [larkCandidates, setLarkCandidates] = useState<LarkOpenIdCandidate[]>([]);
  const [pending, setPending] = useState(false);
  const [saved, setSaved] = useState(false);
  const larkCaptureTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const larkUserIds = parseUserIds(larkUsersRaw);

  useEffect(() => {
    if (larkCaptureSince === null) return;
    let cancelled = false;

    const tick = () => {
      getMyLarkOpenIdCandidates(larkCaptureSince)
        .then((res) => {
          if (cancelled) return;
          setLarkCandidates(res.candidates);
          larkCaptureTimerRef.current = setTimeout(tick, CHAT_ID_POLL_MS);
        })
        .catch((err) => {
          if (cancelled) return;
          if (!(err instanceof Error) || err.message !== "UNAUTHENTICATED") {
            toastBus.handler?.error(err instanceof Error ? err.message : "open_id capture failed");
          }
          larkCaptureTimerRef.current = setTimeout(tick, CHAT_ID_POLL_MS);
        });
    };
    larkCaptureTimerRef.current = setTimeout(tick, 300);
    return () => {
      cancelled = true;
      if (larkCaptureTimerRef.current) {
        clearTimeout(larkCaptureTimerRef.current);
        larkCaptureTimerRef.current = null;
      }
    };
  }, [larkCaptureSince]);

  function startLarkCapture() {
    setLarkCandidates([]);
    setLarkCaptureSince(Math.floor(Date.now() / 1000) - 2);
    toastBus.handler?.info("私聊 Lark / 飞书 bot,或在群里 @ bot,这里会出现 open_id");
  }

  async function onSave(e: React.FormEvent) {
    e.preventDefault();
    if (pending) return;
    const tok = telegram.trim();
    const larkApp = larkAppId.trim();
    const larkSecretValue = larkSecret.trim();
    if (larkOpen && (larkApp || larkSecretValue) && !(larkApp && larkSecretValue)) {
      toastBus.handler?.error("Lark App ID 和 App Secret 需要一起填写");
      return;
    }
    if (larkOpen && !larkApp && !larkSecretValue && larkUserIds.length > 0) {
      if (tok) {
        toastBus.handler?.error("请先单独保存 Lark open_id,再保存 Telegram token");
        return;
      }
      void saveLarkAllowlist(larkUserIds);
      return;
    }
    setPending(true);
    setSaved(false);
    const form: PutMyImForm = {};
    if (tok) form.telegram_bot_token = tok;
    if (larkOpen && larkApp && larkSecretValue) {
      form.lark = {
        app_id: larkApp,
        app_secret: larkSecretValue,
        allowed_user_ids: larkUserIds,
        use_feishu: useFeishu,
      };
    }
    try {
      await putMyIm(form);
      setSaved(true);
      setTelegram("");
      setLarkAppId("");
      setLarkSecret("");
      if (larkOpen && larkUserIds.length === 0) {
        startLarkCapture();
      }
      toastBus.handler?.info("已保存 / Saved");
    } catch (err) {
      if (err instanceof Error && err.message === "UNAUTHENTICATED") return;
      toastBus.handler?.error(err instanceof Error ? err.message : "save failed");
    } finally {
      setPending(false);
    }
  }

  async function saveLarkAllowlist(ids: string[]) {
    if (pending) return;
    const normalized = Array.from(new Set(ids.map((id) => id.trim()).filter(Boolean))).sort();
    setPending(true);
    try {
      const res = await putMyLarkAllowedUsers(normalized);
      setLarkUsersRaw(normalized.join("\n"));
      setSaved(true);
      setLarkCaptureSince(null);
      setLarkCandidates([]);
      toastBus.handler?.info(res.note || "open_id 已保存到 allowlist");
    } catch (err) {
      if (err instanceof Error && err.message === "UNAUTHENTICATED") return;
      toastBus.handler?.error(err instanceof Error ? err.message : "save failed");
    } finally {
      setPending(false);
    }
  }

  async function saveCapturedOpenId(openId: string) {
    const merged = Array.from(new Set([...larkUserIds, openId])).sort();
    await saveLarkAllowlist(merged);
  }

  return (
    <section data-testid="settings-my-im" className="flex flex-col gap-4">
      <div className="flex flex-col gap-1">
        <div className="flex items-center gap-2">
          <MessageSquare className="size-4 text-text-secondary" />
          <h2 className="text-sm font-semibold text-text-primary">我的 IM bot · My bot</h2>
          <Badge variant="accent">自助</Badge>
        </div>
        <p className="text-[11px] text-text-muted leading-relaxed">
          配置你自己的 Telegram / Lark 机器人 —— 它只驱动你自己的 session(不再共用管理员的全局 bot)。
          保存会<b className="text-text-secondary">替换</b>当前配置;留空即清除。下次该 bot 监听重启后生效。
        </p>
      </div>

      <Card className="p-4">
        <form onSubmit={onSave} className="flex flex-col gap-3">
          <div className="flex flex-col gap-1.5">
            <Label htmlFor="my-im-telegram">Telegram bot token</Label>
            <Input
              id="my-im-telegram"
              type="password"
              autoComplete="off"
              value={telegram}
              onChange={(e) => setTelegram(e.target.value)}
              disabled={pending}
              spellCheck={false}
              placeholder="123456:ABC-DEF…"
              className="font-mono"
            />
            <p className="text-[10px] text-text-dim">
              从 @BotFather 拿一个新 bot 的 token(每个 bot 的 token 唯一,保存前会校验)。
            </p>
          </div>

          <button
            type="button"
            onClick={() => setLarkOpen((v) => !v)}
            className="self-start text-[11px] text-text-dim hover:text-text-secondary"
          >
            {larkOpen ? "▾" : "▸"} Lark / 飞书(可选)
          </button>
          {larkOpen ? (
            <div className="flex flex-col gap-2 rounded-md border border-surface-800 p-3">
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="my-im-lark-id">Lark App ID</Label>
                <Input
                  id="my-im-lark-id"
                  value={larkAppId}
                  onChange={(e) => setLarkAppId(e.target.value)}
                  disabled={pending}
                  spellCheck={false}
                  placeholder="cli_…"
                  className="font-mono"
                />
              </div>
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="my-im-lark-secret">Lark App Secret</Label>
                <Input
                  id="my-im-lark-secret"
                  type="password"
                  value={larkSecret}
                  onChange={(e) => setLarkSecret(e.target.value)}
                  disabled={pending}
                  spellCheck={false}
                  className="font-mono"
                />
              </div>
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
              <div className="flex flex-col gap-1.5">
                <Label htmlFor="my-im-lark-users">允许的 open_id</Label>
                <Textarea
                  id="my-im-lark-users"
                  value={larkUsersRaw}
                  onChange={(e) => setLarkUsersRaw(e.target.value)}
                  disabled={pending}
                  rows={3}
                  spellCheck={false}
                  placeholder="ou_abc…"
                  className="font-mono"
                />
                {larkUserIds.length === 0 ? (
                  <p className="text-[10px] text-status-error">
                    空 allowlist = fail-closed。保存 App 后可在下方发现自己的 open_id。
                  </p>
                ) : (
                  <p className="text-[10px] text-text-dim">
                    {larkUserIds.length} 个 open_id 将被允许。
                  </p>
                )}
              </div>
              <div className="flex flex-col gap-2 rounded-md border border-surface-800 bg-surface-950/40 p-2">
                <div className="flex items-center justify-between gap-2">
                  <span className="text-[11px] font-medium text-text-secondary">
                    发现 open_id
                  </span>
                  <Button
                    type="button"
                    variant="ghost"
                    size="sm"
                    onClick={startLarkCapture}
                    disabled={pending}
                  >
                    开始
                  </Button>
                </div>
                {larkCaptureSince !== null ? (
                  <p className="text-[10px] text-text-dim">
                    现在私聊这个 Lark / 飞书 bot,或在群里 @ bot;消息会被拒绝,只用于显示 sender
                    open_id。
                  </p>
                ) : null}
                {larkCandidates.length > 0 ? (
                  <div className="flex flex-col gap-1">
                    {larkCandidates.map((c) => (
                      <button
                        key={`${c.open_id}:${c.message_id}`}
                        type="button"
                        onClick={() => void saveCapturedOpenId(c.open_id)}
                        disabled={pending}
                        className="flex items-center justify-between gap-2 rounded border border-surface-800 px-2 py-1 text-left text-[11px] font-mono text-text-secondary hover:border-brand-500 hover:text-text-primary"
                      >
                        <span>{c.open_id}</span>
                        <span className="text-[10px] text-text-dim">填入并保存</span>
                      </button>
                    ))}
                  </div>
                ) : larkCaptureSince !== null ? (
                  <p className="text-[10px] font-mono text-text-dim">等待消息…</p>
                ) : null}
              </div>
            </div>
          ) : null}

          <div className="flex items-center gap-2">
            <Button type="submit" disabled={pending}>
              {pending ? "保存中…" : "保存"}
            </Button>
            {saved ? (
              <span className="text-[11px] font-mono text-status-running">已保存 ✓</span>
            ) : null}
          </div>
        </form>
      </Card>
    </section>
  );
}

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
    const ok = () => toastBus.handler?.info("链接已复制 / Link copied");
    const fail = () => toastBus.handler?.error("复制失败,请手动复制 / Copy failed, please copy manually");

    // The daemon is often served over plain http:// on a remote IP (not https /
    // not localhost), where `navigator.clipboard` is undefined. Try the async
    // Clipboard API first, then fall back to a temporary off-screen <textarea>
    // + document.execCommand("copy") so the button works in non-secure contexts.
    if (typeof navigator !== "undefined" && navigator.clipboard) {
      navigator.clipboard.writeText(link).then(ok, () => {
        if (!legacyCopy(link)) fail();
        else ok();
      });
    } else if (legacyCopy(link)) {
      ok();
    } else {
      fail();
    }
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
