// v0.8.18 柱2/UI — the avatar button + personal-settings popover (top bar).
//
// Personal settings (display name / avatar / interface language / logout) live
// behind the avatar, distinct from the global/admin Settings page. Stored
// per-browser via useWebSettings in 档0 (no per-user web identity yet); 档1
// ties them to the server identity.
//
// `AvatarPopover` is split out as a PURE, props-driven component so it is
// SSR-testable (the node-env vitest suite has no DOM / click events).

import { useState } from "react";
import { useWebSettings } from "../hooks/useWebSettings";
import { clearToken } from "../lib/token";
import { tr, type Lang } from "../lib/i18n";

/** The fixed avatar palette. */
const AVATARS = ["🟧", "🟦", "🟩", "🟪", "⬛"];

/** Pure, props-driven popover body — no hooks, so it renders under SSR for
 *  tests. The stateful [`AvatarMenu`] wraps it. */
export function AvatarPopover({
  lang,
  displayName,
  avatar,
  onLanguage,
  onName,
  onAvatar,
  onLogout,
}: {
  lang: Lang;
  displayName: string;
  avatar: string;
  onLanguage: (l: Lang) => void;
  onName: (n: string) => void;
  onAvatar: (a: string) => void;
  onLogout: () => void;
}) {
  return (
    <div
      data-testid="avatar-popover"
      className="absolute right-0 top-10 z-50 w-64 rounded-lg border border-surface-700/60 bg-surface-900 p-3 shadow-xl"
    >
      <div className="text-xs font-semibold text-text-primary">
        {tr(lang, "个人设置", "Personal settings")}
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
            className={`grid h-6 w-6 place-items-center rounded ${
              avatar === a ? "ring-2 ring-brand-500" : "opacity-70 hover:opacity-100"
            }`}
          >
            <span aria-hidden>{a}</span>
          </button>
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

      <button
        type="button"
        data-testid="avatar-logout"
        onClick={onLogout}
        className="mt-3 w-full border-t border-surface-800 pt-2 text-left text-[11px] text-status-error hover:text-status-error/80"
      >
        {tr(lang, "⎋ 登出（清你的 token）", "⎋ Log out (clears your token)")}
      </button>
    </div>
  );
}

/** Avatar button + personal-settings popover. Persists to useWebSettings. */
export default function AvatarMenu() {
  const { settings, update } = useWebSettings();
  const [open, setOpen] = useState(false);
  const lang = settings.language;

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
        aria-label={tr(lang, "个人设置", "Personal settings")}
        className="grid h-8 w-8 place-items-center rounded-full border border-surface-700/60 bg-surface-800 text-base hover:border-brand-500/50"
      >
        <span aria-hidden>{settings.avatar || "🟧"}</span>
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
            displayName={settings.displayName}
            avatar={settings.avatar}
            onLanguage={(l) => update({ language: l })}
            onName={(n) => update({ displayName: n })}
            onAvatar={(a) => update({ avatar: a })}
            onLogout={logout}
          />
        </>
      ) : null}
    </div>
  );
}
