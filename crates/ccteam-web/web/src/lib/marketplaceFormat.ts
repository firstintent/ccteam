// v0.8.9 Phase 4 — pure (dependency-free, node-testable) formatting helpers
// for the marketplace browser + Status view + cost pill. No React / DOM here
// so the formatters can be unit-tested directly (mirrors `chatDefaults.ts`).

import type { HubPlugin, InstalledStatus } from "./marketplaceApi";

/** The marketplace category tabs, keyed by `HubPlugin.type`. The label is what
 *  the seg button shows; "Agents / Roles" covers the `agent` type (a role IS an
 *  agent `.md`). */
export const CATEGORIES: { type: HubPlugin["type"]; label: string }[] = [
  { type: "agent", label: "Agents / Roles" },
  { type: "skill", label: "Skills" },
  { type: "workflow", label: "Workflows" },
  { type: "plugin", label: "Plugins" },
];

/** Human label for a plugin's install button / pill, per `installed_status`.
 *  `not_installed` → an action ("安装"); the other two name the current state. */
export function installedStatusLabel(status: InstalledStatus): string {
  switch (status) {
    case "installed":
      return "已装";
    case "update_available":
      return "更新";
    case "not_installed":
    default:
      return "安装";
  }
}

/** Whether a plugin's install button should be a primary call-to-action
 *  (not_installed / update_available) vs an inert "已装" pill (installed). */
export function installable(status: InstalledStatus): boolean {
  return status !== "installed";
}

/** review-before-install (装前 review): whether the CARD's install action must
 *  open the detail drawer (body preview) FIRST instead of installing blind.
 *  A never-installed persona executes as an agent the moment it lands, so it
 *  must be previewable before first install → the card routes through the
 *  drawer. An `update_available` plugin was already reviewed at first install,
 *  so the card may install it directly. (No per-project decoration = global
 *  browse = treat as not_installed = preview first.) */
export function cardInstallNeedsPreview(status: InstalledStatus | undefined): boolean {
  return (status ?? "not_installed") !== "update_available";
}

/** Distinct `source` values across a catalog, sorted, with `builtin` first
 *  (it's the curated baseline) so the source `<select>` reads naturally.
 *  Drives the source filter options (after the leading "全部来源"). */
export function distinctSources(plugins: Pick<HubPlugin, "source">[]): string[] {
  const seen = new Set<string>();
  for (const p of plugins) {
    if (p.source) seen.add(p.source);
  }
  const all = Array.from(seen);
  all.sort((a, b) => {
    if (a === "builtin") return -1;
    if (b === "builtin") return 1;
    return a.localeCompare(b);
  });
  return all;
}

/** Case-insensitive substring match of a query against a plugin's id / name /
 *  description / tags. Empty / whitespace query matches everything. */
export function matchesQuery(plugin: HubPlugin, query: string): boolean {
  const q = query.trim().toLowerCase();
  if (q.length === 0) return true;
  if (plugin.id.toLowerCase().includes(q)) return true;
  if (plugin.name.toLowerCase().includes(q)) return true;
  if (plugin.description.toLowerCase().includes(q)) return true;
  return plugin.tags.some((t) => t.toLowerCase().includes(q));
}

/** Apply the three marketplace filters (category type, source, search) to a
 *  catalog. `source === null` (or "") = all sources. Pure so the filtering is
 *  unit-testable independent of React state. */
export function filterPlugins<T extends HubPlugin>(
  plugins: T[],
  opts: { type: HubPlugin["type"]; source: string | null; query: string },
): T[] {
  return plugins.filter(
    (p) =>
      p.type === opts.type &&
      (!opts.source || p.source === opts.source) &&
      matchesQuery(p, opts.query),
  );
}

// ---- cost / status formatting ---------------------------------------------

/** Format a USD amount as `$X.XX` (2dp). Negative / NaN clamp to `$0.00`. */
export function formatUsd(usd: number): string {
  const v = Number.isFinite(usd) && usd > 0 ? usd : 0;
  return `$${v.toFixed(2)}`;
}

/** The cost-pill / cost-card label: `$cost / $cap` when a cap is set, else just
 *  `$cost`. `null` cap (no budget configured) → no denominator. */
export function formatCostBudget(costUsd: number, capUsd: number | null): string {
  if (capUsd === null || !Number.isFinite(capUsd) || capUsd <= 0) {
    return formatUsd(costUsd);
  }
  return `${formatUsd(costUsd)} / ${formatUsd(capUsd)}`;
}

/** Fraction of the 24h budget consumed (0..∞), or `null` when no cap is set.
 *  Drives the pill's warn color + the Status "near budget" banner. */
export function budgetFraction(costUsd: number, capUsd: number | null): number | null {
  if (capUsd === null || !Number.isFinite(capUsd) || capUsd <= 0) return null;
  const cost = Number.isFinite(costUsd) && costUsd > 0 ? costUsd : 0;
  return cost / capUsd;
}

/** Severity of the current spend against the 24h budget, for color + warnings.
 *   - `ok`    : no cap, or under the warn threshold (< 75%).
 *   - `warn`  : at/over the warn threshold but under the cap (75%–100%).
 *   - `over`  : at/over the cap (≥ 100%) — the daemon may auto-disable here.
 *  Thresholds match the prototype's "near 24h budget" language (89% = amber). */
export type BudgetSeverity = "ok" | "warn" | "over";
export const BUDGET_WARN_FRACTION = 0.75;
export function budgetSeverity(costUsd: number, capUsd: number | null): BudgetSeverity {
  const frac = budgetFraction(costUsd, capUsd);
  if (frac === null) return "ok";
  if (frac >= 1) return "over";
  if (frac >= BUDGET_WARN_FRACTION) return "warn";
  return "ok";
}

/** A per-vendor cost split sorted by descending spend, each formatted
 *  `vendor $X.XX` (e.g. `claude $1.62 · codex $0.52` when joined with " · ").
 *  Zero-only / empty maps yield `[]`. */
export function vendorCostSplit(byVendor: Record<string, number>): string[] {
  return Object.entries(byVendor)
    .filter(([, usd]) => Number.isFinite(usd) && usd > 0)
    .sort((a, b) => b[1] - a[1])
    .map(([vendor, usd]) => `${vendor} ${formatUsd(usd)}`);
}
