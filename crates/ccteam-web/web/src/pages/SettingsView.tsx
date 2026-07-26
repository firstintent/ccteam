// v0.8.24 Track A — 设置 top-level view (prototype `#view-settings`):
// set-nav second column of sub-pages. Tabs: admin sees 运维总览 / 接入 /
// 通用 / 账号 / 管理员; a tenant sees ONLY 通用 / 账号. 运维总览
// combines Status + Hosts; 管理员 carries user management.
//
// ACL (红线 §1.6-3, fail-closed via useMe): 运维总览 / 管理员 are
// admin-only nav items (the backend 403s regardless; this is the UI层
// beta/visibility gate). The 账号 panel absorbs the old AvatarMenu entirely
// (头像 / 昵称 / 语言入口 / 登出 + web token); tenants keep the self-serve
// 「我的 IM bot」 there, while global Telegram/Lark credentials move to Access.

import { useMemo, useState } from "react";
import {
  Activity,
  PlugZap,
  SlidersHorizontal,
  User,
  Users,
} from "lucide-react";
import HostsView from "./HostsView";
import StatusView from "./StatusView";
import { MyImSection, UserManagementSection } from "./SettingsPage";
import AccessView from "./AccessView";
import { copyText } from "../lib/clipboard";
import { makeT, type Lang } from "../lib/i18n";
import { useWebSettings } from "../hooks/useWebSettings";
import { useMe } from "../hooks/useMe";
import { clearToken, getToken, saveToken } from "../lib/token";
import { resetToken } from "../lib/meApi";
import { toastBus } from "../lib/toastBus";

export type SettingsTab = "ops" | "access" | "general" | "account" | "admin";

const ITEMS: { id: SettingsTab; labelKey: string; adminOnly: boolean; icon: React.ReactNode }[] = [
  { id: "ops", labelKey: "setOps", adminOnly: true, icon: <Activity /> },
  { id: "access", labelKey: "setAccess", adminOnly: true, icon: <PlugZap /> },
  { id: "general", labelKey: "setGeneral", adminOnly: false, icon: <SlidersHorizontal /> },
  { id: "account", labelKey: "setAccount", adminOnly: false, icon: <User /> },
  { id: "admin", labelKey: "setAdmin", adminOnly: true, icon: <Users /> },
];

/** Visible nav items for the caller — fail-closed: admin-only panels are
 *  listed only once `/me` resolves is_admin (never flashed to a tenant). */
// eslint-disable-next-line react-refresh/only-export-components -- pure helper co-located for unit tests.
export function visibleSettingsItems(isAdmin: boolean): SettingsTab[] {
  return ITEMS.filter((it) => isAdmin || !it.adminOnly).map((it) => it.id);
}

/** Resolve the routed tab against the caller's visible items. Legacy routes:
 *  hosts/status → ops. Retired tabs are not aliased (pre-v1 no-shim policy). */
// eslint-disable-next-line react-refresh/only-export-components -- pure helper co-located for unit tests.
export function resolveSettingsTab(tab: string | undefined, isAdmin: boolean): SettingsTab {
  const visible = visibleSettingsItems(isAdmin);
  const normalized = tab === "hosts" || tab === "status" ? "ops" : tab;
  if (normalized && (visible as string[]).includes(normalized)) return normalized as SettingsTab;
  return isAdmin ? "ops" : "general";
}

const AVATARS = ["#f59e0b", "#3b82f6", "#22c55e", "#a855f7", "#64748b"];

export default function SettingsView({
  tab: routeTab,
  onNav,
}: {
  tab?: string;
  onNav?: (tab: SettingsTab) => void;
}) {
  const { settings, update } = useWebSettings();
  const lang = settings.language;
  const t = makeT(lang);
  const { me, isAdmin } = useMe();
  const [localTab, setLocalTab] = useState<SettingsTab | null>(null);
  const active = resolveSettingsTab(routeTab ?? localTab ?? undefined, isAdmin);

  const items = useMemo(() => ITEMS.filter((it) => isAdmin || !it.adminOnly), [isAdmin]);

  const pick = (id: SettingsTab) => {
    setLocalTab(id);
    onNav?.(id);
  };

  return (
    <section className="view active row" data-testid="settings-view">
      <div className="set-nav" data-testid="set-nav">
        <h2>{t("settings")}</h2>
        {items.map((it) => (
          <button
            key={it.id}
            type="button"
            data-testid={`set-item-${it.id}`}
            className={`set-item ${active === it.id ? "active" : ""}`}
            onClick={() => pick(it.id)}
          >
            {it.icon}
            {t(it.labelKey)}
          </button>
        ))}
      </div>

      <div className="set-detail">
        <div
          className={`set-detail-inner fade-in ${active === "ops" ? "ops-wide" : ""}`}
          key={active}
        >
          {active === "ops" && isAdmin ? <OpsPanel lang={lang} /> : null}
          {active === "access" && isAdmin ? <AccessView lang={lang} /> : null}

          {active === "admin" && isAdmin ? <AdminPanel lang={lang} /> : null}

          {active === "general" ? (
            <GeneralPanel
              lang={lang}
              theme={settings.theme}
              onLang={(l) => update({ language: l })}
              onTheme={(th) => update({ theme: th })}
            />
          ) : null}

          {active === "account" ? (
            <AccountPanel
              lang={lang}
              isAdmin={isAdmin}
              handle={me?.handle ?? null}
              displayName={settings.displayName}
              avatar={settings.avatar}
              onName={(n) => update({ displayName: n })}
              onAvatar={(a) => update({ avatar: a })}
            />
          ) : null}
        </div>
      </div>
    </section>
  );
}

/** Status · 运维总览: single vertical stack —
 *  daemon health first, then per-host agent cards (full width). */
export function OpsPanel({ lang }: { lang: Lang }) {
  const t = makeT(lang);
  return (
    <div className="ops-stack" data-testid="ops-view">
      <header>
        <h1>{t("setOps")}</h1>
        <p>{t("statusDesc")}</p>
      </header>
      <section className="ops-panel" aria-label="Status">
        <StatusView embedded />
      </section>
      <section className="ops-panel" aria-label="Hosts">
        <HostsView embedded lang={lang} />
      </section>
    </div>
  );
}

/** 管理员 · Admin — user management ONLY (「用户管理 · Users」). Rendered just
 *  for admins: the nav item is gated fail-closed, so a tenant never sees it. */
export function AdminPanel({ lang }: { lang: Lang }) {
  const t = makeT(lang);
  return (
    <div data-testid="settings-admin" className="flex flex-col gap-5">
      <header>
        <h1>{t("setAdmin")}</h1>
      </header>
      <UserManagementSection />
    </div>
  );
}

export function GeneralPanel({
  lang,
  theme,
  onLang,
  onTheme,
}: {
  lang: Lang;
  theme: "light" | "dark";
  onLang: (l: Lang) => void;
  onTheme: (t: "light" | "dark") => void;
}) {
  const t = makeT(lang);
  return (
    <div data-testid="settings-general" className="flex flex-col gap-5">
      <header>
        <h1>{t("setGeneral")}</h1>
      </header>
      <div className="form">
        <div className="field">
          <label>{t("language")}</label>
          <div className="seg" data-testid="lang-seg">
            <button
              type="button"
              data-testid="lang-zh"
              className={lang === "zh" ? "active" : ""}
              onClick={() => onLang("zh")}
            >
              中文
            </button>
            <button
              type="button"
              data-testid="lang-en"
              className={lang === "en" ? "active" : ""}
              onClick={() => onLang("en")}
            >
              English
            </button>
          </div>
        </div>
        <div className="field">
          <label>{t("theme")}</label>
          <div className="seg" data-testid="theme-seg">
            <button
              type="button"
              data-testid="theme-light"
              className={theme === "light" ? "active" : ""}
              onClick={() => onTheme("light")}
            >
              {t("light")}
            </button>
            <button
              type="button"
              data-testid="theme-dark"
              className={theme === "dark" ? "active" : ""}
              onClick={() => onTheme("dark")}
            >
              {t("dark")}
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}

/** Mask a token to its last 4 characters (never echo the full secret). */
// eslint-disable-next-line react-refresh/only-export-components -- pure helper co-located for unit tests.
export function maskToken(token: string | null): string {
  if (!token) return "—";
  const tail = token.slice(-4);
  return `••••••••${tail}`;
}

export function AccountPanel({
  lang,
  isAdmin,
  handle,
  displayName,
  avatar,
  onName,
  onAvatar,
}: {
  lang: Lang;
  isAdmin: boolean;
  handle: string | null;
  displayName: string;
  avatar: string;
  onName: (n: string) => void;
  onAvatar: (a: string) => void;
}) {
  const t = makeT(lang);
  const initial = ((displayName || "").trim() || handle || "C").slice(0, 1).toUpperCase();
  // v0.8.24 — admin self-serve web-token reset: two-step inline confirm
  // (arm → confirm), then store the NEW token locally at once (the old one
  // is already dead server-side; the fetch interceptor picks the new Bearer
  // up on the next request — session uninterrupted, no re-login).
  const [resetArmed, setResetArmed] = useState(false);
  const [resetBusy, setResetBusy] = useState(false);
  const [, setTokenBump] = useState(0);
  const doReset = async () => {
    if (!resetArmed) {
      setResetArmed(true);
      return;
    }
    setResetBusy(true);
    try {
      const { wire_token } = await resetToken();
      saveToken(wire_token);
      setTokenBump((n) => n + 1); // re-render the masked token field
      toastBus.handler?.info(
        lang === "en"
          ? "Web token rotated — this browser already uses the new one."
          : "web token 已重置 —— 本浏览器已自动换用新 token,无需重登。",
      );
    } catch (e) {
      if (!(e instanceof Error && e.message === "UNAUTHENTICATED")) {
        toastBus.handler?.error(
          `${lang === "en" ? "Reset failed" : "重置失败"}: ${e instanceof Error ? e.message : "unknown"}`,
        );
      }
    } finally {
      setResetBusy(false);
      setResetArmed(false);
    }
  };
  const logout = () => {
    clearToken();
    if (typeof window !== "undefined") window.location.reload();
  };
  const copyToken = () => {
    const token = getToken();
    if (!token) {
      toastBus.handler?.info(lang === "en" ? "No token stored in this browser" : "本浏览器未存 token");
      return;
    }
    void copyText(token).then((ok) =>
      ok
        ? toastBus.handler?.info(lang === "en" ? "Token copied" : "token 已复制")
        : toastBus.handler?.error(lang === "en" ? "Copy failed" : "复制失败"),
    );
  };
  return (
    <div data-testid="settings-account" className="flex flex-col gap-5">
      <header>
        <h1>{t("setAccount")}</h1>
        {handle ? (
          <p>
            @{handle}
            {isAdmin ? " · admin" : ""}
          </p>
        ) : null}
      </header>
      <div className="form">
        <div className="field">
          <label>{t("accAvatar")}</label>
          <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
            <span className="avatar" style={{ background: avatar }}>
              {initial}
            </span>
            <input
              type="text"
              data-testid="account-name"
              value={displayName}
              placeholder={handle ?? "you"}
              onChange={(e) => onName(e.target.value)}
              style={{ width: 200 }}
            />
          </div>
          <div style={{ display: "flex", alignItems: "center", gap: 6, marginTop: 4 }}>
            {AVATARS.map((a) => (
              <button
                key={a}
                type="button"
                data-testid={`avatar-swatch-${a}`}
                aria-pressed={avatar === a}
                aria-label={a}
                onClick={() => onAvatar(a)}
                style={{
                  width: 22,
                  height: 22,
                  borderRadius: "50%",
                  background: a,
                  border: "none",
                  opacity: avatar === a ? 1 : 0.5,
                  outline: avatar === a ? "2px solid var(--ink)" : "none",
                  outlineOffset: 2,
                }}
              />
            ))}
          </div>
        </div>
        <div className="field">
          <label>{t("accToken")}</label>
          <input type="password" readOnly value={maskToken(getToken())} data-testid="account-token" />
          <span className="hint">{t("accTokenHint")}</span>
          <div style={{ display: "flex", gap: 8, alignItems: "center" }}>
            <button type="button" className="btn ghost mini" onClick={copyToken}>
              {lang === "en" ? "Copy token" : "复制 token"}
            </button>
            {isAdmin ? (
              <button
                type="button"
                className={`btn mini ${resetArmed ? "primary" : "ghost"}`}
                data-testid="account-reset-token"
                disabled={resetBusy}
                onClick={() => void doReset()}
              >
                {resetBusy
                  ? lang === "en"
                    ? "Rotating…"
                    : "重置中…"
                  : resetArmed
                    ? lang === "en"
                      ? "Confirm reset? (old token dies at once)"
                      : "确认重置?(旧 token 立即失效)"
                    : lang === "en"
                      ? "Reset token"
                      : "重置 web token"}
              </button>
            ) : null}
            {resetArmed && !resetBusy ? (
              <button
                type="button"
                className="btn ghost mini"
                data-testid="account-reset-cancel"
                onClick={() => setResetArmed(false)}
              >
                {lang === "en" ? "Cancel" : "取消"}
              </button>
            ) : null}
          </div>
        </div>
        <div>
          <button type="button" className="btn ghost" data-testid="account-logout" onClick={logout}>
            {t("logout")}
          </button>
        </div>
      </div>

      {/* Tenant self-serve IM stays under Account. Global admin credentials
          live under the admin-only Access tab. */}
      {!isAdmin ? <MyImSection /> : null}
    </div>
  );
}
