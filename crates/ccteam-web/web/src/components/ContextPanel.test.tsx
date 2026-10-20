// @vitest-environment jsdom
// The context panel mounted for real (jsdom + mocked fetch): the header line,
// the stacked bar (deferred excluded), category order + expand, the messages
// efficiency tables, the cache line, the honest `breakdown:null` copy, the
// snapshot badge, Refresh → re-fetch, a shown failure, turn-end re-fetch, and
// Escape / × closing.

import { act } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { SessionContextResponse } from "../lib/contextApi";
import { ContextPanel } from "./ContextPanel";

const BREAKDOWN: SessionContextResponse = {
  sid: "s12",
  live: true,
  breakdown: {
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
        id: "custom_agents",
        name: "Custom agents",
        tokens: 145,
        kind: "used",
        items: [{ name: "cto", source: "projectSettings", detail: "/repo/.claude/agents/cto.md", tokens: 145, loaded: null }],
      },
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
  },
};

const realFetch = globalThis.fetch;

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

/** A fetch that answers every call with a FRESH response (a Response body
 *  reads once, so a shared mockResolvedValue would break the second call). */
function fetchAnswering(body: unknown, status = 200) {
  return vi.fn(() => Promise.resolve(jsonResponse(status, body)));
}

describe("ContextPanel", () => {
  let container: HTMLDivElement;
  let root: ReturnType<typeof createRoot>;

  beforeEach(() => {
    container = document.createElement("div");
    document.body.appendChild(container);
  });

  afterEach(async () => {
    await act(async () => root.unmount());
    container.remove();
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  async function mount(ui: React.ReactNode) {
    await act(async () => {
      root = createRoot(container);
      root.render(ui);
    });
    await flush();
  }
  /** Let the fetch → json → setState chain settle. */
  async function flush() {
    await act(async () => {
      await new Promise((resolve) => setTimeout(resolve, 0));
    });
  }
  const $ = (selector: string) => container.querySelector(selector);
  const $$ = (selector: string) => Array.from(container.querySelectorAll(selector));
  const text = (selector: string) => $(selector)?.textContent ?? "";
  const panel = (over: Partial<React.ComponentProps<typeof ContextPanel>> = {}) => (
    <ContextPanel sid="s12" lang="en" onClose={() => {}} refreshKey={undefined} {...over} />
  );

  it("fetches on mount and renders the header: model, used / window · %, live badge", async () => {
    globalThis.fetch = fetchAnswering(BREAKDOWN);
    await mount(panel());
    expect(globalThis.fetch).toHaveBeenCalledTimes(1);
    expect(String(vi.mocked(globalThis.fetch).mock.calls[0]![0])).toBe("/api/v1/sessions/s12/context");
    expect($('[data-testid="context-panel"]')).not.toBeNull();
    expect(text('[data-testid="ctx-model"]')).toBe("claude-sonnet-5-5");
    expect(text('[data-testid="ctx-usage"]')).toBe("253.8k / 1M · 25.4%");
    expect($('[data-testid="ctx-usage"]')!.getAttribute("title")).toContain("253,770 / 1,000,000");
    expect($('[data-testid="ctx-live"]')).not.toBeNull();
    expect($('[data-testid="ctx-snapshot"]')).toBeNull();
    expect($('[data-testid="ctx-loading"]')).toBeNull();
  });

  it("stacks used (desc) → buffer → free in the bar; deferred is NOT a segment", async () => {
    globalThis.fetch = fetchAnswering(BREAKDOWN);
    await mount(panel());
    const ids = $$('[data-testid="ctx-meter"] .ctx-seg').map((el) => el.getAttribute("data-testid"));
    expect(ids).toEqual([
      "ctx-seg-messages",
      "ctx-seg-memory_files",
      "ctx-seg-system_tools",
      "ctx-seg-system_prompt",
      "ctx-seg-mcp_tools",
      "ctx-seg-custom_agents",
      "ctx-seg-autocompact_buffer",
      "ctx-seg-free_space",
    ]);
    expect($('[data-testid="ctx-seg-mcp_tools_deferred"]')).toBeNull();
    expect($('[data-testid="ctx-seg-system_tools_deferred"]')).toBeNull();
    const messages = $('[data-testid="ctx-seg-messages"]') as HTMLElement;
    expect(messages.style.flexBasis).toBe("23.7146%");
    expect(messages.style.background).toContain("--ctx-1");
    expect(messages.getAttribute("title")).toBe("Messages · 237,146 · 23.7%");
    // The auto-compact threshold is marked on the bar.
    expect(($('[data-testid="ctx-threshold"]') as HTMLElement).style.left).toBe("96.7%");
  });

  it("lists categories by tokens desc (used first), deferred apart, muted, uncounted", async () => {
    globalThis.fetch = fetchAnswering(BREAKDOWN);
    await mount(panel());
    const rows = $$('[data-testid="ctx-categories"] tr.ctx-cat').map((el) => el.getAttribute("data-testid"));
    expect(rows).toEqual([
      "ctx-cat-messages",
      "ctx-cat-memory_files",
      "ctx-cat-system_tools",
      "ctx-cat-system_prompt",
      "ctx-cat-mcp_tools",
      "ctx-cat-custom_agents",
      "ctx-cat-autocompact_buffer",
      "ctx-cat-free_space",
    ]);
    const messagesRow = $('[data-testid="ctx-cat-messages"]')!;
    expect(messagesRow.textContent).toContain("237.1k");
    expect(messagesRow.textContent).toContain("23.7%");
    // Deferred: its own muted section, named as not counted, % column a dash.
    const deferred = $('[data-testid="ctx-deferred"]')!;
    expect(deferred.className).toContain("ctx-muted");
    expect(deferred.textContent).toContain("Deferred tools (not counted)");
    const deferredRows = $$('[data-testid="ctx-deferred-table"] tr.ctx-cat').map((el) => el.getAttribute("data-testid"));
    expect(deferredRows).toEqual(["ctx-cat-system_tools_deferred", "ctx-cat-mcp_tools_deferred"]);
    expect($('[data-testid="ctx-cat-mcp_tools_deferred"] td.num:last-child')!.textContent).toBe("—");
    expect($('[data-testid="ctx-categories"] [data-testid="ctx-cat-mcp_tools_deferred"]')).toBeNull();
  });

  it("expands a category into its items (largest first), marks not-loaded ones", async () => {
    globalThis.fetch = fetchAnswering(BREAKDOWN);
    await mount(panel());
    expect($$('[data-testid="ctx-item"]')).toHaveLength(0);
    const expand = $('[data-testid="ctx-expand-memory_files"]') as HTMLButtonElement;
    expect(expand.getAttribute("aria-expanded")).toBe("false");
    await act(async () => expand.click());
    expect(expand.getAttribute("aria-expanded")).toBe("true");
    const items = $$('[data-testid="ctx-categories"] [data-testid="ctx-item"]');
    expect(items).toHaveLength(2);
    expect(items[0]!.textContent).toContain("/repo/CLAUDE.md");
    expect(items[0]!.textContent).toContain("Project");
    expect(items[0]!.textContent).toContain("11.5k");
    expect(items[1]!.textContent).toContain("/home/u/.claude/CLAUDE.md");
    // A plain row (no items) has no expander.
    expect($('[data-testid="ctx-expand-messages"]')).toBeNull();
    // Detail rides as a secondary line + tooltip.
    await act(async () => ($('[data-testid="ctx-expand-custom_agents"]') as HTMLButtonElement).click());
    const cto = $$('[data-testid="ctx-item"]').find((el) => el.textContent?.includes("cto"))!;
    expect(cto.textContent).toContain("/repo/.claude/agents/cto.md");
    expect(cto.querySelector("td.name")!.getAttribute("title")).toContain("/repo/.claude/agents/cto.md");
    // Deferred items carry the not-loaded mark.
    await act(async () => ($('[data-testid="ctx-expand-mcp_tools_deferred"]') as HTMLButtonElement).click());
    const deferredItem = $('[data-testid="ctx-deferred-table"] [data-testid="ctx-item"]')!;
    expect(deferredItem.querySelector('[data-testid="ctx-not-loaded"]')!.textContent).toBe("not loaded");
    // Collapse memory files again — its two items go, the still-open custom
    // agents row keeps its one.
    await act(async () => expand.click());
    expect(expand.getAttribute("aria-expanded")).toBe("false");
    const remaining = $$('[data-testid="ctx-categories"] [data-testid="ctx-item"]');
    expect(remaining).toHaveLength(1);
    expect(remaining[0]!.textContent).toContain("cto");
  });

  it("messages: split by bucket, per-tool table by total desc, attachments desc", async () => {
    globalThis.fetch = fetchAnswering(BREAKDOWN);
    await mount(panel());
    const split = $$('[data-testid="ctx-split"] tr').map((el) => el.getAttribute("data-testid"));
    expect(split).toEqual([
      "ctx-split-attachment",
      "ctx-split-assistant",
      "ctx-split-tool_result",
      "ctx-split-tool_call",
      "ctx-split-user",
    ]);
    expect(text('[data-testid="ctx-split-attachment"]')).toContain("Attachments");
    expect(text('[data-testid="ctx-split-attachment"]')).toContain("69.5k");
    const tools = $$('[data-testid="ctx-tool-row"]').map((el) => el.getAttribute("data-tool"));
    expect(tools).toEqual(["Bash", "mcp__ccteam__agent"]);
    const bash = $('[data-testid="ctx-tool-row"][data-tool="Bash"]')!;
    expect(Array.from(bash.querySelectorAll("td")).map((td) => td.textContent)).toEqual([
      "Bash",
      "34.4k",
      "61.7k",
      "96.2k",
    ]);
    const attachments = $$('[data-testid="ctx-attachment-row"]').map((el) => el.getAttribute("data-attachment"));
    expect(attachments).toEqual(["prompt_snapshot", "async_hook_response"]);
  });

  it("last request: cache hit ratio = read / (read + write + uncached)", async () => {
    globalThis.fetch = fetchAnswering(BREAKDOWN);
    await mount(panel());
    expect(text('[data-testid="ctx-cache-hit"]')).toBe("cache hit 99.6%");
    expect(text('[data-testid="ctx-last-request"]')).toContain("cache read 252.2k");
    expect(text('[data-testid="ctx-last-request"]')).toContain("cache write 1k");
    expect(text('[data-testid="ctx-last-request"]')).toContain("uncached input 2");
    expect(text('[data-testid="ctx-last-request"]')).toContain("output 1.3k");
  });

  it("degrades per section: no messages / last_request / threshold → those parts are absent", async () => {
    globalThis.fetch = fetchAnswering({
      sid: "s12",
      live: true,
      breakdown: {
        used_tokens: 10,
        window_tokens: 100,
        categories: [{ id: "messages", name: "Messages", tokens: 10, kind: "used" }],
        messages: null,
        last_request: null,
        unknown_extra: { ignored: true },
      },
    });
    await mount(panel());
    expect($('[data-testid="ctx-messages"]')).toBeNull();
    expect($('[data-testid="ctx-last-request"]')).toBeNull();
    expect($('[data-testid="ctx-threshold"]')).toBeNull();
    expect($('[data-testid="ctx-deferred"]')).toBeNull();
    expect($('[data-testid="ctx-model"]')).toBeNull();
    expect(text('[data-testid="ctx-usage"]')).toBe("10 / 100 · 10%");
    expect($$('[data-testid="ctx-meter"] .ctx-seg')).toHaveLength(1);
  });

  it("breakdown:null → an honest explanation, no bar, no error", async () => {
    globalThis.fetch = fetchAnswering({ sid: "s12", live: false, breakdown: null });
    await mount(panel());
    expect(text('[data-testid="ctx-none"]')).toContain("No context breakdown for this session yet.");
    expect(text('[data-testid="ctx-none"]')).toContain("Only Claude stream-json sessions on this host");
    expect($('[data-testid="ctx-meter"]')).toBeNull();
    expect($('[data-testid="ctx-error"]')).toBeNull();
    expect($('[data-testid="ctx-meta"]')).toBeNull();
  });

  it("zh copy says harness, never vendor", async () => {
    globalThis.fetch = fetchAnswering({ sid: "s12", live: false, breakdown: null });
    await mount(panel({ lang: "zh" }));
    const copy = text('[data-testid="ctx-none"]');
    expect(copy).toContain("harness");
    expect(copy.toLowerCase()).not.toContain("vendor");
  });

  it("live:false → a snapshot badge with the capture time, not a live badge", async () => {
    globalThis.fetch = fetchAnswering({
      ...BREAKDOWN,
      live: false,
      breakdown: { ...BREAKDOWN.breakdown, captured_at: "2025-01-01T08:00:00Z" },
    });
    await mount(panel());
    expect($('[data-testid="ctx-live"]')).toBeNull();
    const badge = $('[data-testid="ctx-snapshot"]')!;
    expect(badge.textContent).toMatch(/^snapshot · \d+ d ago$/);
    expect(badge.getAttribute("title")).toContain("captured");
  });

  it("Refresh re-fetches, shows a busy state while pending, then the new numbers", async () => {
    let release: (() => void) | null = null;
    const first = Promise.resolve(jsonResponse(200, BREAKDOWN));
    const second = new Promise<Response>((resolve) => {
      release = () =>
        resolve(
          jsonResponse(200, {
            ...BREAKDOWN,
            breakdown: { ...BREAKDOWN.breakdown, used_tokens: 300_000 },
          }),
        );
    });
    const fetchMock = vi.fn().mockReturnValueOnce(first).mockReturnValueOnce(second);
    globalThis.fetch = fetchMock;
    await mount(panel());
    expect(fetchMock).toHaveBeenCalledTimes(1);
    const refresh = $('[data-testid="ctx-refresh"]') as HTMLButtonElement;
    expect(refresh.disabled).toBe(false);
    await act(async () => refresh.click());
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect(refresh.disabled).toBe(true);
    expect(refresh.getAttribute("aria-busy")).toBe("true");
    expect(refresh.getAttribute("aria-label")).toBe("Refreshing…");
    // The old numbers stay on screen while the refresh is pending.
    expect(text('[data-testid="ctx-usage"]')).toBe("253.8k / 1M · 25.4%");
    await act(async () => release!());
    await flush();
    expect(refresh.disabled).toBe(false);
    expect(text('[data-testid="ctx-usage"]')).toBe("300k / 1M · 30%");
  });

  it("a failed fetch is shown inline (never silent), and Retry re-fetches", async () => {
    const fetchMock = vi
      .fn()
      .mockReturnValueOnce(Promise.resolve(jsonResponse(503, { error: "no gateway (standalone web)" })))
      .mockReturnValueOnce(Promise.resolve(jsonResponse(200, BREAKDOWN)));
    globalThis.fetch = fetchMock;
    await mount(panel());
    const error = $('[data-testid="ctx-error"]')!;
    expect(error.getAttribute("role")).toBe("alert");
    expect(error.textContent).toContain("Context load failed: HTTP 503: no gateway (standalone web)");
    expect($('[data-testid="ctx-loading"]')).toBeNull();
    await act(async () => (error.querySelector("button") as HTMLButtonElement).click());
    await flush();
    expect(fetchMock).toHaveBeenCalledTimes(2);
    expect($('[data-testid="ctx-error"]')).toBeNull();
    expect($('[data-testid="ctx-live"]')).not.toBeNull();
  });

  it("a failed Refresh keeps the last good data on screen next to the error", async () => {
    const fetchMock = vi
      .fn()
      .mockReturnValueOnce(Promise.resolve(jsonResponse(200, BREAKDOWN)))
      // Rejected lazily — an eager Promise.reject counts as unhandled until fetch consumes it.
      .mockImplementationOnce(() => Promise.reject(new TypeError("Failed to fetch")));
    globalThis.fetch = fetchMock;
    await mount(panel());
    await act(async () => ($('[data-testid="ctx-refresh"]') as HTMLButtonElement).click());
    await flush();
    expect(text('[data-testid="ctx-error"]')).toContain("network: Failed to fetch");
    expect(text('[data-testid="ctx-usage"]')).toBe("253.8k / 1M · 25.4%");
  });

  it("re-fetches when a turn ends (refreshKey changes) — no polling", async () => {
    globalThis.fetch = fetchAnswering(BREAKDOWN);
    await mount(panel({ refreshKey: undefined }));
    expect(globalThis.fetch).toHaveBeenCalledTimes(1);
    await act(async () => {
      root.render(panel({ refreshKey: "evt-41" }));
    });
    await flush();
    expect(globalThis.fetch).toHaveBeenCalledTimes(2);
    // Same key again (an unrelated re-render) does not re-fetch.
    await act(async () => {
      root.render(panel({ refreshKey: "evt-41", lang: "en" }));
    });
    await flush();
    expect(globalThis.fetch).toHaveBeenCalledTimes(2);
  });

  it("Escape inside the panel and the × button both close it; focus lands in the panel when asked", async () => {
    globalThis.fetch = fetchAnswering(BREAKDOWN);
    const onClose = vi.fn();
    await mount(panel({ onClose, focusOnMount: true }));
    const aside = $('[data-testid="context-panel"]') as HTMLElement;
    expect(document.activeElement).toBe(aside);
    await act(async () => {
      aside.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    });
    expect(onClose).toHaveBeenCalledTimes(1);
    await act(async () => ($('[data-testid="ctx-close"]') as HTMLButtonElement).click());
    expect(onClose).toHaveBeenCalledTimes(2);
    await act(async () => ($('[data-testid="ctx-backdrop"]') as HTMLButtonElement).click());
    expect(onClose).toHaveBeenCalledTimes(3);
  });
});
