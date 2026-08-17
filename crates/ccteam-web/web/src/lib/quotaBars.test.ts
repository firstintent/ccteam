// VENDOR-QUOTA-1 — the quota mini-bar presentation helpers: bar cells,
// compact durations, reset hints (relative < 24h / weekday < 7d / date
// beyond), and the per-vendor line selection (available only, max two).

import { describe, expect, it } from "vitest";
import {
  compactDuration,
  quotaBar,
  quotaLines,
  quotaPlan,
  quotaWindowLine,
  resetHint,
} from "./quotaBars";
import type { VendorQuota } from "./vendorQuotaApi";

const NOW = new Date("2026-08-17T12:00:00Z");

describe("quotaBar", () => {
  it("fills round(percent/20) cells, clamped to 0..5", () => {
    expect(quotaBar(0)).toBe("░░░░░");
    expect(quotaBar(15)).toBe("▓░░░░");
    expect(quotaBar(42)).toBe("▓▓░░░");
    expect(quotaBar(50)).toBe("▓▓▓░░");
    expect(quotaBar(55)).toBe("▓▓▓░░");
    expect(quotaBar(95)).toBe("▓▓▓▓▓");
    expect(quotaBar(100)).toBe("▓▓▓▓▓");
    expect(quotaBar(137)).toBe("▓▓▓▓▓");
    expect(quotaBar(-5)).toBe("░░░░░");
  });
});

describe("compactDuration", () => {
  it("renders m / h+m / d+h compactly", () => {
    expect(compactDuration(45 * 60_000)).toBe("45m");
    expect(compactDuration(3 * 3_600_000 + 12 * 60_000)).toBe("3h12m");
    expect(compactDuration(3 * 3_600_000)).toBe("3h");
    expect(compactDuration(26 * 3_600_000)).toBe("1d02h");
    expect(compactDuration(-1000)).toBe("0m");
  });
});

describe("resetHint", () => {
  it("is relative under 24h, weekday under 7d, a short date beyond", () => {
    expect(resetHint("2026-08-17T15:12:00Z", NOW, "en")).toBe("resets in 3h12m");
    expect(resetHint("2026-08-17T15:12:00Z", NOW, "zh")).toBe("3h12m后重置");
    // 3 days out → weekday (derived with the same formatter, tz-independent).
    const at = new Date("2026-08-20T00:00:00Z");
    const weekday = at.toLocaleDateString("en-US", { weekday: "short" });
    expect(resetHint(at.toISOString(), NOW, "en")).toBe(`resets ${weekday}`);
    // 10 days out → short date.
    const far = new Date("2026-08-27T00:00:00Z");
    const date = far.toLocaleDateString("en-US", { month: "short", day: "numeric" });
    expect(resetHint(far.toISOString(), NOW, "en")).toBe(`resets ${date}`);
  });

  it("handles past / missing / malformed reset times without crashing", () => {
    expect(resetHint("2026-08-17T11:00:00Z", NOW, "en")).toBe("resets soon");
    expect(resetHint(null, NOW, "en")).toBeNull();
    expect(resetHint("not-a-date", NOW, "en")).toBeNull();
  });
});

describe("quotaLines / quotaPlan (row selection)", () => {
  const available: VendorQuota = {
    vendor: "claude",
    state: "available",
    plan: "max",
    windows: [
      { kind: "five_hour", used_percent: 42, resets_at: "2026-08-17T15:12:00Z" },
      { kind: "weekly", used_percent: 15, resets_at: "2026-08-20T00:00:00Z" },
    ],
  };

  it("available with two windows → two lines, 5h first", () => {
    const lines = quotaLines(available, NOW, "en");
    expect(lines).toHaveLength(2);
    expect(lines[0]).toBe("5h ▓▓░░░ 42% · resets in 3h12m");
    expect(lines[1]).toContain("Week ▓░░░░ 15% · resets");
  });

  it("single-window vendors render exactly one line", () => {
    const one: VendorQuota = {
      vendor: "kimi",
      state: "available",
      windows: [{ kind: "weekly", used_percent: 4, resets_at: null }],
    };
    expect(quotaLines(one, NOW, "en")).toEqual(["Week ░░░░░ 4%"]);
  });

  it("not_subscription / unavailable / missing render NOTHING", () => {
    expect(quotaLines({ vendor: "codex", state: "not_subscription" }, NOW, "en")).toEqual([]);
    expect(quotaLines({ vendor: "grok", state: "unavailable" }, NOW, "en")).toEqual([]);
    expect(quotaLines(null, NOW, "en")).toEqual([]);
    expect(quotaLines(undefined, NOW, "en")).toEqual([]);
    // Available but windowless (plan-only subscriber) renders no bar either.
    expect(quotaLines({ vendor: "codex", state: "available", plan: "go" }, NOW, "en")).toEqual([]);
  });

  it("the plan badge shows only for an available row with a plan", () => {
    expect(quotaPlan(available)).toBe("max");
    expect(quotaPlan({ vendor: "kimi", state: "available", windows: [] })).toBeNull();
    expect(quotaPlan({ vendor: "grok", state: "unavailable" })).toBeNull();
    expect(quotaPlan(null)).toBeNull();
  });

  it("quotaWindowLine works in zh too", () => {
    expect(
      quotaWindowLine(
        { kind: "five_hour", used_percent: 42, resets_at: "2026-08-17T15:12:00Z" },
        NOW,
        "zh",
      ),
    ).toBe("5h ▓▓░░░ 42% · 3h12m后重置");
  });
});
