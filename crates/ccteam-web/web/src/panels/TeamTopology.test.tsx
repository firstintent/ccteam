// V0.5.0 F96 — TeamTopology smoke tests.
//
// vitest in this workspace runs without @testing-library/react, so we
// instead use React's `renderToString` to assert the rendered HTML
// shape. We're verifying: each member renders a card with the right
// badge (📝 ad-hoc vs ↗ definition); state badges line up with
// backendType; and an empty teammate list still renders the lead.

import { describe, expect, it } from "vitest";
import { renderToString } from "react-dom/server";
import { MemoryRouter } from "react-router-dom";
import { TeamTopology } from "./TeamTopology";
import type { InboxMessage, TeamConfig, TeamMember } from "../lib/teamsApi";

function member(overrides: Partial<TeamMember>): TeamMember {
  return {
    agent_id: "x@team",
    name: "x",
    agent_type: "general-purpose",
    model: "sonnet",
    color: null,
    joined_at: null,
    cwd: null,
    prompt: "inline prompt",
    subscriptions: [],
    tmux_pane_id: "in-process",
    backend_type: "in-process",
    plan_mode_required: null,
    definition_backed: false,
    ...overrides,
  };
}

function config(members: TeamMember[]): TeamConfig {
  return {
    name: "roblog",
    description: "blog",
    created_at: null,
    lead_agent_id: null,
    lead_session_id: null,
    members,
  };
}

function renderTopology(
  cfg: TeamConfig,
  idle: Set<string> = new Set(),
  recent: InboxMessage[] = [],
): string {
  return renderToString(
    <MemoryRouter>
      <TeamTopology config={cfg} idleTeammates={idle} recentMessages={recent} />
    </MemoryRouter>,
  );
}

describe("TeamTopology", () => {
  it("renders the ad-hoc badge for general-purpose members", () => {
    const cfg = config([
      member({ name: "team-lead", agent_type: "team-lead", definition_backed: false }),
      member({
        name: "researcher",
        agent_type: "general-purpose",
        definition_backed: false,
        color: "blue",
      }),
    ]);
    const html = renderTopology(cfg);
    expect(html).toContain("data-testid=\"member-card-team-lead\"");
    expect(html).toContain("data-testid=\"member-card-researcher\"");
    expect(html).toContain("data-testid=\"badge-adhoc\"");
    expect(html).not.toContain("data-testid=\"badge-definition\"");
  });

  it("renders the definition badge for non-ad-hoc agentType", () => {
    const cfg = config([
      member({
        name: "code-reviewer",
        agent_type: "code-reviewer",
        definition_backed: true,
      }),
    ]);
    const html = renderTopology(cfg);
    expect(html).toContain("data-testid=\"badge-definition\"");
    expect(html).toContain("↗ definition");
  });

  // V0.5.1 F104b — Anthropic's built-in `Explore` subagent type ships
  // without a `.claude/agents/Explore.md`. Backend now flags these as
  // `definition_backed: false`, so the SPA renders the ad-hoc badge
  // and shows the real agent_type label (NOT "unknown") and the
  // model. The "↗ definition" / "definition missing" path is never
  // taken.
  it("renders Explore built-in teammates as ad-hoc with their real label", () => {
    const cfg = config([
      member({
        name: "rust-core-explorer",
        agent_type: "Explore",
        model: "haiku",
        definition_backed: false,
      }),
    ]);
    const html = renderTopology(cfg);
    expect(html).toContain("data-testid=\"member-card-rust-core-explorer\"");
    // Real agent_type / model labels show, not the "unknown" fallback.
    expect(html).toContain("Explore");
    expect(html).toContain("haiku");
    expect(html).not.toContain("unknown");
    // Ad-hoc badge wins, definition link / missing banner absent.
    expect(html).toContain("data-testid=\"badge-adhoc\"");
    expect(html).not.toContain("data-testid=\"badge-definition\"");
    expect(html).not.toContain("data-testid=\"definition-missing\"");
  });

  it("renders 'idle' state badge when teammate is in the idle set", () => {
    const cfg = config([
      member({ name: "researcher", backend_type: "in-process" }),
    ]);
    const html = renderTopology(cfg, new Set(["researcher"]));
    expect(html).toContain("data-testid=\"state-badge-idle\"");
  });

  it("renders 'in-process' state when backend matches and not idle", () => {
    const cfg = config([
      member({ name: "researcher", backend_type: "in-process" }),
    ]);
    const html = renderTopology(cfg);
    expect(html).toContain("data-testid=\"state-badge-in-process\"");
  });

  it("renders 'tmux' state for tmux backend", () => {
    const cfg = config([
      member({ name: "dev", backend_type: "tmux" }),
    ]);
    const html = renderTopology(cfg);
    expect(html).toContain("data-testid=\"state-badge-tmux\"");
  });

  it("renders 'missing' state when backendType is empty string", () => {
    const cfg = config([
      member({ name: "team-lead", agent_type: "team-lead", backend_type: "" }),
    ]);
    const html = renderTopology(cfg);
    expect(html).toContain("data-testid=\"state-badge-missing\"");
  });

  it("renders nothing in the grid when there are zero teammates besides lead", () => {
    const cfg = config([
      member({ name: "team-lead", agent_type: "team-lead", backend_type: "" }),
    ]);
    const html = renderTopology(cfg);
    expect(html).toContain("data-testid=\"member-card-team-lead\"");
    // The grid still exists (so the layout stays stable) but contains
    // no member cards.
    expect(html).toContain("data-testid=\"topology-grid\"");
  });
});
