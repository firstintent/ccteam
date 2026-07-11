// v0.8.24 Track A — 设置 top-level view (prototype `#view-settings`):
// set-nav second column with SIX sub-pages — 主机 / 插件市场 / Status /
// IM 接入 / 通用 / 账号.
//
// ACL (红线 §1.6-3, fail-closed via useMe): 主机 / Status / IM 接入 are
// admin-only nav items — a tenant sees ONLY 插件市场 / 通用 / 账号 (the
// backend 403s regardless; this is the UI层 beta/visibility gate). The 账号
// panel absorbs the old AvatarMenu entirely (头像 / 昵称 / 语言入口 / 登出 +
// web token), and carries the tenant's self-serve 「我的 IM bot」.

import { useMemo, useState } from "react";
import {
  Activity,
  MessageSquare,
  Package,
  Server,
  SlidersHorizontal,
  User,
} from "lucide-react";
import HostsView from "./HostsView";
import MarketplaceView from "./MarketplaceView";
import StatusView from "./StatusView";
import SettingsPage, { MyImSection } from "./SettingsPage";
import { makeT, type Lang } from "../lib/i18n";
import { useWebSettings } from "../hooks/useWebSettings";
import { useMe } from "../hooks/useMe";
import { clearToken, getToken } from "../lib/token";
import { toastBus } from "../lib/toastBus";
import type { SessionView as SessionSummary } from "../lib/sessionsApi";

export type SettingsTab = "hosts" | "market" | "status" | "im" | "general" | "account";

const ITEMS: { id: SettingsTab; labelKey: string; adminOnly: boolean; icon: React.ReactNode }[] = [
  { id: "hosts", labelKey: "setHosts", adminOnly: true, icon: <Server /> },
  { id: "market", labelKey: "setMarket", adminOnly: false, icon: <Package /> },
  { id: "status", labelKey: "setStatus", adminOnly: true, icon: <Activity /> },
  { id: "im", labelKey: "setIm", adminOnly: true, icon: <MessageSquare /> },
  { id: "general", labelKey: "setGeneral", adminOnly: false, icon: <SlidersHorizontal /> },
  { id: "account", labelKey: "setAccount", adminOnly: false, icon: <User /> },
];

/** Visible nav items for the caller — fail-closed: admin-only panels are
 *  listed only once `/me` resolves is_admin (never flashed to a tenant). */
// eslint-disable-next-line react-refresh/only-export-components -- pure helper co-located for unit tests.
export function visibleSettingsItems(isAdmin: boolean): SettingsTab[] {
  return ITEMS.filter((it) => isAdmin || !it.adminOnly).map((it) => it.id);
}

/** Resolve the routed tab against the caller's visible items. */
// eslint-disable-next-line react-refresh/only-export-components -- pure helper co-located for unit tests.
export function resolveSettingsTab(tab: string | undefined, isAdmin: boolean): SettingsTab {
  const visible = visibleSettingsItems(isAdmin);
  if (tab && (visible as string[]).includes(tab)) return tab as SettingsTab;
  return isAdmin ? "hosts" : "market";
}

const AVATARS = ["#f59e0b", "#3b82f6", "#22c55e", "#a855f7", "#64748b"];

export default function SettingsView({
  tab: routeTab,
  onNav,
  rail = [],
}: {
  tab?: string;
  onNav?: (tab: SettingsTab) => void;
  rail?: SessionSummary[];
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
        <div className={`set-detail-inner fade-in ${active === "market" || active === "status" ? "wide" : ""}`} key={active}>
          {active === "hosts" && isAdmin ? <HostsView lang={lang} /> : null}

          {active === "market" ? (
            <>
              <header>
                <h1>{t("setMarket")}</h1>
                <p>{t("marketDesc")}</p>
              </header>
              <MarketplaceView embedded />
            </>
          ) : null}

          {active === "status" && isAdmin ? <StatusView rail={rail} /> : null}

          {active === "im" && isAdmin ? (
            <>
              <header>
                <h1>{t("setIm")}</h1>
                <p>{t("imDesc")}</p>
              </header>
              <SettingsPage />
            </>
          ) : null}

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
    if (typeof navigator !== "undefined" && navigator.clipboard) {
      void navigator.clipboard.writeText(token).then(
        () => toastBus.handler?.info(lang === "en" ? "Token copied" : "token 已复制"),
        () => toastBus.handler?.error(lang === "en" ? "Copy failed" : "复制失败"),
      );
    }
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
          <div style={{ display: "flex", gap: 8 }}>
            <button type="button" className="btn ghost mini" onClick={copyToken}>
              {lang === "en" ? "Copy token" : "复制 token"}
            </button>
          </div>
        </div>
        <div>
          <button type="button" className="btn ghost" data-testid="account-logout" onClick={logout}>
            {t("logout")}
          </button>
        </div>
      </div>

      {/* Tenant self-serve IM bot (v0.8.20 F2) lives under 账号 — the global
          IM credentials stay admin-only under 设置→IM 接入. */}
      {!isAdmin ? <MyImSection /> : null}
    </div>
  );
}
