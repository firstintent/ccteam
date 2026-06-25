// v0.8.18 柱2/UI — the avatar button + personal-settings popover (top bar).
//
// Personal settings (display name / avatar / interface language / theme /
// logout) live behind the avatar, distinct from the global/admin Settings
// page. Stored per-browser via useWebSettings in 档0 (no per-user web identity
// yet); 档1 ties them to the server identity.
//
// `AvatarPopover` is split out as a PURE, props-driven component so it is
// SSR-testable (the node-env vitest suite has no DOM / click events).
//
// v0.8.19 — emoji avatar swatches → color dots; added the single-icon
// light/dark theme toggle (Sun in dark = go light, Moon in light = go dark).

import { useState } from "react";
import { LogOut, Moon, Sun } from "lucide-react";
import { useWebSettings } from "../hooks/useWebSettings";
import { useMe } from "../hooks/useMe";
import { clearToken } from "../lib/token";
import { tr, type Lang } from "../lib/i18n";

/** The fixed avatar color palette (hex) — replaces the old emoji swatches. */
const AVATARS = ["#f59e0b", "#3b82f6", "#22c55e", "#a855f7", "#64748b"];

/** A stored avatar is a hex color; fall back to the default for any legacy
 *  (emoji) value so old localStorage never renders a broken swatch. */
function avatarColor(v: string): string {
  return /^#[0-9a-fA-F]{3,8}$/.test(v) ? v : "#f59e0b";
}

/** Pure, props-driven popover body — no hooks, so it renders under SSR for
 *  tests. The stateful [`AvatarMenu`] wraps it. */
export function AvatarPopover({
  lang,
  handle,
  displayName,
  avatar,
  theme,
  onLanguage,
  onName,
  onAvatar,
  onTheme,
  onLogout,
}: {
  lang: Lang;
  /** The signed-in identity's name from `/api/v1/me` (tenant handle / "owner"),
   *  or null until it loads. Shown so a tenant sees whose console this is. */
  handle: string | null;
  displayName: string;
  avatar: string;
  theme: "dark" | "light";
  onLanguage: (l: Lang) => void;
  onName: (n: string) => void;
  onAvatar: (a: string) => void;
  onTheme: () => void;
  onLogout: () => void;
}) {
  return (
    <div
      data-testid="avatar-popover"
      className="absolute right-0 top-10 z-50 w-64 rounded-lg border border-surface-700/60 bg-surface-900 p-3 shadow-xl"
    >
      <div className="flex items-baseline justify-between gap-2">
        <div className="text-xs font-semibold text-text-primary">
          {tr(lang, "个人设置", "Personal settings")}
        </div>
        {handle ? (
          <span
            data-testid="avatar-handle"
            title={tr(lang, `当前登录:${handle}`, `Signed in as ${handle}`)}
            className="truncate text-[11px] font-mono text-text-secondary"
          >
            @{handle}
          </span>
        ) : null}
      </div>

      <label className="mt-2 block text-[11px] text-text-dim">
        {tr(lang, "显示名", "Display name")}
      </label>
      <input
        data-testid="avatar-name-input"
        value={displayName}
        onChange={(e) => onName(e.target.value)}
        placeholder={tr(lang, "你的名字", "Your name")}
        className="mt-1 w-full rounded-md border border-surface-700/60 bg-surface-900 px-2 py-1 text-sm text-text-primary placeholder:text-text-dim focus:outline-none focus:ring-1 focus:ring-brand-500"
      />

      <div className="mt-3 flex items-center gap-1.5">
        <span className="mr-auto text-[11px] text-text-dim">{tr(lang, "头像", "Avatar")}</span>
        {AVATARS.map((a) => (
          <button
            key={a}
            type="button"
            data-testid={`avatar-swatch-${a}`}
            onClick={() => onAvatar(a)}
            aria-pressed={avatar === a}
            aria-label={a}
            style={{ backgroundColor: a }}
            className={`h-6 w-6 rounded-full transition-opacity ${
              avatar === a
                ? "ring-2 ring-text-bright"
                : "opacity-60 hover:opacity-100"
            }`}
          />
        ))}
      </div>

      <div className="mt-3 flex items-center gap-2">
        <span className="text-[11px] text-text-dim">{tr(lang, "界面语言", "Language")}</span>
        <div className="ml-auto inline-flex overflow-hidden rounded-md border border-surface-700/60 text-xs">
          <button
            type="button"
            data-testid="lang-zh"
            onClick={() => onLanguage("zh")}
            aria-pressed={lang === "zh"}
            className={`px-2 py-1 ${
              lang === "zh"
                ? "bg-brand-500/20 text-brand-400"
                : "text-text-secondary hover:text-text-primary"
            }`}
          >
            中文
          </button>
          <button
            type="button"
            data-testid="lang-en"
            onClick={() => onLanguage("en")}
            aria-pressed={lang === "en"}
            className={`px-2 py-1 ${
              lang === "en"
                ? "bg-brand-500/20 text-brand-400"
                : "text-text-secondary hover:text-text-primary"
            }`}
          >
            English
          </button>
        </div>
      </div>

      <div className="mt-3 flex items-center gap-2">
        <span className="text-[11px] text-text-dim">{tr(lang, "主题", "Theme")}</span>
        <button
          type="button"
          data-testid="theme-toggle"
          onClick={onTheme}
          aria-label={tr(lang, "切换明暗", "Toggle theme")}
          title={tr(lang, "切换明暗", "Toggle theme")}
          className="ml-auto grid h-7 w-7 place-items-center rounded-md border border-surface-700/60 text-text-secondary hover:bg-surface-800 hover:text-text-primary"
        >
          {theme === "dark" ? <Sun className="h-4 w-4" /> : <Moon className="h-4 w-4" />}
        </button>
      </div>

      <button
        type="button"
        data-testid="avatar-logout"
        onClick={onLogout}
        className="mt-3 flex w-full items-center gap-1.5 border-t border-surface-800 pt-2 text-left text-[11px] text-status-error hover:text-status-error/80"
      >
        <LogOut className="h-3.5 w-3.5" />
        {tr(lang, "登出（清你的 token）", "Log out (clears your token)")}
      </button>
    </div>
  );
}

/** Avatar button + personal-settings popover. Persists to useWebSettings. */
export default function AvatarMenu() {
  const { settings, update } = useWebSettings();
  const { me } = useMe();
  const [open, setOpen] = useState(false);
  const lang = settings.language;
  const handle = me?.handle ?? null;
  // Prefer the locally-chosen display name for the avatar initial; fall back to
  // the signed-in identity's handle so a tenant who never set a name still sees
  // a meaningful letter.
  const initial = ((settings.displayName || "").trim() || handle || "")
    .slice(0, 1)
    .toUpperCase();

  const logout = () => {
    clearToken();
    if (typeof window !== "undefined") {
      // Reload so the token gate re-evaluates and shows the entry page.
      window.location.reload();
    }
  };

  return (
    <div className="relative">
      <button
        type="button"
        data-testid="avatar-button"
        onClick={() => setOpen((o) => !o)}
        aria-label={
          handle
            ? tr(lang, `个人设置(${handle})`, `Personal settings (${handle})`)
            : tr(lang, "个人设置", "Personal settings")
        }
        title={handle ? tr(lang, `当前登录:${handle}`, `Signed in as ${handle}`) : undefined}
        style={{ backgroundColor: avatarColor(settings.avatar) }}
        className="grid h-8 w-8 place-items-center rounded-full text-[11px] font-semibold text-surface-950 ring-1 ring-surface-700/60 hover:ring-brand-500/60"
      >
        <span aria-hidden>{initial}</span>
      </button>
      {open ? (
        <>
          {/* click-away backdrop */}
          <button
            type="button"
            aria-label={tr(lang, "关闭", "Close")}
            onClick={() => setOpen(false)}
            className="fixed inset-0 z-40 cursor-default"
          />
          <AvatarPopover
            lang={lang}
            handle={handle}
            displayName={settings.displayName}
            avatar={settings.avatar}
            theme={settings.theme}
            onLanguage={(l) => update({ language: l })}
            onName={(n) => update({ displayName: n })}
            onAvatar={(a) => update({ avatar: a })}
            onTheme={() => update({ theme: settings.theme === "dark" ? "light" : "dark" })}
            onLogout={logout}
          />
        </>
      ) : null}
    </div>
  );
}
