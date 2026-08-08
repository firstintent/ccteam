// v0.8.24 Track A — 设置 top-level view (prototype `#view-settings`):
// set-nav second column of sub-pages. Every identity sees 运维总览 / 接入 /
// 通用 / 账号; only an admin sees 管理员. 运维总览 combines Status + Hosts;
// 管理员 carries user management.
//
// ACL (fail-closed via useMe): only 管理员 is an admin-only nav item. The 账号
// panel absorbs the old AvatarMenu entirely (头像 / 昵称 / 语言入口 / 登出 +
// web token); tenant self-serve IM and global admin credentials live as the
// two identity-specific shapes of Access.

import { useCallback, useEffect, useMemo, useState } from "react";
import {
  Activity,
  FolderMinus,
  PlugZap,
  SlidersHorizontal,
  User,
  Users,
} from "lucide-react";
import HostsView from "./HostsView";
import StatusView from "./StatusView";
import { UserManagementSection } from "./SettingsPage";
import AccessView from "./AccessView";
import { copyText } from "../lib/clipboard";
import { deleteProject, fetchDashboard, type DashboardRow } from "../lib/dashboardApi";
import { makeT, type Lang } from "../lib/i18n";
import { useWebSettings } from "../hooks/useWebSettings";
import { useMe } from "../hooks/useMe";
import { clearToken, getToken, saveToken } from "../lib/token";
import { resetToken } from "../lib/meApi";
import { toastBus } from "../lib/toastBus";

export type SettingsTab = "ops" | "access" | "general" | "account" | "admin";

const ITEMS: { id: SettingsTab; labelKey: string; adminOnly: boolean; icon: React.ReactNode }[] = [
  { id: "ops", labelKey: "setOps", adminOnly: false, icon: <Activity /> },
  { id: "access", labelKey: "setAccess", adminOnly: false, icon: <PlugZap /> },
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
  return "ops";
}

// eslint-disable-next-line react-refresh/only-export-components -- pure helper co-located for unit tests.
export function settingsDetailWidthClass(active: SettingsTab): string {
  if (active === "ops") return "ops-wide";
  if (active === "access") return "access-wide";
  return "";
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
          className={`set-detail-inner fade-in ${settingsDetailWidthClass(active)}`}
          key={active}
        >
          {active === "ops" ? <OpsPanel lang={lang} /> : null}
          {active === "access" ? <AccessView lang={lang} /> : null}

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
      <section className="ops-panel" aria-label="Projects">
        <ProjectsPanel lang={lang} />
      </section>
      <section className="ops-panel" aria-label="Hosts">
        <HostsView embedded lang={lang} />
      </section>
    </div>
  );
}

/** 项目 · Projects — catalog management inside 运维总览. One row per
 *  registered project; 移除 arms an inline type-the-slug confirm (mistype-proof
 *  for a destructive-looking action that is deliberately NOT destructive on
 *  disk: `DELETE /api/v1/projects/{slug}` deregisters + stops live sessions
 *  only — the working tree stays, and the copy says so). Hook-free row view
 *  exported for node-env tests; this container owns arming/typing state. */
export function ProjectsPanel({ lang }: { lang: Lang }) {
  const t = makeT(lang);
  const [rows, setRows] = useState<DashboardRow[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  /** Slug whose remove-confirm is open (one at a time), + its typed echo. */
  const [arming, setArming] = useState<string | null>(null);
  const [typed, setTyped] = useState("");
  const [busy, setBusy] = useState<string | null>(null);

  const load = useCallback(() => {
    fetchDashboard()
      .then((next) => {
        setRows(next);
        setError(null);
      })
      .catch((e) => {
        if (e instanceof Error && e.message === "UNAUTHENTICATED") return;
        setError(e instanceof Error ? e.message : "load failed");
      });
  }, []);
  useEffect(() => load(), [load]);

  const arm = (slug: string) => {
    setArming(slug);
    setTyped("");
  };
  const cancel = () => {
    setArming(null);
    setTyped("");
  };
  const confirm = async (slug: string) => {
    if (busy) return;
    setBusy(slug);
    try {
      const res = await deleteProject(slug);
      const stopped = res.sessions_stopped.length;
      toastBus.handler?.info(
        lang === "en"
          ? `Removed ${slug} from ccteam (${stopped} live session${stopped === 1 ? "" : "s"} stopped) — files on disk untouched.`
          : `已从 ccteam 移除 ${slug}(停止 ${stopped} 个 live 会话)—— 磁盘文件未动。`,
      );
      cancel();
      load();
    } catch (e) {
      if (!(e instanceof Error && e.message === "UNAUTHENTICATED")) {
        toastBus.handler?.error(
          `${lang === "en" ? "Remove failed" : "移除失败"}: ${e instanceof Error ? e.message : "unknown"}`,
        );
      }
    } finally {
      setBusy(null);
    }
  };

  return (
    <div className="hosts-stack" data-testid="projects-panel">
      <header className="hosts-head-bar">
        <div className="hosts-head-copy">
          <h2 className="hosts-section-title">{t("projectsTitle")}</h2>
          <p className="hosts-head-desc">{t("projectsDesc")}</p>
        </div>
      </header>
      {error ? (
        <div role="alert" className="badge warn" style={{ padding: "8px 12px", borderRadius: 10, fontSize: 12.5 }}>
          {t("projectsLoadFailed")}: {error}
        </div>
      ) : rows === null ? (
        <p data-testid="projects-loading" style={{ fontSize: 13, color: "var(--text-faint)" }}>
          {t("loading")}
        </p>
      ) : rows.length === 0 ? (
        <p style={{ fontSize: 13, color: "var(--text-faint)" }}>{t("projectsEmpty")}</p>
      ) : (
        rows.map((row) => (
          <ProjectRemoveRow
            key={row.slug}
            row={row}
            lang={lang}
            arming={arming === row.slug}
            typed={arming === row.slug ? typed : ""}
            busy={busy === row.slug}
            onArm={() => arm(row.slug)}
            onCancel={cancel}
            onTyped={setTyped}
            onConfirm={() => void confirm(row.slug)}
          />
        ))
      )}
    </div>
  );
}

/** One project row + its inline remove confirm. Hook-free (state lives in
 *  {@link ProjectsPanel}) so node-env tests can walk it and fire callbacks. */
export function ProjectRemoveRow({
  row,
  lang = "zh",
  arming,
  typed,
  busy,
  onArm,
  onCancel,
  onTyped,
  onConfirm,
}: {
  row: DashboardRow;
  lang?: Lang;
  arming: boolean;
  typed: string;
  busy: boolean;
  onArm: () => void;
  onCancel: () => void;
  onTyped: (value: string) => void;
  onConfirm: () => void;
}) {
  const t = makeT(lang);
  return (
    <div className="host-actions" data-testid={`project-row-${row.slug}`}>
      <div className="host-actions-head">
        <span className="host-actions-name mono">{row.slug}</span>
        {row.host !== "local" ? (
          <span className="host-actions-id mono">@ {row.host}</span>
        ) : null}
        {row.broken ? <span className="badge warn">orphaned</span> : null}
        <span className="host-actions-id mono" title={row.path} style={{ marginLeft: "auto" }}>
          {row.path}
        </span>
        {!arming ? (
          <button
            type="button"
            className="btn ghost mini"
            data-testid={`project-remove-${row.slug}`}
            disabled={busy}
            onClick={onArm}
          >
            <FolderMinus aria-hidden="true" style={{ width: 13, height: 13 }} />
            {t("projectRemove")}
          </button>
        ) : null}
      </div>
      {arming ? (
        <div
          className="host-actions-items"
          data-testid={`project-remove-confirm-${row.slug}`}
          style={{ alignItems: "center" }}
        >
          <span className="host-actions-idle">{t("projectRemoveHint")}</span>
          <label className="host-actions-idle" htmlFor={`project-remove-typed-${row.slug}`}>
            {t("projectRemoveType")}
          </label>
          <input
            id={`project-remove-typed-${row.slug}`}
            data-testid={`project-remove-typed-${row.slug}`}
            className="mono"
            value={typed}
            spellCheck={false}
            placeholder={row.slug}
            onChange={(e) => onTyped(e.target.value)}
            style={{ width: 160, fontSize: 12 }}
          />
          <button
            type="button"
            className="btn danger mini"
            data-testid={`project-remove-go-${row.slug}`}
            disabled={busy || typed.trim() !== row.slug}
            onClick={onConfirm}
          >
            {busy ? t("projectRemoveBusy") : t("projectRemoveConfirm")}
          </button>
          <button type="button" className="btn ghost mini" disabled={busy} onClick={onCancel}>
            {t("cancel")}
          </button>
        </div>
      ) : null}
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
  // v0.8.24 — self-serve web-token reset: two-step inline confirm
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
    </div>
  );
}
