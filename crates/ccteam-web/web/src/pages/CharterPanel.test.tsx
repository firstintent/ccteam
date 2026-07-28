// v0.9.11 TEAM-2 — charter tab (roster + editor) node-env suite. Same
// conventions as AgentsView.test.tsx: `renderToString` proves structure;
// click wiring on the hook-free views is exercised by walking the element
// tree. The full save chain is covered piecewise (node env, no DOM):
// button → onSave here, PUT wire shape in routingApi.test.ts, and the
// saved-state transition in charterState.test.ts.

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

import CharterPanel, { CharterEditorView, VendorRosterCards, type RosterHost } from "./CharterPanel";
import { charterReducer, initialCharter, type CharterState } from "../lib/charterState";
import type { RoutingDoc } from "../lib/routingApi";
import type { AgentNode } from "../lib/agentsApi";
import type { AgentHealth } from "../lib/hostsApi";

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

function fixtureAgent(over: Partial<AgentHealth> = {}): AgentHealth {
  return {
    vendor: "claude",
    harness_id: "claude-code",
    installed: true,
    version: "2.1.34",
    bin: "/usr/bin/claude",
    mcp_registered: true,
    mcp_registrable: true,
    status: "ready",
    hint: null,
    ...over,
  };
}

function fixtureDoc(over: Partial<RoutingDoc> = {}): RoutingDoc {
  return {
    exists: true,
    source: "project",
    path: "/srv/demo/.ccteam/routing.md",
    fallback_path: null,
    content: "# 分工\ncodex builds\n",
    sha256: "abc123",
    updated_at: "2026-07-29T00:00:00+00:00",
    ...over,
  };
}

function loaded(doc: RoutingDoc): CharterState {
  return charterReducer(initialCharter, { kind: "loaded", doc });
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

const noHandlers = {
  onStartDraft: () => {},
  onEdit: () => {},
  onTogglePreview: () => {},
  onSave: () => {},
};

describe("VendorRosterCards (health + graph aggregation)", () => {
  const hosts: RosterHost[] = [
    {
      host: "local",
      hostname: "box",
      agents: [
        fixtureAgent(),
        fixtureAgent({
          vendor: "codex",
          status: "needs_config",
          hint: "codex login",
          version: "0.55.0",
        }),
        fixtureAgent({ vendor: "kimi", installed: false, version: null, status: "not_installed" }),
      ],
    },
  ];
  const nodes = [
    fixtureNode({ sid: "s0", vendor: "claude", status: "live", cost_usd: 0.25 }),
    fixtureNode({ sid: "s1", vendor: "claude", status: "live", cost_usd: 0.1 }),
    fixtureNode({ sid: "s2", vendor: "claude", status: "idle", cost_usd: 0.05 }),
    fixtureNode({ sid: "s3", vendor: "codex", status: "idle", cost_usd: 0.02 }),
    // Another host's claude session must NOT count into local's card.
    fixtureNode({ sid: "s4", vendor: "claude", host: "gpu-1", cost_usd: 9.99 }),
  ];

  it("renders one card per (host, vendor) with exact API health + live/Σcost from the graph", () => {
    const html = renderToString(<VendorRosterCards hosts={hosts} nodes={nodes} />);
    expect(html).toContain('data-testid="charter-roster"');
    expect(html).toContain('data-testid="charter-roster-card-local-claude"');
    expect(html).toContain("2.1.34");
    expect(html).toContain("就绪"); // ready badge
    expect(html).toContain("●2"); // claude: 2 live of 3 local sessions
    expect(html).toContain("$0.40"); // claude local Σ: .25+.1+.05 (not the gpu-1 9.99)
    // needs_config renders the API's remediation hint verbatim.
    expect(html).toContain("需配置");
    expect(html).toContain("codex login");
    expect(html).toContain("$0.02");
    // not_installed stays honest (no invented version/auth state).
    expect(html).toContain("未安装");
    // Single host → no host label on cards.
    expect(html).not.toContain("charter-roster-host");
  });

  it("shows the host label only on a multi-host fleet; empty roster renders nothing", () => {
    const multi = renderToString(
      <VendorRosterCards
        hosts={[...hosts, { host: "gpu-1", hostname: "gpu-1", agents: [fixtureAgent()] }]}
        nodes={nodes}
      />,
    );
    expect(multi).toContain("charter-roster-host");
    expect(multi).toContain('data-testid="charter-roster-card-gpu-1-claude"');
    expect(multi).toContain("$9.99"); // gpu-1's claude aggregates its own host only
    expect(renderToString(<VendorRosterCards hosts={[]} nodes={nodes} />)).toBe("");
  });
});

describe("CharterEditorView (state machine faces)", () => {
  it("project source opens the editor clean: textarea + disabled save", () => {
    const html = renderToString(
      <CharterEditorView state={loaded(fixtureDoc())} {...noHandlers} />,
    );
    expect(html).toContain('data-testid="charter-textarea"');
    expect(html).toContain("codex builds");
    expect(html).toMatch(/data-testid="charter-save"[^>]*disabled/);
    expect(html).toContain("/srv/demo/.ccteam/routing.md");
    // No draft CTAs when the project file already exists.
    expect(html).not.toContain("charter-copy-draft");
  });

  it("a dirty draft enables save and shows 未保存; save click fires onSave", () => {
    const dirty = charterReducer(loaded(fixtureDoc()), { kind: "edit", content: "v2" });
    const html = renderToString(<CharterEditorView state={dirty} {...noHandlers} />);
    expect(html).not.toMatch(/data-testid="charter-save"[^>]*disabled/);
    expect(html).toContain("未保存");

    const onSave = vi.fn();
    const clicks = collectOnClicks(
      CharterEditorView({ state: dirty, ...noHandlers, onSave }),
    );
    // [编辑, 预览, 保存] in render order — the last click is the save.
    expect(clicks).toHaveLength(3);
    clicks[2]!();
    expect(onSave).toHaveBeenCalledTimes(1);
  });

  it("global source is read-only with both CTAs; 拷入起稿 starts a copy draft", () => {
    const globalDoc = fixtureDoc({
      source: "global",
      fallback_path: "/home/u/.ccteam/routing.md",
    });
    const html = renderToString(
      <CharterEditorView state={loaded(globalDoc)} {...noHandlers} />,
    );
    expect(html).toContain('data-testid="charter-global-note"');
    expect(html).toContain("/home/u/.ccteam/routing.md");
    expect(html).toContain("codex builds"); // global content rendered read-only
    expect(html).not.toContain("charter-textarea");
    expect(html).toContain('data-testid="charter-copy-draft"');
    expect(html).toContain('data-testid="charter-blank-draft"');

    const onStartDraft = vi.fn();
    const clicks = collectOnClicks(
      CharterEditorView({ state: loaded(globalDoc), ...noHandlers, onStartDraft }),
    );
    expect(clicks).toHaveLength(2); // [拷入起稿, 空白起稿]
    clicks[0]!();
    expect(onStartDraft).toHaveBeenCalledWith("copy");
    clicks[1]!();
    expect(onStartDraft).toHaveBeenCalledWith("blank");

    // …and the machine turns that CTA into a dirty, editable draft.
    const drafted = charterReducer(loaded(globalDoc), { kind: "start-draft", from: "copy" });
    const draftedHtml = renderToString(<CharterEditorView state={drafted} {...noHandlers} />);
    expect(draftedHtml).toContain('data-testid="charter-textarea"');
    expect(draftedHtml).toContain("未保存");
  });

  it("source none offers 空白起稿 only; preview mode renders markdown instead of the textarea", () => {
    const none = fixtureDoc({ source: "none", exists: false, content: "", sha256: null, updated_at: null });
    const html = renderToString(<CharterEditorView state={loaded(none)} {...noHandlers} />);
    expect(html).toContain('data-testid="charter-none-note"');
    expect(html).toContain('data-testid="charter-blank-draft"');
    expect(html).not.toContain("charter-copy-draft");

    const previewing = charterReducer(
      charterReducer(loaded(fixtureDoc()), { kind: "edit", content: "# 标题\n" }),
      { kind: "toggle-preview" },
    );
    const previewHtml = renderToString(<CharterEditorView state={previewing} {...noHandlers} />);
    expect(previewHtml).not.toContain("charter-textarea");
    expect(previewHtml).toContain("charter-preview");
    expect(previewHtml).toContain("标题");
  });

  it("a saved receipt shows the short sha; a save failure surfaces inline", () => {
    let s = charterReducer(loaded(fixtureDoc()), { kind: "edit", content: "v2" });
    s = charterReducer(s, {
      kind: "saved",
      result: { sha256: "deadbeefcafe0000", updated_at: "2026-07-29T01:02:03+00:00" },
    });
    const html = renderToString(<CharterEditorView state={s} {...noHandlers} />);
    expect(html).toContain("deadbeef"); // short sha
    expect(html).toContain("2026-07-29 01:02");

    const failed = charterReducer(
      charterReducer(s, { kind: "edit", content: "v3" }),
      { kind: "save-failed", error: "HTTP 413" },
    );
    const failedHtml = renderToString(<CharterEditorView state={failed} {...noHandlers} />);
    expect(failedHtml).toContain("HTTP 413");
    expect(failedHtml).toContain("charter-textarea"); // draft survives the failure
  });
});

describe("CharterPanel (shell smoke)", () => {
  it("renders roster/editor scaffolding + the standing honesty note", () => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
    const html = renderToString(<CharterPanel nodes={[]} />);
    expect(html).toContain('data-testid="charter-panel"');
    expect(html).toContain('data-testid="charter-honesty"');
    expect(html).toContain("MCP status");
    expect(html).toContain("分工宪章");
  });

  it("renders in English when lang='en'", () => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
    const html = renderToString(<CharterPanel nodes={[]} lang="en" />);
    expect(html).toContain("Division-of-labor charter");
    expect(html).toContain("never injected");
  });
});
