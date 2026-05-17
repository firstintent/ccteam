// V0.5.0 F96 — Topology panel.
//
// CSS grid layout (no D3 — keeps the bundle small and avoids an extra
// dep + canvas in the SPA). Lead node sits in the centre column with
// teammates arranged in a responsive grid around it. Each node renders:
//
//   - color avatar + name + agentType
//   - 📝 ad-hoc badge OR ↗ definition link (definition_backed?)
//   - state halo (in-process / tmux / missing / idle)
//   - cwd (truncated)
//   - subscriptions edges → text list "subscribed to: x, y" inline
//
// Definition click → fetches the .md and opens a modal. Ad-hoc click →
// opens a modal with the inline prompt straight off `member.prompt`.
//
// IMPORTANT: panel must render quickly with whatever fields are
// present. Anthropic schema is best-effort tolerated server-side; the
// UI doesn't add a second layer of validation. Missing color falls
// back to a neutral chip via `colorClasses(null)`.

import { memo, useMemo, useState } from "react";
import type {
  AgentDefinition,
  InboxMessage,
  TeamConfig,
  TeamMember,
} from "../lib/teamsApi";
import {
  colorClasses,
  deriveMemberState,
  fetchMemberDefinition,
} from "../lib/teamsApi";

interface Props {
  config: TeamConfig;
  /** Set of teammate names that emitted an idle notification within
   *  the recent SSE window. The parent (TeamDetailPage) maintains
   *  this from `team_teammate_idle` events; F94 is the source after
   *  Wave 2 — until then the set is empty. */
  idleTeammates: Set<string>;
  /** Recent inbox messages used to render the "subscribed to" hover
   *  hint. The parent passes the latest N messages; the panel
   *  groups them per-sender. */
  recentMessages: InboxMessage[];
}

interface ModalState {
  member: TeamMember;
  loading: boolean;
  error: string | null;
  definition: AgentDefinition | null;
  definitionMissing: boolean;
}

export const TeamTopology = memo(function TeamTopology({
  config,
  idleTeammates,
  recentMessages,
}: Props) {
  const [modal, setModal] = useState<ModalState | null>(null);
  const lead = config.members.find((m) => m.agent_type === "team-lead") ?? null;
  const teammates = config.members.filter((m) => m !== lead);

  const recentBySender = useMemo(() => {
    const out: Record<string, InboxMessage[]> = {};
    for (const m of recentMessages) {
      const key = m.from;
      if (!out[key]) out[key] = [];
      out[key].push(m);
    }
    return out;
  }, [recentMessages]);

  const openMember = async (m: TeamMember) => {
    if (!m.definition_backed) {
      // Ad-hoc — inline prompt modal, no network.
      setModal({
        member: m,
        loading: false,
        error: null,
        definition: null,
        definitionMissing: false,
      });
      return;
    }
    setModal({
      member: m,
      loading: true,
      error: null,
      definition: null,
      definitionMissing: false,
    });
    try {
      const resp = await fetchMemberDefinition(config.name, m.name);
      setModal({
        member: m,
        loading: false,
        error: null,
        definition: resp.definition,
        definitionMissing: resp.definition_missing,
      });
    } catch (err) {
      setModal({
        member: m,
        loading: false,
        error: err instanceof Error ? err.message : String(err),
        definition: null,
        definitionMissing: false,
      });
    }
  };

  return (
    <div data-testid="topology-panel" className="flex flex-col gap-4 p-4">
      {lead && (
        <div className="flex justify-center">
          <MemberCard
            member={lead}
            isLead
            idle={idleTeammates.has(lead.name)}
            recent={recentBySender[lead.name] ?? []}
            onClick={() => openMember(lead)}
          />
        </div>
      )}
      <div
        data-testid="topology-grid"
        className="grid grid-cols-[repeat(auto-fit,minmax(220px,1fr))] gap-4"
      >
        {teammates.map((m) => (
          <MemberCard
            key={m.agent_id}
            member={m}
            isLead={false}
            idle={idleTeammates.has(m.name)}
            recent={recentBySender[m.name] ?? []}
            onClick={() => openMember(m)}
          />
        ))}
      </div>
      {modal && <MemberModal state={modal} onClose={() => setModal(null)} />}
    </div>
  );
});

interface CardProps {
  member: TeamMember;
  isLead: boolean;
  idle: boolean;
  recent: InboxMessage[];
  onClick: () => void;
}

function MemberCard({ member, isLead, idle, recent, onClick }: CardProps) {
  const colors = colorClasses(member.color);
  const state = deriveMemberState(member.backend_type, idle);
  const haloClass = haloClassFor(state);
  return (
    <button
      type="button"
      onClick={onClick}
      data-testid={`member-card-${member.name}`}
      className={`text-left bg-surface-800/60 hover:bg-surface-800 border ${colors.border} rounded-lg p-3 transition-colors cursor-pointer flex flex-col gap-2 min-w-0 ${haloClass}`}
    >
      <div className="flex items-center gap-2 min-w-0">
        <span
          aria-hidden
          className={`shrink-0 w-7 h-7 rounded-full flex items-center justify-center font-mono text-xs ${colors.bg} ${colors.text}`}
        >
          {member.name.slice(0, 2).toUpperCase()}
        </span>
        <div className="flex flex-col min-w-0 flex-1">
          <span
            className="font-mono text-sm text-text-primary truncate"
            title={member.name}
          >
            {member.name}
            {isLead && (
              <span className="ml-1 text-[10px] text-text-dim uppercase tracking-wide">
                lead
              </span>
            )}
          </span>
          <span className="text-[11px] text-text-dim font-mono truncate">
            {member.agent_type ?? "unknown"} · {member.model ?? "—"}
          </span>
        </div>
      </div>
      <div className="flex items-center gap-1 flex-wrap">
        {member.definition_backed ? (
          <span
            data-testid="badge-definition"
            className="text-[10px] font-mono px-1.5 py-0.5 rounded bg-accent-600/10 text-accent-600 border border-accent-600/30"
          >
            ↗ definition
          </span>
        ) : (
          <span
            data-testid="badge-adhoc"
            className="text-[10px] font-mono px-1.5 py-0.5 rounded bg-surface-700/40 text-text-secondary border border-surface-700/40"
          >
            📝 ad-hoc
          </span>
        )}
        <span
          data-testid={`state-badge-${state}`}
          className={`text-[10px] font-mono px-1.5 py-0.5 rounded ${stateBadgeClass(state)}`}
          title={`backend: ${member.backend_type ?? "—"}`}
        >
          {state}
        </span>
      </div>
      <div className="text-[10px] text-text-dim font-mono truncate" title={member.cwd ?? ""}>
        {member.cwd ?? "no cwd"}
      </div>
      {recent.length > 0 && (
        <div className="text-[10px] text-text-dim italic">
          {recent.length} recent msg{recent.length === 1 ? "" : "s"}
        </div>
      )}
    </button>
  );
}

function haloClassFor(state: ReturnType<typeof deriveMemberState>): string {
  switch (state) {
    case "in-process":
      return "ring-1 ring-blue-400/30";
    case "tmux":
      return "ring-1 ring-emerald-400/30";
    case "idle":
      return "ring-1 ring-amber-400/40";
    case "missing":
      return "ring-1 ring-surface-700/40";
  }
}

function stateBadgeClass(state: ReturnType<typeof deriveMemberState>): string {
  switch (state) {
    case "in-process":
      return "bg-blue-500/15 text-blue-300 border border-blue-400/30";
    case "tmux":
      return "bg-emerald-500/15 text-emerald-300 border border-emerald-400/30";
    case "idle":
      return "bg-amber-500/15 text-amber-300 border border-amber-400/30";
    case "missing":
      return "bg-surface-700/40 text-text-dim border border-surface-700/40";
  }
}

interface ModalProps {
  state: ModalState;
  onClose: () => void;
}

function MemberModal({ state, onClose }: ModalProps) {
  const m = state.member;
  return (
    <div
      data-testid="member-modal"
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 p-4"
      onClick={onClose}
    >
      <div
        className="bg-surface-900 border border-surface-700/40 rounded-lg max-w-3xl w-full max-h-[80vh] overflow-y-auto"
        onClick={(e) => e.stopPropagation()}
      >
        <header className="px-4 py-3 border-b border-surface-700/40 flex items-center justify-between">
          <div className="flex flex-col">
            <h2 className="font-mono text-sm text-text-primary">
              {m.name}{" "}
              <span className="text-text-dim">
                · {m.agent_type ?? "unknown"}
              </span>
            </h2>
            <span className="text-[11px] text-text-dim">
              {m.definition_backed ? "definition-backed" : "ad-hoc inline prompt"}
            </span>
          </div>
          <button
            type="button"
            onClick={onClose}
            className="text-text-dim hover:text-text-secondary text-xs font-mono cursor-pointer"
          >
            close
          </button>
        </header>
        <div className="p-4 text-xs text-text-primary font-mono leading-relaxed whitespace-pre-wrap">
          {state.loading && <div className="text-text-dim">loading…</div>}
          {state.error && (
            <div className="text-status-error">error: {state.error}</div>
          )}
          {!state.loading && !state.error && m.definition_backed && state.definitionMissing && (
            <div
              data-testid="definition-missing"
              className="rounded bg-amber-500/10 border border-amber-400/30 text-amber-300 p-3"
            >
              definition file missing:{" "}
              <code>
                .claude/agents/{m.agent_type ?? "<unknown>"}.md
              </code>
            </div>
          )}
          {!state.loading && state.definition && (
            <div className="flex flex-col gap-3">
              <div className="text-[11px] text-text-dim font-mono">
                scope: {state.definition.scope} · path: {state.definition.path}
              </div>
              {state.definition.skills_not_applied.length > 0 && (
                <div className="rounded bg-amber-500/10 border border-amber-400/30 text-amber-300 p-2 text-[11px]">
                  skills not applied as teammate:{" "}
                  {state.definition.skills_not_applied.join(", ")}
                </div>
              )}
              {state.definition.mcp_servers_not_applied.length > 0 && (
                <div className="rounded bg-amber-500/10 border border-amber-400/30 text-amber-300 p-2 text-[11px]">
                  mcpServers not applied as teammate:{" "}
                  {state.definition.mcp_servers_not_applied.join(", ")}
                </div>
              )}
              <pre className="bg-surface-800/60 rounded p-2 text-[11px] overflow-x-auto">
                {JSON.stringify(state.definition.frontmatter, null, 2)}
              </pre>
              <div data-testid="definition-body">{state.definition.body}</div>
            </div>
          )}
          {!state.loading && !state.error && !m.definition_backed && (
            <div data-testid="adhoc-prompt">
              {m.prompt ?? "(no inline prompt stored on this member)"}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
