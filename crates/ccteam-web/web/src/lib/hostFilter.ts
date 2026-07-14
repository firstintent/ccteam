// Home 工作区维度联动 — pure derivations, unit-tested without React.
//
// The rules (owner decision):
// - A project BINDS exactly one host. Existing projects inherit that host;
//   new projects may be created on any online host with an installed agent.
// - A host BINDS its vendors: the composer's vendor/model menu only offers
//   harnesses the picked host actually has installed (per its probe report).

import type { HostDetail, HostSummary } from "./hostsApi";
import { VENDORS, type VendorId } from "./vendors";

/** Hosts the current project selection can actually spawn on. `details` maps
 *  host id → detail (missing/null = detail fetch failed → not spawnable).
 *  `projectHost` is the existing project's binding, not its slug. */
export function eligibleHosts(
  summaries: HostSummary[],
  details: Record<string, HostDetail | null>,
  projectHost: string,
  isNewProject: boolean,
): HostSummary[] {
  if (!isNewProject) {
    const bound = projectHost || "local";
    const known = summaries.find((summary) => summary.host === bound);
    return known
      ? [known]
      : [{
          host: bound,
          hostname: bound,
          is_local: bound === "local",
          status: bound === "local" ? "online" : "offline",
          agent_count: 0,
          agents_ready: 0,
        }];
  }

  return summaries
    .filter((summary) => {
      if (summary.is_local || summary.host === "local") return true;
      if (summary.status !== "online") return false;
      return details[summary.host]?.agents.some((agent) => agent.installed) ?? false;
    })
    .sort((a, b) => Number(b.is_local || b.host === "local") - Number(a.is_local || a.host === "local"));
}

/** Vendors installed on the picked host, in VENDORS menu order. `null` means
 *  "don't filter" (detail unknown, or probe reported nothing installed —
 *  fail open so the menu never goes empty; the spawn error stays honest). */
export function allowedVendorsFor(detail: HostDetail | null | undefined): VendorId[] | null {
  if (!detail) return null;
  const installed = new Set(detail.agents.filter((a) => a.installed).map((a) => a.vendor));
  const list = VENDORS.filter((v) => installed.has(v.id)).map((v) => v.id);
  return list.length > 0 ? list : null;
}
