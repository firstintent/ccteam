// V0.3.2 F54 — ccteam-flavored sidebar.
//
// The AoE original was 770 lines wired to AoE's Workspace / Session
// domain (rename, long-press context menu, branch labels, notification
// presets). F53 deleted every consumer; F54 replaces the file with a
// thin nav tree over `DashboardRow[]` grouped by team → kind. F55 will
// reuse this component on project / session detail pages by keeping
// the same prop shape (so the active highlight just moves).
//
// Groups:
//   team → ["workflow" | "flex" | "multi_workflow" | other...]
//
// Kind order is stable (workflow first, flex second, multi_workflow
// third, anything else after) so collapsing one team doesn't jumble
// the others. Inside a kind, slugs are sorted alphabetically.
//
// Width / collapse state isn't persisted yet — F55 may revive the
// drag-to-resize handle once detail pages are in.

import { memo, useMemo } from "react";
import { Link } from "react-router-dom";
import type { DashboardRow } from "../lib/dashboardApi";

interface Props {
  /** Flat list straight off `fetchDashboard()`. Sidebar groups
   *  internally so consumers don't need to pre-sort. */
  projects: DashboardRow[];
  /** Slug of the currently-active project, or null on `/`. Highlights
   *  the matching row so the user keeps their place when navigating
   *  detail → dashboard → detail. */
  activeSlug?: string | null;
}

const KIND_ORDER = ["workflow", "flex", "multi_workflow"];

interface GroupedKind {
  kind: string;
  projects: DashboardRow[];
}

interface GroupedTeam {
  team: string;
  kinds: GroupedKind[];
}

function groupProjects(projects: DashboardRow[]): GroupedTeam[] {
  // team → kind → rows. Using plain objects (not Maps) so the sort
  // step below can stay synchronous and the output is JSON-friendly
  // if we ever want to memoize via JSON.stringify.
  const byTeam: Record<string, Record<string, DashboardRow[]>> = {};
  for (const p of projects) {
    const team = p.team || "(unknown)";
    const kind = p.kind || "workflow";
    if (!byTeam[team]) byTeam[team] = {};
    if (!byTeam[team][kind]) byTeam[team][kind] = [];
    byTeam[team][kind].push(p);
  }

  const teams = Object.keys(byTeam).sort();
  return teams.map((team) => {
    const kindMap = byTeam[team];
    const allKinds = Object.keys(kindMap);
    // Stable kind order: known kinds in KIND_ORDER first, rest
    // alphabetical at the tail.
    const ordered = [
      ...KIND_ORDER.filter((k) => allKinds.includes(k)),
      ...allKinds.filter((k) => !KIND_ORDER.includes(k)).sort(),
    ];
    return {
      team,
      kinds: ordered.map((kind) => ({
        kind,
        projects: [...kindMap[kind]].sort((a, b) =>
          a.slug.localeCompare(b.slug),
        ),
      })),
    };
  });
}

export const WorkspaceSidebar = memo(function WorkspaceSidebar({
  projects,
  activeSlug,
}: Props) {
  const groups = useMemo(() => groupProjects(projects), [projects]);

  if (groups.length === 0) {
    return (
      <aside className="w-64 shrink-0 border-r border-surface-700/30 bg-surface-800/30 p-3 text-xs text-text-dim font-mono">
        No projects yet. Run <code className="text-text-secondary">ccteam new</code> to create one.
      </aside>
    );
  }

  return (
    <aside className="w-64 shrink-0 border-r border-surface-700/30 bg-surface-800/30 overflow-y-auto">
      <div className="px-3 pt-3 pb-1 text-[11px] uppercase tracking-widest text-text-dim font-mono">
        Projects
      </div>
      <nav className="pb-3">
        {groups.map((g) => (
          <div key={g.team} className="mt-2">
            <div className="px-3 py-1 text-[11px] font-mono text-text-secondary truncate" title={g.team}>
              {g.team}
            </div>
            {g.kinds.map((k) => (
              <div key={k.kind} className="mb-1">
                <div className="px-3 pt-1 pb-0.5 text-[10px] uppercase tracking-wide text-text-dim font-mono">
                  {k.kind}
                </div>
                {k.projects.map((p) => {
                  const isActive = p.slug === activeSlug;
                  return (
                    <Link
                      key={p.slug}
                      to={`/p/${encodeURIComponent(p.slug)}`}
                      className={`block px-3 py-1.5 text-[13px] font-mono truncate transition-colors border-l-2 ${
                        isActive
                          ? "border-brand-600 bg-surface-850 text-text-primary"
                          : "border-transparent text-text-secondary hover:bg-surface-700/40"
                      }`}
                      title={`${p.slug} — ${p.current_phase}`}
                    >
                      {p.slug}
                    </Link>
                  );
                })}
              </div>
            ))}
          </div>
        ))}
      </nav>
    </aside>
  );
});
