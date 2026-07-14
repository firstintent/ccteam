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

import AgentsView, { AgentsGraphSvg, AgentsRoster } from "./AgentsView";
import { computeAgentsLayout } from "../lib/agentsLayout";
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

describe("AgentsGraphSvg (pure presentational, fixture-driven)", () => {
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

  it("renders N node cards + M edge paths from fixture data", () => {
    const nodes: AgentNode[] = [
      fixtureNode({ sid: "s0", role: "brain" }),
      fixtureNode({ sid: "s1", role: "worker", vendor: "grok", depth: 1, parent_sid: "s0" }),
      fixtureNode({ sid: "s2", role: "worker2", vendor: "codex", depth: 1, parent_sid: "s0" }),
    ];
    const edges: AgentEdge[] = [
      { parent: "s0", child: "s1", active: true },
      { parent: "s0", child: "s2", active: false },
    ];
    const layout = computeAgentsLayout(nodes, edges, ["local"]);
    const html = renderToString(
      <AgentsGraphSvg layout={layout} selected={"s1"} pulsing={new Set(["s1"])} onSelect={() => {}} />,
    );
    // 3 node cards.
    expect(html).toContain('data-testid="agents-node-s0"');
    expect(html).toContain('data-testid="agents-node-s1"');
    expect(html).toContain('data-testid="agents-node-s2"');
    // 2 edge paths.
    expect(html).toContain('data-testid="agents-edge-s0-s1"');
    expect(html).toContain('data-testid="agents-edge-s0-s2"');
    // The active edge is flagged; the inactive one is not.
    expect(html).toMatch(/agents-edge-s0-s1"[^>]*data-active="true"/);
    expect(html).toMatch(/agents-edge-s0-s2"[^>]*data-active="false"/);
    // The selected node carries the selected class.
    expect(html).toMatch(/agents-node selected"[^>]*data-testid="agents-node-s1"/);
    // One lane (single host).
    expect(html).toContain('data-testid="agents-lane-local"');
  });

  it("renders zero node/edge markup for an empty graph", () => {
    const layout = computeAgentsLayout([], [], []);
    const html = renderToString(
      <AgentsGraphSvg layout={layout} selected={null} pulsing={new Set()} onSelect={() => {}} />,
    );
    expect(html).not.toContain("agents-node-");
    expect(html).not.toContain("agents-edge-");
  });
});
