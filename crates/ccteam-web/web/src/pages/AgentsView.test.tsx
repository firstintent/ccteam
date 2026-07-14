import { describe, expect, it, vi } from "vitest";
import { renderToString } from "react-dom/server";

vi.hoisted(() => {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const g = globalThis as any;
  if (typeof g.window === "undefined") {
    g.window = { innerWidth: 1024, addEventListener() {}, removeEventListener() {} };
  }
  if (typeof g.localStorage === "undefined") {
    g.localStorage = { getItem: () => null, setItem() {}, removeItem() {} };
  }
});

import AgentsView, { AgentsRoster, AgentsTree } from "./AgentsView";
import { flattenDelegationTree } from "../lib/agentsTree";
import type { AgentEdge, AgentNode } from "../lib/agentsApi";

describe("AgentsView (shell smoke)", () => {
  it("renders the team-view container, KPI strip + tabs (data loads async, never under SSR)", () => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
    const html = renderToString(<AgentsView />);
    expect(html).toContain('data-testid="agents-view"');
    expect(html).toContain('data-testid="agents-canvas"');
    expect(html).toContain('data-testid="agents-kpis"');
    expect(html).toContain('data-testid="agents-tab-roster"');
    expect(html).toContain('data-testid="agents-tab-timeline"');
    expect(html).toContain('data-testid="agents-tab-topology"');
    expect(html).toContain("团队");
  });

  it("the timeline tab renders the timeline strip", () => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
    const html = renderToString(<AgentsView initialTab="timeline" />);
    expect(html).toContain('data-testid="agents-timeline"');
  });

  it("renders in English when lang='en'", () => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
    const html = renderToString(<AgentsView lang="en" />);
    expect(html).toContain("Team");
  });
});

describe("AgentsRoster (pure presentational, fixture-driven)", () => {
  function fixtureNode(over: Partial<AgentNode> = {}): AgentNode {
    return {
      sid: "s0",
      slug: "demo",
      role: "brain",
      vendor: "claude",
      host: "local",
      status: "live",
      depth: 0,
      last_active: "2026-01-01T00:00:00Z",
      turn_count: 3,
      ...over,
    };
  }

  it("renders one row per session, children indented under parents", () => {
    const rows = flattenDelegationTree([
      fixtureNode({ sid: "s0" }),
      fixtureNode({ sid: "s1", parent_sid: "s0", vendor: "codex", cost_usd: 0.42 }),
    ]);
    const html = renderToString(
      <AgentsRoster rows={rows} selected="s1" pulsing={new Set(["s0"])} onSelect={() => {}} />,
    );
    expect(html).toContain('data-testid="agents-roster"');
    expect(html).toContain('data-testid="agents-roster-row-s0"');
    expect(html).toContain('data-testid="agents-roster-row-s1"');
    // Selected row carries the class; costs render; child is elbow-indented.
    expect(html).toMatch(/agents-roster-row selected"[^>]*data-testid="agents-roster-row-s1"/);
    expect(html).toContain("$0.4200");
    expect(html).toContain("agents-roster-elbow");
  });
});

describe("AgentsTree (SSR-safe, fixture-driven)", () => {
  function fixtureNode(over: Partial<AgentNode> = {}): AgentNode {
    return {
      sid: "s0",
      slug: "demo",
      role: "brain",
      vendor: "claude",
      host: "local",
      status: "live",
      depth: 0,
      last_active: "2026-01-01T00:00:00Z",
      turn_count: 3,
      ...over,
    };
  }

  it("renders nested delegation rows grouped across projects", () => {
    const nodes: AgentNode[] = [
      fixtureNode({ sid: "s0", role: "brain" }),
      fixtureNode({ sid: "s1", role: "worker", vendor: "grok", depth: 1, parent_sid: "s0" }),
      fixtureNode({ sid: "s2", role: "worker2", vendor: "codex", depth: 2, parent_sid: "s1" }),
      fixtureNode({ sid: "s3", slug: "other", role: "root", vendor: "opencode" }),
    ];
    const edges: AgentEdge[] = [
      { parent: "s0", child: "s1", active: true },
      { parent: "s1", child: "s2", active: false },
    ];
    const html = renderToString(
      <AgentsTree nodes={nodes} edges={edges} selected="s1" pulsing={new Set(["s2"])} onSelect={() => {}} />,
    );
    expect(html).toContain('data-testid="agents-tree"');
    expect(html).toContain('data-testid="agents-tree-project-demo"');
    expect(html).toContain('data-testid="agents-tree-project-other"');
    expect(html).toContain('data-testid="agents-tree-row-s0"');
    expect(html).toMatch(/agents-tree-row selected delegating"[^>]*data-testid="agents-tree-row-s1"/);
    expect(html).toMatch(/data-testid="agents-tree-row-s1"[^>]*data-delegating="true"/);
    expect(html).toMatch(/aria-level="3"[^>]*data-testid="agents-tree-row-s2"/);
    expect(html).toContain("agents-tree-indent has-parent");
    expect(html).toContain("chip opencode");
  });
});
