// V0.5.0 F96 — `/teams` route, top-level list of all host teams.
//
// Cards link to `/teams/:name`. Empty state nudges the user to the
// native Claude Code Team workflow because ccteam itself doesn't
// (and shouldn't) create teams — `~/.claude/teams/` is Anthropic's
// SoT (PRD V0.5.0 §整体红线 1).
//
// Auth surface mirrors Dashboard: a 401 from `/api/v1/teams` throws
// `UNAUTHENTICATED`, which the global fetchInterceptor + TokenEntryGate
// already handle in `App.tsx`. Other failures render an inline banner.

import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import { fetchTeams, type TeamListEntry } from "../lib/teamsApi";

export default function TeamsListPage() {
  const [teams, setTeams] = useState<TeamListEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    setError(null);
    setTeams(null);
    fetchTeams()
      .then((rows) => {
        if (!cancelled) setTeams(rows);
      })
      .catch((err) => {
        if (cancelled) return;
        const msg = err instanceof Error ? err.message : String(err);
        if (msg !== "UNAUTHENTICATED") setError(msg);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  if (error) {
    return (
      <div className="p-4 text-xs text-status-error font-mono" role="alert">
        failed to load teams: {error}
      </div>
    );
  }
  if (teams === null) {
    return (
      <div className="p-4 text-xs text-text-dim font-mono">loading teams…</div>
    );
  }
  if (teams.length === 0) {
    return (
      <div
        data-testid="teams-empty"
        className="p-6 text-xs text-text-dim font-mono flex flex-col gap-2"
      >
        <span>No agent teams found.</span>
        <span>
          Try <code>/ccteam:team</code> in a Claude session, or use Anthropic's
          native team flow.
        </span>
      </div>
    );
  }
  return (
    <div data-testid="teams-list" className="p-4">
      <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-3">
        {teams.map((t) => (
          <TeamCard key={t.name} team={t} />
        ))}
      </div>
    </div>
  );
}

function TeamCard({ team }: { team: TeamListEntry }) {
  return (
    <Link
      to={`/teams/${encodeURIComponent(team.name)}`}
      data-testid={`team-card-${team.name}`}
      className="block bg-surface-800/60 hover:bg-surface-800 border border-surface-700/40 rounded-lg p-3 transition-colors flex flex-col gap-1 min-w-0"
    >
      <div className="flex items-center gap-2 min-w-0">
        <span
          className="font-mono text-sm text-text-primary truncate flex-1"
          title={team.name}
        >
          {team.name}
        </span>
        <span className="shrink-0 px-1.5 py-0.5 rounded text-[10px] font-mono uppercase tracking-wider bg-accent-600/10 text-accent-600">
          {team.member_count}{" "}
          {team.member_count === 1 ? "member" : "members"}
        </span>
      </div>
      {team.description && (
        <p
          className="text-[11px] text-text-secondary line-clamp-2"
          title={team.description}
        >
          {team.description}
        </p>
      )}
      <span className="text-[10px] text-text-dim font-mono mt-1">
        last activity: {team.last_activity ?? "—"}
      </span>
    </Link>
  );
}
