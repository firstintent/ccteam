// Home 工作区维度联动 — pure derivations, unit-tested without React.
//
// The rules (owner decision):
// - A project BINDS its hosts: an existing project can only spawn on hosts
//   that actually have that slug registered (local always qualifies — the
//   Home project list IS the local daemon registry). A new-project path is
//   created by the local daemon, so it pins host = local.
// - A host BINDS its vendors: the composer's vendor/model menu only offers
//   harnesses the picked host actually has installed (per its probe report).

import type { HostDetail, HostSummary } from "./hostsApi";
import { VENDORS, type VendorId } from "./vendors";

/** Hosts the current project selection can actually spawn on. `details` maps
 *  host id → detail (missing/null = detail fetch failed → not spawnable). */
export function eligibleHosts(
  summaries: HostSummary[],
  details: Record<string, HostDetail | null>,
  projectSlug: string,
  isNewProject: boolean,
): HostSummary[] {
  return summaries.filter((s) => {
    if (s.is_local) return true;
    if (isNewProject) return false; // new projects are created on the local daemon
    const d = details[s.host];
    if (!d) return false; // offline / never heartbeated — remote spawn would fail
    if (!d.agents.some((a) => a.installed)) return false; // nothing to spawn with
    return (d.projects ?? []).some((p) => p.slug === projectSlug);
  });
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
