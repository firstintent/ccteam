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

/** Shell / nav labels, keyed by route or shell surface. */
export const NAV_LABELS: Record<string, { zh: string; en: string }> = {
  marketplace: { zh: "插件市场", en: "Plugins" },
  status: { zh: "Status", en: "Status" },
  hosts: { zh: "主机", en: "Hosts" },
  settings: { zh: "设置", en: "Settings" },
  workflow: { zh: "工作流", en: "Workflow" },
  home: { zh: "开工吧!", en: "Let's go!" },
  newSession: { zh: "新建会话", en: "New session" },
  search: { zh: "搜索会话 / 项目", en: "Search sessions / projects" },
  collapse: { zh: "折叠侧栏", en: "Collapse sidebar" },
  expand: { zh: "展开侧栏", en: "Expand sidebar" },
  general: { zh: "通用", en: "General" },
  account: { zh: "账号", en: "Account" },
  im: { zh: "IM 接入", en: "IM" },
  theme: { zh: "主题", en: "Theme" },
  language: { zh: "界面语言", en: "Language" },
  light: { zh: "浅色", en: "Light" },
  dark: { zh: "深色", en: "Dark" },
};

/** Resolve a nav label for the active language. Falls back to the key. */
export function navLabel(key: string, lang: Lang): string {
  const entry = NAV_LABELS[key];
  if (!entry) return key;
  return lang === "en" ? entry.en : entry.zh;
}
