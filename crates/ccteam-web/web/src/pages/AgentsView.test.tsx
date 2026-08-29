// v0.9.11 TEAM-1 — topology-first team view. Node-env suite (no DOM):
// `renderToString` proves structure/links (Links need a Router context →
// MemoryRouter); click wiring on the hook-free presentational components
// (AgentsTicker / VendorKpiChips) is exercised by walking their element tree
// and invoking `onClick` directly.

import { describe, expect, it, vi } from "vitest";
import { renderToString } from "react-dom/server";
import { MemoryRouter } from "react-router-dom";

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

import AgentsView, {
  AgentsPanel,
  AgentsTicker,
  AgentsTree,
  TeamTabSeg,
  VendorKpiChips,
} from "./AgentsView";
import { emptyFold } from "./chatTranscript";
import type { AgentNode } from "../lib/agentsApi";
import type { TimestampedAgentsEvent } from "../lib/agentsReducer";

function fixtureNode(over: Partial<AgentNode> = {}): AgentNode {
  return {
    sid: "s0",
    slug: "demo",
    role: "brain",
    vendor: "claude",
    host: "local",
    status: "idle",
    residency: "resident",
    depth: 0,
    last_active: "2026-01-01T00:00:00Z",
    turn_count: 3,
    ...over,
  };
}

function delegationEvent(over: Partial<TimestampedAgentsEvent> = {}): TimestampedAgentsEvent {
  return {
    kind: "delegation",
    content: "",
    relation: "dispatched",
    parent_sid: "s0",
    child_sid: "s1",
    receivedAt: Date.now(),
    ...over,
  };
}

type ClickHandler = (e?: unknown) => void;

/** Collect every `onClick` prop in a (hook-free) component's element tree,
 *  in render order — the node-env stand-in for a DOM click. */
function collectOnClicks(el: unknown, out: ClickHandler[] = []): ClickHandler[] {
  if (el == null || typeof el !== "object") return out;
  if (Array.isArray(el)) {
    for (const child of el) collectOnClicks(child, out);
    return out;
  }
  const props = (el as { props?: { onClick?: unknown; children?: unknown } }).props;
  if (props) {
    if (typeof props.onClick === "function") out.push(props.onClick as ClickHandler);
    collectOnClicks(props.children, out);
  }
  return out;
}

describe("AgentsView (shell smoke)", () => {
  it("renders the team-view container + KPI strip; roster/timeline tabs are gone", () => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
    const html = renderToString(
      <MemoryRouter>
        <AgentsView />
      </MemoryRouter>,
    );
    expect(html).toContain('data-testid="agents-view"');
    expect(html).toContain('data-testid="agents-canvas"');
    expect(html).toContain('data-testid="agents-kpis"');
    expect(html).toContain("团队");
    expect(html).not.toContain("agents-tab-");
    expect(html).not.toContain("agents-roster");
    expect(html).not.toContain("agents-timeline");
  });

  it("renders in English when lang='en'", () => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
    const html = renderToString(
      <MemoryRouter>
        <AgentsView lang="en" />
      </MemoryRouter>,
    );
    expect(html).toContain("Team");
  });

  // v0.9.11 TEAM-2 — 拓扑|分工 seg between the ticker and the body.
  it("renders the tab seg with topology default; initialTab='charter' swaps in the charter panel", () => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
    const html = renderToString(
      <MemoryRouter>
        <AgentsView />
      </MemoryRouter>,
    );
    expect(html).toContain('data-testid="agents-seg"');
    expect(html).toContain('data-testid="agents-canvas"');
    expect(html).not.toContain('data-testid="charter-panel"');

    const charter = renderToString(
      <MemoryRouter>
        <AgentsView initialTab="charter" />
      </MemoryRouter>,
    );
    expect(charter).toContain('data-testid="charter-panel"');
    expect(charter).not.toContain('data-testid="agents-canvas"');
    // The KPI strip stays global above the seg on both tabs.
    expect(charter).toContain('data-testid="agents-kpis"');
  });
});

describe("TeamTabSeg (拓扑 | 分工)", () => {
  it("renders both tabs, active one highlighted", () => {
    const html = renderToString(<TeamTabSeg tab="topology" onSwitch={() => {}} />);
    expect(html).toContain('data-testid="agents-seg"');
    expect(html).toContain("拓扑");
    expect(html).toContain("分工");
    expect(html).toMatch(/class="active"[^>]*data-testid="agents-seg-topology"/);
    const charterActive = renderToString(<TeamTabSeg tab="charter" onSwitch={() => {}} />);
    expect(charterActive).toMatch(/class="active"[^>]*data-testid="agents-seg-charter"/);
  });

  it("clicking a tab switches to it", () => {
    const onSwitch = vi.fn();
    const clicks = collectOnClicks(TeamTabSeg({ tab: "topology", onSwitch }));
    expect(clicks).toHaveLength(2); // [拓扑, 分工]
    clicks[1]!();
    expect(onSwitch).toHaveBeenCalledWith("charter");
    clicks[0]!();
    expect(onSwitch).toHaveBeenCalledWith("topology");
  });
});

describe("AgentsTree (SSR-safe, fixture-driven)", () => {
  const nodes: AgentNode[] = [
    fixtureNode({ sid: "s0", role: "brain" }),
    fixtureNode({ sid: "s1", role: "worker", vendor: "grok", depth: 1, parent_sid: "s0" }),
    fixtureNode({ sid: "s2", role: "worker2", vendor: "codex", depth: 2, parent_sid: "s1" }),
    fixtureNode({ sid: "s3", slug: "other", role: "root", vendor: "opencode" }),
  ];

  it("renders nested delegation rows grouped across projects", () => {
    const edges = [
      { parent: "s0", child: "s1", active: true },
      { parent: "s1", child: "s2", active: false },
    ];
    const html = renderToString(
      <MemoryRouter>
        <AgentsTree nodes={nodes} edges={edges} selected="s1" pulsing={new Set(["s2"])} onSelect={() => {}} />
      </MemoryRouter>,
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

  it("every row carries a real open link to its chat route", () => {
    const html = renderToString(
      <MemoryRouter>
        <AgentsTree nodes={nodes} edges={[]} selected={null} pulsing={new Set()} onSelect={() => {}} />
      </MemoryRouter>,
    );
    expect(html).toContain('href="/chat/s/s0"');
    expect(html).toContain('href="/chat/s/s1"');
    expect(html).toContain('href="/chat/s/s3"');
    expect(html).toContain("打开 ↗");
  });

  it("every project header carries a collapse toggle; default is expanded", () => {
    const html = renderToString(
      <MemoryRouter>
        <AgentsTree nodes={nodes} edges={[]} selected={null} pulsing={new Set()} onSelect={() => {}} />
      </MemoryRouter>,
    );
    expect(html).toMatch(/aria-expanded="true"[^>]*data-testid="agents-tree-project-toggle-demo"/);
    expect(html).toMatch(/aria-expanded="true"[^>]*data-testid="agents-tree-project-toggle-other"/);
    // Rows render under the default (everything expanded).
    expect(html).toContain('data-testid="agents-tree-row-s0"');
  });

  it("a slug in the persisted collapsed set renders header-only (rows hidden, counts kept)", () => {
    const stub = globalThis.localStorage as unknown as { getItem: (key: string) => string | null };
    const original = stub.getItem;
    stub.getItem = (key: string) =>
      key === "ccteam.agents.collapsedProjects.v1" ? '["demo"]' : null;
    try {
      const html = renderToString(
        <MemoryRouter>
          <AgentsTree nodes={nodes} edges={[]} selected={null} pulsing={new Set()} onSelect={() => {}} />
        </MemoryRouter>,
      ).replace(/<!-- -->/g, "");
      // Header stays visible — slug, counts, and the toggle in collapsed state.
      expect(html).toContain('data-testid="agents-tree-project-demo"');
      expect(html).toContain("3/3");
      expect(html).toMatch(/aria-expanded="false"[^>]*data-testid="agents-tree-project-toggle-demo"/);
      // …but the project's session rows are gone; other projects are untouched.
      expect(html).not.toContain('data-testid="agents-tree-row-s0"');
      expect(html).not.toContain('data-testid="agents-tree-row-s1"');
      expect(html).not.toContain('data-testid="agents-tree-row-s2"');
      expect(html).toContain('data-testid="agents-tree-row-s3"');
    } finally {
      stub.getItem = original;
    }
  });

  it("每行展示 模型 · effort — vendor tokens verbatim, dash when nothing live", () => {
    const rows = [
      fixtureNode({ sid: "s0", model: "claude-opus-5", effort: "high" }),
      fixtureNode({ sid: "s1", model: "gpt-5.5-codex", effort: "xhigh" }),
      // A live model with no effort axis ⇒ model alone, no separator.
      fixtureNode({ sid: "s2", model: "kimi-code/k3" }),
      // Idle: the graph reports neither — never a spawn-time guess.
      fixtureNode({ sid: "s3", residency: "released" }),
    ];
    const html = renderToString(
      <MemoryRouter>
        <AgentsTree nodes={rows} edges={[]} selected={null} pulsing={new Set()} onSelect={() => {}} />
      </MemoryRouter>,
    ).replace(/<!-- -->/g, "");
    expect(html).toContain("claude-opus-5 · high");
    expect(html).toContain("gpt-5.5-codex · xhigh");
    expect(html).toContain("kimi-code/k3");
    expect(html).not.toContain("kimi-code/k3 ·");
    expect(html).toMatch(/agents-tree-model[^>]*>—</);
  });

  it("renders Pi as its own topology identity with provider/model-id", () => {
    const html = renderToString(
      <MemoryRouter>
        <AgentsTree
          nodes={[
            fixtureNode({
              sid: "s6",
              vendor: "pi",
              model: "anthropic/claude-opus-4-6",
              effort: "xhigh",
            }),
          ]}
          edges={[]}
          selected={null}
          pulsing={new Set()}
          onSelect={() => {}}
        />
      </MemoryRouter>,
    ).replace(/<!-- -->/g, "");
    expect(html).toContain('data-vendor="pi"');
    expect(html).toContain("chip pi vendor-chip");
    expect(html).toContain("anthropic/claude-opus-4-6 · xhigh");
  });

  it("the effort token never translates — zh and en render the same cell", () => {
    const render = (lang: "zh" | "en") =>
      renderToString(
        <MemoryRouter>
          <AgentsTree
            nodes={[fixtureNode({ model: "claude-opus-5", effort: "high" })]}
            edges={[]}
            selected={null}
            pulsing={new Set()}
            lang={lang}
            onSelect={() => {}}
          />
        </MemoryRouter>,
      ).replace(/<!-- -->/g, "");
    expect(render("zh")).toContain("claude-opus-5 · high");
    expect(render("en")).toContain("claude-opus-5 · high");
  });

  it("host badge renders only when the graph spans more than one host", () => {
    const multiHostNodes = [fixtureNode({ sid: "s0" }), fixtureNode({ sid: "s1", host: "gpu-1" })];
    const single = renderToString(
      <MemoryRouter>
        <AgentsTree nodes={multiHostNodes} edges={[]} hosts={["local"]} selected={null} pulsing={new Set()} onSelect={() => {}} />
      </MemoryRouter>,
    );
    expect(single).not.toContain("agents-tree-host");
    expect(single).not.toContain("with-host");

    const multi = renderToString(
      <MemoryRouter>
        <AgentsTree
          nodes={multiHostNodes}
          edges={[]}
          hosts={["local", "gpu-1"]}
          selected={null}
          pulsing={new Set()}
          onSelect={() => {}}
        />
      </MemoryRouter>,
    );
    expect(multi).toContain("agents-tree with-host");
    expect(multi).toContain("agents-tree-host");
    expect(multi).toContain("gpu-1");
  });
});

describe("AgentsPanel (pure presentational)", () => {
  it("the 打开会话 action is a real link to the session's chat route", () => {
    const html = renderToString(
      <MemoryRouter>
        <AgentsPanel
          node={fixtureNode({ sid: "s7", vendor: "codex", cost_usd: 0.42 })}
          pulsing={new Set()}
          activityFold={emptyFold()}
          history={[]}
        />
      </MemoryRouter>,
    );
    expect(html).toContain('data-testid="agents-panel"');
    expect(html).toMatch(/data-testid="agents-open-chat"[^>]*href="\/chat\/s\/s7"|href="\/chat\/s\/s7"[^>]*data-testid="agents-open-chat"/);
    expect(html).toContain("打开会话");
    expect(html).toContain("$0.4200");
    expect(html).toContain("codex");
  });
});

describe("AgentsTicker (recent dispatches)", () => {
  it("hidden when there are no delegation events", () => {
    const html = renderToString(
      <AgentsTicker events={[delegationEvent({ kind: "answer" })]} onSelect={() => {}} />,
    );
    expect(html).toBe("");
  });

  it("shows the last 5 delegation events, newest first", () => {
    const events = [1, 2, 3, 4, 5, 6].map((i) =>
      delegationEvent({ child_sid: `c${i}`, receivedAt: 1000 * i }),
    );
    const html = renderToString(<AgentsTicker events={events} onSelect={() => {}} />);
    expect(html).toContain('data-testid="agents-ticker"');
    expect(html).toContain("s0 → c6");
    expect(html).toContain("dispatched");
    expect(html).not.toContain("c1"); // capped at 5
    expect(html.indexOf("c6")).toBeLessThan(html.indexOf("c5")); // newest first
  });

  it("clicking an entry selects the child session", () => {
    const onSelect = vi.fn();
    const clicks = collectOnClicks(AgentsTicker({ events: [delegationEvent()], onSelect }));
    expect(clicks).toHaveLength(1);
    clicks[0]!();
    expect(onSelect).toHaveBeenCalledWith("s1");
  });
});

describe("VendorKpiChips (per-vendor rollup + topology filter)", () => {
  const nodes = [
    fixtureNode({ sid: "s0", vendor: "claude", cost_usd: 0.3 }),
    fixtureNode({ sid: "s1", vendor: "claude", residency: "released", cost_usd: 0.2 }),
    fixtureNode({ sid: "s2", vendor: "grok", cost_usd: 0.05, parent_sid: "s0" }),
  ];

  it("renders one chip per vendor with live count + Σcost; the active vendor is highlighted", () => {
    const html = renderToString(<VendorKpiChips nodes={nodes} active="grok" onToggle={() => {}} />);
    expect(html).toContain('data-testid="agents-vendor-chips"');
    expect(html).toContain('data-testid="agents-vendor-chip-claude"');
    expect(html).toContain("●1"); // claude: 1 live of 2 sessions
    expect(html).toContain("$0.50"); // claude: 0.3 + 0.2
    expect(html).toContain("$0.05"); // grok
    expect(html).toMatch(/agents-vendor-chip active"[^>]*data-testid="agents-vendor-chip-grok"/);
    expect(renderToString(<VendorKpiChips nodes={[]} active={null} onToggle={() => {}} />)).toBe("");
  });

  it("clicking a chip toggles the vendor filter; filtered orphans render as roots", () => {
    const onToggle = vi.fn();
    const clicks = collectOnClicks(VendorKpiChips({ nodes, active: null, onToggle }));
    expect(clicks).toHaveLength(2); // claude, grok (sorted)
    clicks[0]!();
    expect(onToggle).toHaveBeenCalledWith("claude");

    // The view feeds the tree only the filtered vendor's nodes — a child
    // whose parent got filtered out is promoted to a root (never dropped).
    const html = renderToString(
      <MemoryRouter>
        <AgentsTree
          nodes={nodes.filter((n) => n.vendor === "grok")}
          edges={[]}
          selected={null}
          pulsing={new Set()}
          onSelect={() => {}}
        />
      </MemoryRouter>,
    );
    expect(html).not.toContain('data-testid="agents-tree-row-s0"');
    expect(html).toMatch(/aria-level="1"[^>]*data-testid="agents-tree-row-s2"/);
  });
});

// v0.9.11 TEAM-7 — a charter-tab roster card clicks through to "this vendor's
// sessions". The handler is two setState calls on the SAME `vendorFilter`/`tab`
// state the chips drive, and a node-env server render can't observe a state
// update (React drops a child-triggered parent update), so the interlock is
// proven in its two halves: AgentsView really hands the charter panel a pick
// handler (module-mocked panel captures the prop), and the landing state that
// handler sets renders as the topology tab holding that vendor's rows only —
// asserted through the same expression the view applies, exactly as the
// chip-filter test above does.
describe("roster vendor pick → filtered topology (TEAM-7)", () => {
  const nodes = [
    fixtureNode({ sid: "s0", vendor: "claude" }),
    fixtureNode({ sid: "s1", vendor: "codex", cost_usd: 0.11, parent_sid: "s0" }),
    fixtureNode({ sid: "s2", vendor: "grok", residency: "released" }),
  ];

  it("hands the charter tab a pick handler", async () => {
    const seen: { onVendorPick?: (vendor: string) => void }[] = [];
    vi.doMock("./CharterPanel", () => ({
      default: (props: { onVendorPick?: (vendor: string) => void }) => {
        seen.push(props);
        return <div data-testid="charter-panel" />;
      },
    }));
    try {
      // The static import above is already cached, so the view has to be
      // re-evaluated for the mocked panel to reach it (doMock keeps its
      // registration across a module-registry reset).
      vi.resetModules();
      const { default: MountedAgentsView } = await import("./AgentsView");
      globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
      renderToString(
        <MemoryRouter>
          <MountedAgentsView initialTab="charter" />
        </MemoryRouter>,
      );
      expect(seen).toHaveLength(1);
      expect(seen[0]!.onVendorPick).toBeTypeOf("function");
    } finally {
      vi.doUnmock("./CharterPanel");
      vi.resetModules();
    }
  });

  it("the picked vendor lands on a topology tab holding only that vendor's rows", () => {
    const picked = "codex";

    // …the tab half: `setTab("topology")` puts the seg (and the canvas) back.
    const seg = renderToString(<TeamTabSeg tab="topology" onSwitch={() => {}} />);
    expect(seg).toMatch(/class="active"[^>]*data-testid="agents-seg-topology"/);

    // …the filter half: `setVendorFilter(picked)` is the tree's node source,
    // so a filtered-out parent's child is promoted, never dropped.
    const tree = renderToString(
      <MemoryRouter>
        <AgentsTree
          nodes={nodes.filter((n) => n.vendor === picked)}
          edges={[]}
          selected={null}
          pulsing={new Set()}
          onSelect={() => {}}
        />
      </MemoryRouter>,
    );
    expect(tree).toMatch(/aria-level="1"[^>]*data-testid="agents-tree-row-s1"/);
    expect(tree).not.toContain('data-testid="agents-tree-row-s0"');
    expect(tree).not.toContain('data-testid="agents-tree-row-s2"');

    // …and it IS the chips' filter, so that chip shows active (click = clear).
    const chips = renderToString(<VendorKpiChips nodes={nodes} active={picked} onToggle={() => {}} />);
    expect(chips).toMatch(/agents-vendor-chip active"[^>]*data-testid="agents-vendor-chip-codex"/);
  });
});
