// Pure helpers behind the per-session context panel (`ContextPanel.tsx`):
// sorting, bar segments, category colors, cache ratio, token/percent/time
// formatting, and the per-browser open/closed memory. No React / DOM so the
// vitest suite exercises every rule directly (mirrors `statusLine.ts`).

import type {
  ContextAttachmentUsage,
  ContextBreakdown,
  ContextCategory,
  ContextLastRequest,
  ContextMessages,
} from "./contextApi";
import type { Lang } from "./i18n";

// ---- tokens / percent ------------------------------------------------------

/** Compact token count: `253_770 → "253.8k"`, `33_000 → "33k"`,
 *  `1_000_000 → "1M"`, `941 → "941"`. One decimal, trailing `.0` dropped.
 *  Non-numbers / negatives render an em-dash — an unknown count is a
 *  different claim from zero. */
export function formatTokensCompact(n: number | null | undefined): string {
  if (typeof n !== "number" || !Number.isFinite(n) || n < 0) return "—";
  const trim = (s: string) => s.replace(/\.0$/, "");
  if (n >= 999_950) return `${trim((n / 1_000_000).toFixed(1))}M`;
  if (n >= 999.5) return `${trim((n / 1000).toFixed(1))}k`;
  return `${Math.round(n)}`;
}

/** Exact count with thousands separators for tooltips (`253,770`). */
export function formatTokensExact(n: number | null | undefined): string {
  if (typeof n !== "number" || !Number.isFinite(n)) return "—";
  return Math.round(n).toLocaleString("en-US");
}

/** `tokens / whole × 100`, or null when the whole is unknown / non-positive. */
export function pctOf(tokens: number | null | undefined, whole: number | null | undefined): number | null {
  if (typeof tokens !== "number" || !Number.isFinite(tokens)) return null;
  if (typeof whole !== "number" || !Number.isFinite(whole) || whole <= 0) return null;
  return (tokens / whole) * 100;
}

/** One decimal, trailing `.0` dropped: `25.4%`, `71%`, `99.6%`. Values under
 *  a tenth of a percent (but above zero) read `<0.1%` rather than a misleading
 *  `0%`; null → em-dash. */
export function formatPct(pct: number | null): string {
  if (pct === null || !Number.isFinite(pct)) return "—";
  if (pct > 0 && pct < 0.1) return "<0.1%";
  return `${pct.toFixed(1).replace(/\.0$/, "")}%`;
}

// ---- categories --------------------------------------------------------------

/** Categorical color slot per category id (1..8 → `--ctx-<n>`; 0 → the
 *  neutral "other" grey). FIXED per entity, never assigned by rank, so a
 *  category keeps its color across sessions and refreshes. The eight hues are
 *  the validated dataviz default palette (adjacent-pair CVD ΔE ≥ 8 in both
 *  modes); any id not listed folds into neutral rather than minting a 9th hue. */
const CATEGORY_SLOT: Record<string, number> = {
  messages: 1,
  memory_files: 2,
  system_tools: 3,
  skills: 4,
  system_prompt: 5,
  mcp_tools: 6,
  custom_agents: 7,
  mcp_instructions: 8,
};

export function categorySlot(id: string): number {
  return CATEGORY_SLOT[id] ?? 0;
}

/** Localized display name for a known category id; the server's own `name`
 *  otherwise (English copy, so `en` always prefers it). */
const CATEGORY_LABEL_ZH: Record<string, string> = {
  system_prompt: "系统提示词",
  system_tools: "系统工具",
  mcp_tools: "MCP 工具",
  mcp_instructions: "MCP 服务说明",
  mcp_tools_deferred: "MCP 工具(延迟)",
  system_tools_deferred: "系统工具(延迟)",
  custom_agents: "自定义 agent",
  memory_files: "记忆文件",
  skills: "Skills",
  messages: "消息",
  autocompact_buffer: "自动压缩预留",
  free_space: "剩余空间",
};

export function categoryLabel(category: Pick<ContextCategory, "id" | "name">, lang: Lang): string {
  if (lang === "zh") return CATEGORY_LABEL_ZH[category.id] ?? category.name;
  return category.name;
}

function tokensOf(c: { tokens?: number | null }): number {
  return typeof c.tokens === "number" && Number.isFinite(c.tokens) ? c.tokens : 0;
}

const byTokensDesc = <T extends { tokens?: number | null }>(a: T, b: T) => tokensOf(b) - tokensOf(a);

export interface CategorySplit {
  /** Occupies the window — sorted tokens desc. */
  used: ContextCategory[];
  /** Tool schemas NOT loaded — never counted; sorted tokens desc. */
  deferred: ContextCategory[];
  /** Auto-compact reserve(s), then free space — in that order. */
  reserve: ContextCategory[];
  /** Any kind the UI does not know; shown as plain rows, outside the bar. */
  other: ContextCategory[];
}

export function splitCategories(breakdown: ContextBreakdown): CategorySplit {
  const all = breakdown.categories ?? [];
  const used = all.filter((c) => c.kind === "used").sort(byTokensDesc);
  const deferred = all.filter((c) => c.kind === "deferred").sort(byTokensDesc);
  const reserve = [
    ...all.filter((c) => c.kind === "buffer"),
    ...all.filter((c) => c.kind === "free"),
  ];
  const other = all.filter(
    (c) => c.kind !== "used" && c.kind !== "deferred" && c.kind !== "buffer" && c.kind !== "free",
  );
  return { used, deferred, reserve, other };
}

/** The category table order: used (desc) → buffer → free → unknown kinds.
 *  Deferred categories are NOT here — they get their own muted list. */
export function tableRows(breakdown: ContextBreakdown): ContextCategory[] {
  const split = splitCategories(breakdown);
  return [...split.used, ...split.reserve, ...split.other];
}

export interface BarSegment {
  id: string;
  name: string;
  tokens: number;
  kind: "used" | "buffer" | "free";
  /** Share of the window, 0..100. */
  pct: number;
  /** Color slot (used only); null for buffer / free. */
  slot: number | null;
}

/** One stacked bar of the window: used categories (desc, each its own color),
 *  then buffer, then free. Deferred categories are excluded — they do not
 *  occupy context. Zero-token segments are dropped (nothing to paint). The
 *  denominator is `window_tokens`; when the server omits it, the segments'
 *  own sum stands in so the bar still reads as part-to-whole. */
export function barSegments(breakdown: ContextBreakdown): BarSegment[] {
  const { used, reserve } = splitCategories(breakdown);
  const ordered: Array<ContextCategory & { kind: BarSegment["kind"] }> = [
    ...used.map((c) => ({ ...c, kind: "used" as const })),
    ...reserve.map((c) => ({ ...c, kind: c.kind === "buffer" ? ("buffer" as const) : ("free" as const) })),
  ].filter((c) => tokensOf(c) > 0);
  const sum = ordered.reduce((acc, c) => acc + tokensOf(c), 0);
  const whole =
    typeof breakdown.window_tokens === "number" && breakdown.window_tokens > 0
      ? breakdown.window_tokens
      : sum;
  if (whole <= 0) return [];
  return ordered.map((c) => ({
    id: c.id,
    name: c.name,
    tokens: tokensOf(c),
    kind: c.kind,
    pct: (tokensOf(c) / whole) * 100,
    slot: c.kind === "used" ? categorySlot(c.id) : null,
  }));
}

/** Window occupancy in percent — `used_tokens / window_tokens`. */
export function usedPct(breakdown: ContextBreakdown): number | null {
  return pctOf(breakdown.used_tokens, breakdown.window_tokens);
}

/** Where the auto-compact threshold sits on the bar (0..100), or null. */
export function thresholdPct(breakdown: ContextBreakdown): number | null {
  const pct = pctOf(breakdown.auto_compact_threshold, breakdown.window_tokens);
  return pct === null ? null : Math.max(0, Math.min(100, pct));
}

export function hasItems(category: ContextCategory): boolean {
  return Array.isArray(category.items) && category.items.length > 0;
}

/** Items of a category, largest first. */
export function sortedItems(category: ContextCategory) {
  return [...(category.items ?? [])].sort(byTokensDesc);
}

// ---- messages ----------------------------------------------------------------

export type MessageSplitKey =
  | "tool_call"
  | "tool_result"
  | "attachment"
  | "assistant"
  | "user"
  | "other";

export interface MessageSplitRow {
  key: MessageSplitKey;
  tokens: number;
}

/** The five (six) buckets of the conversation, largest first. Buckets the
 *  server left null are omitted; a zero `other` is omitted too (noise). */
export function messageSplit(messages: ContextMessages): MessageSplitRow[] {
  const pairs: Array<[MessageSplitKey, number | null | undefined]> = [
    ["tool_call", messages.tool_call_tokens],
    ["tool_result", messages.tool_result_tokens],
    ["attachment", messages.attachment_tokens],
    ["assistant", messages.assistant_tokens],
    ["user", messages.user_tokens],
    ["other", messages.other_tokens],
  ];
  return pairs
    .filter((entry): entry is [MessageSplitKey, number] => typeof entry[1] === "number" && Number.isFinite(entry[1]))
    .filter(([key, tokens]) => !(key === "other" && tokens <= 0))
    .map(([key, tokens]) => ({ key, tokens }))
    .sort(byTokensDesc);
}

/** Sum of the known buckets — the denominator for "% of messages". */
export function messagesTotal(messages: ContextMessages): number {
  return messageSplit(messages).reduce((acc, row) => acc + row.tokens, 0);
}

export interface ToolRow {
  name: string;
  call: number;
  result: number;
  total: number;
}

/** Per-tool cost (call + result), largest total first. */
export function toolRows(messages: ContextMessages): ToolRow[] {
  return (messages.tools ?? [])
    .map((tool) => {
      const call = typeof tool.call_tokens === "number" ? tool.call_tokens : 0;
      const result = typeof tool.result_tokens === "number" ? tool.result_tokens : 0;
      return { name: tool.name, call, result, total: call + result };
    })
    .sort((a, b) => b.total - a.total);
}

/** Attachment types, largest first. */
export function attachmentRows(messages: ContextMessages): ContextAttachmentUsage[] {
  return [...(messages.attachments ?? [])].sort(byTokensDesc);
}

// ---- last request ------------------------------------------------------------

/** `cache_read / (cache_read + cache_creation + input)` in 0..1, or null when
 *  nothing was read, written or sent (no request to speak of). */
export function cacheHitRatio(last: ContextLastRequest): number | null {
  const read = typeof last.cache_read_tokens === "number" ? last.cache_read_tokens : 0;
  const write = typeof last.cache_creation_tokens === "number" ? last.cache_creation_tokens : 0;
  const input = typeof last.input_tokens === "number" ? last.input_tokens : 0;
  const denominator = read + write + input;
  if (denominator <= 0) return null;
  return read / denominator;
}

// ---- text ----------------------------------------------------------------------

/** Keep the head and the tail of a long path (`/repo/.cl…/cto.md`) so neither
 *  the project nor the file is lost; the full string rides the tooltip. */
export function truncateMiddle(text: string, max = 44): string {
  if (text.length <= max) return text;
  const keep = Math.max(1, max - 1);
  const head = Math.ceil(keep * 0.55);
  const tail = keep - head;
  return `${text.slice(0, head)}…${text.slice(text.length - tail)}`;
}

/** `刚刚` / `3 分钟前` / `2 小时前` / `3 天前` (en: `just now` / `3 min ago` /
 *  `2 h ago` / `3 d ago`). Null for an unparseable timestamp. */
export function relativeTime(iso: string | null | undefined, nowMs: number, lang: Lang): string | null {
  if (!iso) return null;
  const at = new Date(iso).getTime();
  if (Number.isNaN(at)) return null;
  const diff = Math.max(0, nowMs - at);
  const minutes = Math.floor(diff / 60_000);
  if (minutes < 1) return lang === "en" ? "just now" : "刚刚";
  if (minutes < 60) return lang === "en" ? `${minutes} min ago` : `${minutes} 分钟前`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return lang === "en" ? `${hours} h ago` : `${hours} 小时前`;
  const days = Math.floor(hours / 24);
  return lang === "en" ? `${days} d ago` : `${days} 天前`;
}

// ---- open/closed memory ------------------------------------------------------

/** localStorage key for the panel's docked state (per browser, not per
 *  session: the panel is a workspace layout preference that should survive a
 *  session switch — SessionView remounts per sid and reads this back). */
export const CONTEXT_PANEL_OPEN_KEY = "ccteam.contextPanel.open.v1";

type StorageLike = Pick<Storage, "getItem" | "setItem">;

export function readContextPanelOpen(storage: StorageLike | null | undefined): boolean {
  if (!storage) return false;
  try {
    return storage.getItem(CONTEXT_PANEL_OPEN_KEY) === "1";
  } catch {
    return false;
  }
}

export function writeContextPanelOpen(storage: StorageLike | null | undefined, open: boolean): void {
  if (!storage) return;
  try {
    storage.setItem(CONTEXT_PANEL_OPEN_KEY, open ? "1" : "0");
  } catch {
    // Private mode / quota — the toggle still works for this page.
  }
}

/** ≤820px is the shell's phone breakpoint (the sidebar becomes a drawer). On
 *  such a viewport the panel is an overlay that covers the chat, so a
 *  remembered "open" is NOT restored automatically — the user re-opens it.
 *  SSR / jsdom (no matchMedia) count as wide. */
export const NARROW_VIEWPORT_QUERY = "(max-width: 820px)";

export function isNarrowViewport(win: { matchMedia?: (q: string) => { matches: boolean } } | undefined): boolean {
  if (!win || typeof win.matchMedia !== "function") return false;
  try {
    return win.matchMedia(NARROW_VIEWPORT_QUERY).matches;
  } catch {
    return false;
  }
}
