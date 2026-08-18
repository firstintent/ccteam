// VENDOR-QUOTA-1 — pure presentation helpers for the vendor quota mini bars
// on the Ops & Hosts rows. One window renders as one compact line:
//   `5h ▓▓░░░ 42% · resets in 3h12m`   `Week ▓░░░░ 15% · resets Tue`
// Everything takes `now` explicitly so the vitest suite is deterministic;
// the component passes `new Date()`. Local timezone + app language come from
// `Date` / explicit `lang` — never from the browser locale alone.

import type { Lang } from "./i18n";
import type { QuotaWindow, QuotaWindowKind, VendorQuota } from "./vendorQuotaApi";

/** The 5-cell bar: round(percent/20) filled cells, clamped to [0, 5]. */
export function quotaBar(usedPercent: number): string {
  const filled = Math.max(0, Math.min(5, Math.round(usedPercent / 20)));
  return "▓".repeat(filled) + "░".repeat(5 - filled);
}

function windowLabel(kind: QuotaWindowKind, lang: Lang): string {
  if (kind === "five_hour") return "5h";
  if (kind === "weekly") return lang === "en" ? "Week" : "周";
  return lang === "en" ? "Month" : "月";
}

/** Compact duration: `42m` / `3h12m` / `2d05h`; negative clamps to `0m`. */
export function compactDuration(ms: number): string {
  const totalMin = Math.max(0, Math.round(ms / 60_000));
  if (totalMin < 60) return `${totalMin}m`;
  const days = Math.floor(totalMin / (60 * 24));
  const hours = Math.floor((totalMin % (60 * 24)) / 60);
  const minutes = totalMin % 60;
  const mm = String(minutes).padStart(2, "0");
  if (days > 0) return `${days}d${String(hours).padStart(2, "0")}h`;
  return minutes > 0 ? `${hours}h${mm}m` : `${hours}h`;
}

/** The reset hint, or null when the vendor reports no reset time.
 *  < 24h → relative (`resets in 3h12m`); < 7d → weekday (`resets Tue`);
 *  beyond → short date. Past timestamps read "resets soon". */
export function resetHint(
  resetsAt: string | null | undefined,
  now: Date,
  lang: Lang,
): string | null {
  if (!resetsAt) return null;
  const at = new Date(resetsAt);
  if (Number.isNaN(at.getTime())) return null;
  const diff = at.getTime() - now.getTime();
  const locale = lang === "en" ? "en-US" : "zh-CN";
  if (diff <= 0) return lang === "en" ? "resets soon" : "即将重置";
  if (diff < 24 * 3_600_000) {
    return lang === "en" ? `resets in ${compactDuration(diff)}` : `${compactDuration(diff)}后重置`;
  }
  if (diff < 7 * 24 * 3_600_000) {
    const weekday = at.toLocaleDateString(locale, { weekday: "short" });
    return lang === "en" ? `resets ${weekday}` : `${weekday}重置`;
  }
  const date = at.toLocaleDateString(locale, { month: "short", day: "numeric" });
  return lang === "en" ? `resets ${date}` : `${date}重置`;
}

/** One window's render line: `5h ▓▓░░░ 42% · resets in 3h12m`. */
export function quotaWindowLine(w: QuotaWindow, now: Date, lang: Lang): string {
  const pct = `${Math.round(w.used_percent)}%`;
  const head = `${windowLabel(w.kind, lang)} ${quotaBar(w.used_percent)} ${pct}`;
  const hint = resetHint(w.resets_at, now, lang);
  return hint ? `${head} · ${hint}` : head;
}

/** The lines a vendor row renders for its quota: up to two window bars when
 *  `available`, otherwise NOTHING (`not_subscription` / `unavailable` /
 *  missing row all collapse to the empty list — the zone hides). */
export function quotaLines(quota: VendorQuota | null | undefined, now: Date, lang: Lang): string[] {
  if (!quota || quota.state !== "available") return [];
  return (quota.windows ?? []).slice(0, 2).map((w) => quotaWindowLine(w, now, lang));
}

/** The plan badge text, only for an available row that carries one. */
export function quotaPlan(quota: VendorQuota | null | undefined): string | null {
  if (!quota || quota.state !== "available") return null;
  const plan = quota.plan?.trim();
  return plan ? plan : null;
}
