// Pure rules behind the context panel: compact numbers, the bar's segments
// (deferred excluded), table order, message/tool/attachment sorting, the
// cache-hit ratio, middle truncation, relative age, and the per-browser
// open/closed memory.

import { describe, expect, it } from "vitest";

import type { ContextBreakdown } from "./contextApi";
import {
  attachmentRows,
  barSegments,
  cacheHitRatio,
  categoryLabel,
  categorySlot,
  CONTEXT_PANEL_OPEN_KEY,
  formatPct,
  formatTokensCompact,
  formatTokensExact,
  isNarrowViewport,
  messageSplit,
  messagesTotal,
  pctOf,
  readContextPanelOpen,
  relativeTime,
  splitCategories,
  tableRows,
  thresholdPct,
  toolRows,
  truncateMiddle,
  usedPct,
  writeContextPanelOpen,
} from "./contextBreakdown";

export const FIXTURE: ContextBreakdown = {
  vendor: "claude",
  model: "claude-sonnet-5-5",
  captured_at: "2026-10-21T08:00:00Z",
  used_tokens: 253_770,
  window_tokens: 1_000_000,
  auto_compact_threshold: 967_000,
  categories: [
    { id: "system_prompt", name: "System prompt", tokens: 2247, kind: "used", items: [] },
    { id: "system_tools", name: "System tools", tokens: 6714, kind: "used", items: [] },
    {
      id: "mcp_tools",
      name: "MCP tools",
      tokens: 941,
      kind: "used",
      items: [{ name: "mcp__ccteam__agent", source: "ccteam", detail: null, tokens: 941, loaded: true }],
    },
    {
      id: "mcp_tools_deferred",
      name: "MCP tools (deferred)",
      tokens: 1290,
      kind: "deferred",
      items: [{ name: "mcp__ccteam__agent_read", source: "ccteam", detail: null, tokens: 649, loaded: false }],
    },
    { id: "system_tools_deferred", name: "System tools (deferred)", tokens: 12_011, kind: "deferred", items: [] },
    {
      id: "memory_files",
      name: "Memory files",
      tokens: 15_169,
      kind: "used",
      items: [
        { name: "/home/u/.claude/CLAUDE.md", source: "User", detail: null, tokens: 103, loaded: null },
        { name: "/repo/CLAUDE.md", source: "Project", detail: null, tokens: 11_489, loaded: null },
      ],
    },
    { id: "messages", name: "Messages", tokens: 237_146, kind: "used", items: [] },
    { id: "autocompact_buffer", name: "Autocompact buffer", tokens: 33_000, kind: "buffer", items: [] },
    { id: "free_space", name: "Free space", tokens: 713_230, kind: "free", items: [] },
  ],
  messages: {
    tool_call_tokens: 36_149,
    tool_result_tokens: 62_805,
    attachment_tokens: 69_528,
    assistant_tokens: 67_354,
    user_tokens: 1732,
    other_tokens: 0,
    tools: [
      { name: "mcp__ccteam__agent", call_tokens: 1220, result_tokens: 84 },
      { name: "Bash", call_tokens: 34_420, result_tokens: 61_749 },
    ],
    attachments: [
      { name: "async_hook_response", tokens: 17_151 },
      { name: "prompt_snapshot", tokens: 26_483 },
    ],
  },
  last_request: { input_tokens: 2, output_tokens: 1259, cache_creation_tokens: 1022, cache_read_tokens: 252_179 },
};

describe("token / percent formatting", () => {
  it("formats compactly with one decimal and no trailing .0", () => {
    expect(formatTokensCompact(253_770)).toBe("253.8k");
    expect(formatTokensCompact(33_000)).toBe("33k");
    expect(formatTokensCompact(2247)).toBe("2.2k");
    expect(formatTokensCompact(941)).toBe("941");
    expect(formatTokensCompact(1_000_000)).toBe("1M");
    expect(formatTokensCompact(967_000)).toBe("967k");
    expect(formatTokensCompact(999_960)).toBe("1M");
    expect(formatTokensCompact(0)).toBe("0");
  });

  it("renders unknown counts as a dash, never 0", () => {
    expect(formatTokensCompact(null)).toBe("—");
    expect(formatTokensCompact(undefined)).toBe("—");
    expect(formatTokensCompact(Number.NaN)).toBe("—");
    expect(formatTokensExact(null)).toBe("—");
  });

  it("exact numbers carry thousands separators for tooltips", () => {
    expect(formatTokensExact(253_770)).toBe("253,770");
  });

  it("pctOf is null without a positive whole", () => {
    expect(pctOf(250, 1000)).toBe(25);
    expect(pctOf(250, 0)).toBeNull();
    expect(pctOf(250, null)).toBeNull();
    expect(pctOf(null, 1000)).toBeNull();
  });

  it("formatPct: one decimal, tiny values read <0.1%, null is a dash", () => {
    expect(formatPct(25.377)).toBe("25.4%");
    expect(formatPct(71)).toBe("71%");
    expect(formatPct(99.6)).toBe("99.6%");
    expect(formatPct(0.04)).toBe("<0.1%");
    expect(formatPct(0)).toBe("0%");
    expect(formatPct(null)).toBe("—");
  });
});

describe("categories", () => {
  it("splits used / deferred / reserve, each sorted by tokens desc", () => {
    const split = splitCategories(FIXTURE);
    expect(split.used.map((c) => c.id)).toEqual([
      "messages",
      "memory_files",
      "system_tools",
      "system_prompt",
      "mcp_tools",
    ]);
    expect(split.deferred.map((c) => c.id)).toEqual(["system_tools_deferred", "mcp_tools_deferred"]);
    expect(split.reserve.map((c) => c.id)).toEqual(["autocompact_buffer", "free_space"]);
    expect(split.other).toEqual([]);
  });

  it("table rows = used desc, then buffer, then free — deferred never listed there", () => {
    expect(tableRows(FIXTURE).map((c) => c.id)).toEqual([
      "messages",
      "memory_files",
      "system_tools",
      "system_prompt",
      "mcp_tools",
      "autocompact_buffer",
      "free_space",
    ]);
  });

  it("unknown kinds are kept as plain rows after the reserve", () => {
    const rows = tableRows({
      categories: [
        { id: "x", name: "X", tokens: 5, kind: "mystery" },
        { id: "messages", name: "Messages", tokens: 10, kind: "used" },
      ],
    });
    expect(rows.map((c) => c.id)).toEqual(["messages", "x"]);
  });

  it("bar segments: used desc → buffer → free, deferred excluded, % of window", () => {
    const segments = barSegments(FIXTURE);
    expect(segments.map((s) => s.id)).toEqual([
      "messages",
      "memory_files",
      "system_tools",
      "system_prompt",
      "mcp_tools",
      "autocompact_buffer",
      "free_space",
    ]);
    expect(segments.find((s) => s.id === "mcp_tools_deferred")).toBeUndefined();
    expect(segments[0]).toMatchObject({ kind: "used", slot: 1 });
    expect(segments[0]!.pct).toBeCloseTo(23.7146, 6);
    const buffer = segments.find((s) => s.id === "autocompact_buffer")!;
    expect(buffer).toMatchObject({ kind: "buffer", slot: null });
    expect(buffer.pct).toBeCloseTo(3.3, 6);
    expect(segments[segments.length - 1]).toMatchObject({ kind: "free", slot: null });
    expect(segments[segments.length - 1]!.pct).toBeCloseTo(71.323, 6);
  });

  it("bar segments fall back to their own sum when the window is unknown", () => {
    const segments = barSegments({
      categories: [
        { id: "messages", name: "Messages", tokens: 75, kind: "used" },
        { id: "free_space", name: "Free", tokens: 25, kind: "free" },
        { id: "zero", name: "Zero", tokens: 0, kind: "used" },
      ],
    });
    expect(segments.map((s) => [s.id, s.pct])).toEqual([
      ["messages", 75],
      ["free_space", 25],
    ]);
  });

  it("no categories → no bar", () => {
    expect(barSegments({})).toEqual([]);
    expect(barSegments({ categories: null })).toEqual([]);
  });

  it("colors follow the entity, unknown ids fold into neutral", () => {
    expect(categorySlot("messages")).toBe(1);
    expect(categorySlot("memory_files")).toBe(2);
    expect(categorySlot("brand_new_category")).toBe(0);
    const slots = ["messages", "memory_files", "system_tools", "skills", "system_prompt", "mcp_tools", "custom_agents", "mcp_instructions"].map(categorySlot);
    expect(new Set(slots).size).toBe(8);
  });

  it("labels: zh maps known ids, en keeps the server name, unknown ids fall back", () => {
    expect(categoryLabel({ id: "memory_files", name: "Memory files" }, "zh")).toBe("记忆文件");
    expect(categoryLabel({ id: "memory_files", name: "Memory files" }, "en")).toBe("Memory files");
    expect(categoryLabel({ id: "novel", name: "Novel thing" }, "zh")).toBe("Novel thing");
  });

  it("usedPct / thresholdPct read off the window", () => {
    expect(usedPct(FIXTURE)).toBeCloseTo(25.377, 3);
    expect(thresholdPct(FIXTURE)).toBeCloseTo(96.7, 3);
    expect(usedPct({ used_tokens: 5 })).toBeNull();
    expect(thresholdPct({ auto_compact_threshold: 5, window_tokens: 0 })).toBeNull();
    expect(thresholdPct({ auto_compact_threshold: 2000, window_tokens: 1000 })).toBe(100);
  });
});

describe("messages", () => {
  it("splits the conversation into buckets, largest first, dropping a zero 'other'", () => {
    expect(messageSplit(FIXTURE.messages!).map((r) => r.key)).toEqual([
      "attachment",
      "assistant",
      "tool_result",
      "tool_call",
      "user",
    ]);
    expect(messagesTotal(FIXTURE.messages!)).toBe(36_149 + 62_805 + 69_528 + 67_354 + 1732);
  });

  it("keeps a non-zero 'other' and omits null buckets", () => {
    expect(messageSplit({ other_tokens: 9, user_tokens: null, assistant_tokens: 3 })).toEqual([
      { key: "other", tokens: 9 },
      { key: "assistant", tokens: 3 },
    ]);
    expect(messageSplit({})).toEqual([]);
  });

  it("tool rows total call + result and sort by total desc", () => {
    const rows = toolRows(FIXTURE.messages!);
    expect(rows.map((r) => r.name)).toEqual(["Bash", "mcp__ccteam__agent"]);
    expect(rows[0]).toEqual({ name: "Bash", call: 34_420, result: 61_749, total: 96_169 });
    expect(toolRows({ tools: [{ name: "Read" }] })).toEqual([{ name: "Read", call: 0, result: 0, total: 0 }]);
    expect(toolRows({ tools: null })).toEqual([]);
  });

  it("attachments sort by tokens desc", () => {
    expect(attachmentRows(FIXTURE.messages!).map((a) => a.name)).toEqual([
      "prompt_snapshot",
      "async_hook_response",
    ]);
    expect(attachmentRows({})).toEqual([]);
  });
});

describe("last request", () => {
  it("cache hit = read / (read + write + uncached input)", () => {
    expect(cacheHitRatio(FIXTURE.last_request!)).toBeCloseTo(252_179 / (252_179 + 1022 + 2), 6);
  });
  it("is null when nothing was read, written or sent", () => {
    expect(cacheHitRatio({})).toBeNull();
    expect(cacheHitRatio({ output_tokens: 10 })).toBeNull();
    expect(cacheHitRatio({ cache_read_tokens: 0, input_tokens: 0 })).toBeNull();
  });
  it("treats missing halves as zero", () => {
    expect(cacheHitRatio({ cache_read_tokens: 50, input_tokens: 50 })).toBe(0.5);
    expect(cacheHitRatio({ input_tokens: 50 })).toBe(0);
  });
});

describe("text", () => {
  it("truncateMiddle keeps head and tail, never longer than max", () => {
    const path = "/home/ubuntu/rob/ccteam/.claude/agents/cto-the-very-long-name.md";
    const short = truncateMiddle(path, 30);
    expect(short.length).toBeLessThanOrEqual(30);
    expect(short.startsWith("/home/ubuntu")).toBe(true);
    expect(short.endsWith("name.md")).toBe(true);
    expect(short).toContain("…");
    expect(truncateMiddle("short", 30)).toBe("short");
  });

  it("relativeTime in both languages; unparseable → null", () => {
    const now = Date.parse("2026-10-21T10:00:00Z");
    expect(relativeTime("2026-10-21T09:59:40Z", now, "en")).toBe("just now");
    expect(relativeTime("2026-10-21T09:57:00Z", now, "en")).toBe("3 min ago");
    expect(relativeTime("2026-10-21T09:57:00Z", now, "zh")).toBe("3 分钟前");
    expect(relativeTime("2026-10-21T07:30:00Z", now, "en")).toBe("2 h ago");
    expect(relativeTime("2026-10-21T07:30:00Z", now, "zh")).toBe("2 小时前");
    expect(relativeTime("2026-10-18T07:30:00Z", now, "en")).toBe("3 d ago");
    expect(relativeTime("2026-10-18T07:30:00Z", now, "zh")).toBe("3 天前");
    expect(relativeTime("nope", now, "en")).toBeNull();
    expect(relativeTime(null, now, "en")).toBeNull();
  });
});

describe("open/closed memory", () => {
  function memoryStorage() {
    const map = new Map<string, string>();
    return {
      getItem: (k: string) => map.get(k) ?? null,
      setItem: (k: string, v: string) => void map.set(k, v),
      map,
    };
  }

  it("defaults closed, round-trips, and survives a throwing storage", () => {
    const storage = memoryStorage();
    expect(readContextPanelOpen(storage)).toBe(false);
    writeContextPanelOpen(storage, true);
    expect(storage.map.get(CONTEXT_PANEL_OPEN_KEY)).toBe("1");
    expect(readContextPanelOpen(storage)).toBe(true);
    writeContextPanelOpen(storage, false);
    expect(readContextPanelOpen(storage)).toBe(false);
    expect(readContextPanelOpen(null)).toBe(false);
    const broken = {
      getItem: () => {
        throw new Error("denied");
      },
      setItem: () => {
        throw new Error("denied");
      },
    };
    expect(readContextPanelOpen(broken)).toBe(false);
    expect(() => writeContextPanelOpen(broken, true)).not.toThrow();
  });

  it("isNarrowViewport: no matchMedia (SSR / jsdom) counts as wide", () => {
    expect(isNarrowViewport(undefined)).toBe(false);
    expect(isNarrowViewport({})).toBe(false);
    expect(isNarrowViewport({ matchMedia: () => ({ matches: true }) })).toBe(true);
    expect(isNarrowViewport({ matchMedia: () => ({ matches: false }) })).toBe(false);
  });
});
