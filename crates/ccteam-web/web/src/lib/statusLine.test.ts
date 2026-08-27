// Pure statusline formatter tests (no fetch / no DOM). Guard the passthrough
// of the server-rendered line + the structured fallback the SessionView bar
// renders.

import { describe, expect, it } from "vitest";

import type { SessionStatus } from "./sessionsApi";
import { contextPct, formatContext, formatStatusLine, formatTurnStatus, humanizeTokens } from "./statusLine";

function status(over: Partial<SessionStatus> = {}): SessionStatus {
  return {
    sid: "s8",
    model: null,
    context: null,
    status_line: null,
    ...over,
  };
}

describe("humanizeTokens", () => {
  it("renders whole millions as <n>M and everything ≥1000 as <n>k", () => {
    expect(humanizeTokens(1_000_000)).toBe("1M");
    expect(humanizeTokens(200_000)).toBe("200k");
    expect(humanizeTokens(188_000)).toBe("188k");
    expect(humanizeTokens(1500)).toBe("2k"); // rounded
  });

  it("keeps a sub-1000 count raw and clamps non-positive / non-finite to 0", () => {
    expect(humanizeTokens(512)).toBe("512");
    expect(humanizeTokens(0)).toBe("0");
    expect(humanizeTokens(-1)).toBe("0");
    expect(humanizeTokens(Number.NaN)).toBe("0");
  });

  it("uses the k-form for a non-whole number of millions", () => {
    // 1.5M is not a whole million → falls to the k branch.
    expect(humanizeTokens(1_500_000)).toBe("1500k");
  });
});

describe("formatContext", () => {
  it("renders `ctx used / window (pct%)` with a rounded pct", () => {
    expect(
      formatContext({ used_tokens: 188_000, window_tokens: 1_000_000, pct: 18.8 }),
    ).toBe("ctx 188k / 1M (19%)");
  });

  it("handles a 200k window", () => {
    expect(
      formatContext({ used_tokens: 100_000, window_tokens: 200_000, pct: 50 }),
    ).toBe("ctx 100k / 200k (50%)");
  });

  // A known window with no reported occupancy (a just-resumed ACP session)
  // must read as unknown — rendering it as `0 (0%)` claims an empty context.
  it("renders a dash when occupancy is unknown", () => {
    expect(
      formatContext({ used_tokens: null, window_tokens: 500_000, pct: null }),
    ).toBe("ctx — / 500k (usage unknown)");
    expect(
      formatContext({ used_tokens: null, window_tokens: 0, pct: null }),
    ).toBe("ctx —");
  });

  it("renders the used count alone when the window is unknown", () => {
    expect(formatContext({ used_tokens: 5_000, window_tokens: 0, pct: null })).toBe(
      "ctx 5k (window unknown)",
    );
  });
});

describe("formatStatusLine", () => {
  it("returns the server-rendered status_line verbatim when present", () => {
    const line = "claude-opus-4-8[1m] · ctx 188k / 1M (19%)";
    expect(formatStatusLine(status({ status_line: line, model: "ignored" }))).toBe(line);
  });

  it("falls back to model + context when there is no status_line", () => {
    expect(
      formatStatusLine(
        status({
          model: "claude-opus-4-8[1m]",
          context: { used_tokens: 188_000, window_tokens: 1_000_000, pct: 18.8 },
        }),
      ),
    ).toBe("claude-opus-4-8[1m] · ctx 188k / 1M (19%)");
  });

  it("builds from a 1M window vs a 200k window in the fallback", () => {
    expect(
      formatStatusLine(
        status({
          model: "m",
          context: { used_tokens: 50_000, window_tokens: 1_000_000, pct: 5 },
        }),
      ),
    ).toBe("m · ctx 50k / 1M (5%)");
    expect(
      formatStatusLine(
        status({
          model: "m",
          context: { used_tokens: 50_000, window_tokens: 200_000, pct: 25 },
        }),
      ),
    ).toBe("m · ctx 50k / 200k (25%)");
  });

  it("falls back to model alone or context alone when only one is present", () => {
    expect(formatStatusLine(status({ model: "claude-opus-4-8[1m]" }))).toBe(
      "claude-opus-4-8[1m]",
    );
    expect(
      formatStatusLine(
        status({ context: { used_tokens: 188_000, window_tokens: 1_000_000, pct: 19 } }),
      ),
    ).toBe("ctx 188k / 1M (19%)");
  });

  it("returns null for an all-null (brand-new) session → render nothing", () => {
    expect(formatStatusLine(status())).toBeNull();
  });

  it("treats an empty / whitespace status_line as absent (falls through)", () => {
    expect(formatStatusLine(status({ status_line: "   " }))).toBeNull();
    expect(formatStatusLine(status({ status_line: "", model: "m" }))).toBe("m");
  });
});

describe("formatTurnStatus", () => {
  it("renders turn and context only — model and ledger never repeat in the footer", () => {
    expect(
      formatTurnStatus({
        model: "gpt-5.3-codex",
        context: { used_tokens: 85, window_tokens: 100, pct: 85 },
        turn: 7,
        cost_usd: 0.42,
        tokens_total: 22_008_310,
      }),
    ).toEqual({ text: "turn 7 · ctx 85%⚠", warn: true });
  });

  it("derives context percentage from serialized token counts", () => {
    expect(
      formatTurnStatus({
        context: { used_tokens: 19, window_tokens: 100 },
        turn: 1,
      }),
    ).toEqual({ text: "turn 1 · ctx 19%", warn: false });
    expect(contextPct({ used_tokens: 19, window_tokens: 100 })).toBe(19);
  });

  it("omits unknown context and renders nothing without a status", () => {
    expect(formatTurnStatus({ turn: 2, tokens_total: 12_345 })).toEqual({ text: "turn 2", warn: false });
    expect(formatTurnStatus()).toBeNull();
  });
});
