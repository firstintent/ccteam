// v0.8.24 Track A — pure display helpers for the sidebar rail + Home recents,
// extracted from the old ChatConsole so they stay dependency-free and
// unit-testable (this repo has no @testing-library DOM harness; pure logic is
// tested directly).

/** Display label for a rail / history / recent session row (v0.8.22 P1
 *  session-title system): the user-facing title when set, else the role, else
 *  the roleless placeholder. */
export function railSessionLabel(s: { title?: string | null; role: string }): string {
  return s.title || s.role || "(无 role)";
}

/** Chinese relative-time phrase for an RFC3339 timestamp — mirrors
 *  `ccteam-im::gateway::relative_time_zh` so IM `/sessions` and the web rail
 *  read the same way. Unparseable/empty input renders `"—"`. */
export function relativeTimeZh(iso: string | null | undefined): string {
  if (!iso) return "—";
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return "—";
  const secs = Math.max(0, Math.floor((Date.now() - then) / 1000));
  if (secs < 60) return "刚刚";
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins}分钟前`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}小时前`;
  const days = Math.floor(hours / 24);
  if (days === 1) return "昨天";
  if (days < 7) return `${days}天前`;
  const weeks = Math.floor(days / 7);
  if (weeks < 5) return `${weeks}周前`;
  return new Date(then).toISOString().slice(0, 10);
}

/** Compact English relative time (prototype card style: `12m` / `2h` / `3d`). */
export function relativeTimeEn(iso: string | null | undefined): string {
  if (!iso) return "—";
  const then = Date.parse(iso);
  if (Number.isNaN(then)) return "—";
  const secs = Math.max(0, Math.floor((Date.now() - then) / 1000));
  if (secs < 60) return "now";
  const mins = Math.floor(secs / 60);
  if (mins < 60) return `${mins}m`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h`;
  const days = Math.floor(hours / 24);
  if (days < 7) return `${days}d`;
  const weeks = Math.floor(days / 7);
  if (weeks < 5) return `${weeks}w`;
  return new Date(then).toISOString().slice(0, 10);
}

/** Language-aware relative time for the recents grid. */
export function relativeTime(lang: "zh" | "en", iso: string | null | undefined): string {
  return lang === "en" ? relativeTimeEn(iso) : relativeTimeZh(iso);
}
