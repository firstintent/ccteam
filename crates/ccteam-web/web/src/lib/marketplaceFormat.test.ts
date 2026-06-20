// v0.8.9 Phase 4 — pure formatter tests (no fetch / no DOM). These guard the
// marketplace filter logic + the cost/budget formatting that the cost pill +
// Status view depend on.

import { describe, expect, it } from "vitest";

import type { HubPlugin } from "./marketplaceApi";
import {
  budgetFraction,
  BUDGET_WARN_FRACTION,
  budgetSeverity,
  cardInstallNeedsPreview,
  CATEGORIES,
  distinctSources,
  filterPlugins,
  formatCostBudget,
  formatUsd,
  installable,
  installedStatusLabel,
  matchesQuery,
  vendorCostSplit,
} from "./marketplaceFormat";

function plugin(over: Partial<HubPlugin> = {}): HubPlugin {
  return {
    id: "code-reviewer",
    type: "agent",
    name: "Code Reviewer",
    description: "line-by-line review + security",
    path: "agents/code-reviewer.md",
    content_sha: "abc",
    source: "agency-agents",
    upstream: "",
    license: "MIT",
    tags: ["review", "security"],
    ...over,
  };
}

describe("CATEGORIES (browse tabs)", () => {
  it("includes a Plugins tab for vendor-native plugin entries", () => {
    const plugin = CATEGORIES.find((c) => c.type === "plugin");
    expect(plugin?.label).toBe("Plugins");
  });

  it("filters a plugin-type entry under the plugin category", () => {
    const entries = [
      plugin({ id: "code-reviewer", type: "agent" }),
      plugin({ id: "understand-anything", type: "plugin", source: "external" }),
    ];
    const got = filterPlugins(entries, { type: "plugin", source: null, query: "" });
    expect(got.map((p) => p.id)).toEqual(["understand-anything"]);
  });
});

describe("installedStatusLabel + installable", () => {
  it("labels each installed_status", () => {
    expect(installedStatusLabel("not_installed")).toBe("安装");
    expect(installedStatusLabel("installed")).toBe("已装");
    expect(installedStatusLabel("update_available")).toBe("更新");
  });

  it("treats only `installed` as not-installable (an inert pill)", () => {
    expect(installable("not_installed")).toBe(true);
    expect(installable("update_available")).toBe(true);
    expect(installable("installed")).toBe(false);
  });
});

describe("cardInstallNeedsPreview (review-before-install gate)", () => {
  it("routes a never-installed plugin through the drawer (preview first)", () => {
    // not_installed (+ the global browse-only undefined case) must preview the
    // body before it can land, because installed personas execute as agents.
    expect(cardInstallNeedsPreview("not_installed")).toBe(true);
    expect(cardInstallNeedsPreview(undefined)).toBe(true);
  });

  it("lets an already-reviewed update install directly from the card", () => {
    // update_available was previewed at first install → one-click update.
    expect(cardInstallNeedsPreview("update_available")).toBe(false);
  });

  it("does not gate an already-installed plugin (it shows an inert pill)", () => {
    // `installed` never renders an install button (它 is an 已装 pill), so the
    // gate is moot — but it should not report a preview need either.
    expect(cardInstallNeedsPreview("installed")).toBe(true);
  });
});

describe("distinctSources", () => {
  it("returns sorted distinct sources with builtin first", () => {
    const got = distinctSources([
      { source: "agency-agents" },
      { source: "builtin" },
      { source: "agency-agents" },
      { source: "zoo" },
    ]);
    expect(got).toEqual(["builtin", "agency-agents", "zoo"]);
  });

  it("skips empty sources", () => {
    expect(distinctSources([{ source: "" }, { source: "builtin" }])).toEqual(["builtin"]);
  });
});

describe("matchesQuery", () => {
  it("matches id / name / description / tags case-insensitively", () => {
    const p = plugin();
    expect(matchesQuery(p, "")).toBe(true);
    expect(matchesQuery(p, "  ")).toBe(true);
    expect(matchesQuery(p, "CODE")).toBe(true); // id
    expect(matchesQuery(p, "reviewer")).toBe(true); // name
    expect(matchesQuery(p, "security")).toBe(true); // desc + tag
    expect(matchesQuery(p, "nonsense")).toBe(false);
  });
});

describe("filterPlugins", () => {
  const agents = [
    plugin({ id: "a1", type: "agent", source: "builtin", name: "Alpha", tags: [] }),
    plugin({ id: "a2", type: "agent", source: "agency-agents", name: "Beta", tags: [] }),
  ];
  const skills = [plugin({ id: "s1", type: "skill", source: "builtin", name: "Sk", tags: [] })];
  const all = [...agents, ...skills];

  it("filters by category type", () => {
    expect(filterPlugins(all, { type: "agent", source: null, query: "" }).map((p) => p.id)).toEqual(
      ["a1", "a2"],
    );
    expect(filterPlugins(all, { type: "skill", source: null, query: "" }).map((p) => p.id)).toEqual(
      ["s1"],
    );
    expect(filterPlugins(all, { type: "workflow", source: null, query: "" })).toEqual([]);
  });

  it("filters by source (null = all)", () => {
    expect(
      filterPlugins(all, { type: "agent", source: "builtin", query: "" }).map((p) => p.id),
    ).toEqual(["a1"]);
  });

  it("filters by search query within the category", () => {
    expect(
      filterPlugins(all, { type: "agent", source: null, query: "beta" }).map((p) => p.id),
    ).toEqual(["a2"]);
  });
});

describe("formatUsd", () => {
  it("formats to 2dp with a leading $", () => {
    expect(formatUsd(2.1)).toBe("$2.10");
    expect(formatUsd(2.145)).toBe("$2.15");
    expect(formatUsd(0)).toBe("$0.00");
  });

  it("clamps negatives / NaN to $0.00", () => {
    expect(formatUsd(-1)).toBe("$0.00");
    expect(formatUsd(Number.NaN)).toBe("$0.00");
  });
});

describe("formatCostBudget", () => {
  it("shows $cost / $cap when a cap is set", () => {
    expect(formatCostBudget(2.14, 20)).toBe("$2.14 / $20.00");
  });

  it("shows just $cost when no cap (null / 0)", () => {
    expect(formatCostBudget(2.14, null)).toBe("$2.14");
    expect(formatCostBudget(2.14, 0)).toBe("$2.14");
  });
});

describe("budgetFraction + budgetSeverity", () => {
  it("computes the consumed fraction, or null with no cap", () => {
    expect(budgetFraction(10, 20)).toBe(0.5);
    expect(budgetFraction(10, null)).toBeNull();
    expect(budgetFraction(10, 0)).toBeNull();
  });

  it("severity: ok < warn-threshold ≤ warn < 100% ≤ over", () => {
    expect(budgetSeverity(1, 20)).toBe("ok"); // 5%
    expect(budgetSeverity(20 * BUDGET_WARN_FRACTION, 20)).toBe("warn"); // exactly threshold
    expect(budgetSeverity(18, 20)).toBe("warn"); // 90%
    expect(budgetSeverity(20, 20)).toBe("over"); // 100%
    expect(budgetSeverity(25, 20)).toBe("over"); // >100%
    expect(budgetSeverity(999, null)).toBe("ok"); // no cap = never warn
  });
});

describe("vendorCostSplit", () => {
  it("sorts by descending spend, formats `vendor $X.XX`", () => {
    expect(vendorCostSplit({ claude: 1.62, codex: 0.52 })).toEqual([
      "claude $1.62",
      "codex $0.52",
    ]);
    // re-orders when codex outspends claude
    expect(vendorCostSplit({ claude: 0.1, codex: 5 })).toEqual(["codex $5.00", "claude $0.10"]);
  });

  it("drops zero / empty entries", () => {
    expect(vendorCostSplit({})).toEqual([]);
    expect(vendorCostSplit({ claude: 0 })).toEqual([]);
  });
});
