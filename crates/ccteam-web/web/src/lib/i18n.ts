// v0.8.18 柱2/UI — minimal interface-language helper.
//
// Honest scope: this is NOT full UI i18n (every string translated). It drives
// the NAV labels + a handful of key labels by the user's chosen language
// (中文 default / English). Full i18n — a framework + every string extracted —
// is a separate later version; this is the seam it grows from.

export type Lang = "zh" | "en";

/** Pick the right string for the current language (`zh` is the default). */
export function tr(lang: Lang, zh: string, en: string): string {
  return lang === "en" ? en : zh;
}

/** The bottom global-nav labels, per language. Keyed by the `GlobalView`
 *  route key the ChatConsole shell uses. */
export const NAV_LABELS: Record<string, { zh: string; en: string }> = {
  marketplace: { zh: "插件市场", en: "Plugins" },
  status: { zh: "Status", en: "Status" },
  hosts: { zh: "主机", en: "Hosts" },
  settings: { zh: "Settings", en: "Settings" },
};

/** Resolve a nav label for the active language. Falls back to the key. */
export function navLabel(key: string, lang: Lang): string {
  const entry = NAV_LABELS[key];
  if (!entry) return key;
  return lang === "en" ? entry.en : entry.zh;
}
