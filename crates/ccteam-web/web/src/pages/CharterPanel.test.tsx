// v0.9.11 TEAM-2/3/6 — charter tab (roster + 编队起手 playbooks + editor)
// node-env suite. Same conventions as AgentsView.test.tsx: `renderToString`
// proves structure (Links need a Router context → MemoryRouter); click/link
// wiring on the hook-free views is exercised by walking the element tree.
// The full save chain is covered piecewise (node env, no DOM): button →
// onSave here, PUT wire shape in routingApi.test.ts, and the saved-state
// transition in charterState.test.ts. The playbook DEFINITIONS (6 entries,
// vendors, i18n completeness) are pinned in lib/playbooks.test.ts.
//
// TEAM-6: `VendorRosterCards` stays hook-free (collapsed/confirmingHost are
// props, not internal `useState`) precisely so it's directly callable here
// the same way as `CharterEditorView`; the click-driven collapse/remove
// ORCHESTRATION (`handleRosterRemoveClick`, sort order) lives in
// `lib/charterRoster.ts` and is tested on its own in charterRoster.test.ts
// (mocking `deleteHost`) — mirroring how `charterState.ts`'s reducer is
// tested apart from the views that dispatch into it. This file only proves
// VendorRosterCards renders/wires those pieces correctly.

import { describe, expect, it, vi } from "vitest";
import { renderToString } from "react-dom/server";
import { Link, MemoryRouter } from "react-router-dom";

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

import CharterPanel, { CharterEditorView, PlaybookCards, VendorRosterCards } from "./CharterPanel";
import { charterReducer, initialCharter, type CharterState } from "../lib/charterState";
import type { RosterHost } from "../lib/charterRoster";
import { PLAYBOOKS } from "../lib/playbooks";
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
    tool_surface: "native_mcp_config",
    status: "ready",
    hint: null,
    ...over,
  };
}

function fixtureHost(over: Partial<RosterHost> = {}): RosterHost {
  return {
    host: "local",
    hostname: "box",
    status: "online",
    agents: [fixtureAgent()],
    ...over,
  };
}

/** TEAM-8: a FIXED clock for the offline-age cases. Every heartbeat fixture
 *  below is pinned relative to this, so nothing depends on when the suite
 *  runs — `VendorRosterCards` takes `nowMs` as a prop for exactly this. */
const NOW_MS = Date.UTC(2026, 6, 29, 12, 0, 0);
const hoursAgo = (h: number) => Math.floor(NOW_MS / 1000) - h * 3600;

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

/** Find the first element in a (hook-free) component's element tree whose
 *  `data-testid` matches, returning its props — the node-env stand-in for
 *  `screen.getByTestId(...)`. Needed alongside `collectOnClicks` once a
 *  render can hold several same-shaped interactive elements (one remove
 *  button per host group) that positional indexing would make fragile. */
function findByTestId(
  el: unknown,
  testid: string,
): { onClick?: ClickHandler; [key: string]: unknown } | null {
  if (el == null || typeof el !== "object") return null;
  if (Array.isArray(el)) {
    for (const child of el) {
      const found = findByTestId(child, testid);
      if (found) return found;
    }
    return null;
  }
  const props = (el as { props?: { "data-testid"?: unknown; children?: unknown } }).props;
  if (!props) return null;
  if (props["data-testid"] === testid) return props as { onClick?: ClickHandler };
  return findByTestId(props.children, testid);
}

const noHandlers = {
  onStartDraft: () => {},
  onEdit: () => {},
  onTogglePreview: () => {},
  onSave: () => {},
};

describe("VendorRosterCards (grouped by host — health + graph aggregation)", () => {
  const hosts: RosterHost[] = [
    fixtureHost({
      host: "local",
      hostname: "box",
      status: "online",
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
    }),
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
  });

  it("(a) renders each host as its own group section with hostname + ALWAYS-shown id + status", () => {
    const html = renderToString(<VendorRosterCards hosts={hosts} nodes={nodes} />);
    expect(html).toContain('data-testid="charter-roster-group-local"');
    expect(html).toContain('data-testid="charter-roster-group-head-local"');
    expect(html).toContain('data-testid="charter-roster-cards-local"');
    expect(html).toContain(">box<"); // hostname, prominent
    // The host id is ALWAYS shown — even for a single host — since that's
    // exactly the disambiguator two same-named hosts need.
    expect(html).toMatch(/charter-roster-group-id mono">local</);
    expect(html).toContain("在线"); // online status label
    // `local` never gets a remove button — it's the daemon's own machine.
    expect(html).not.toContain('data-testid="charter-roster-remove-local"');
  });

  it("(a) an offline non-local host shows its offline status + id, and a remove button", () => {
    const withSatellite: RosterHost[] = [
      ...hosts,
      fixtureHost({
        host: "smoke-self",
        hostname: "claude-dev-04",
        status: "offline",
        agents: [fixtureAgent({ vendor: "codex" })],
      }),
    ];
    const html = renderToString(
      <VendorRosterCards hosts={withSatellite} nodes={nodes} collapsed={new Set()} />,
    );
    expect(html).toContain('data-testid="charter-roster-group-smoke-self"');
    expect(html).toMatch(/charter-roster-group-id mono">smoke-self</);
    expect(html).toContain("离线"); // offline status label
    expect(html).toContain('data-testid="charter-roster-remove-smoke-self"');
    expect(html).toContain("移除"); // not armed yet → plain remove label
  });

  it("(b) sorts local first, then online before offline, keeping each bucket's own relative order", () => {
    const scrambled: RosterHost[] = [
      fixtureHost({ host: "b-offline", hostname: "b", status: "offline", agents: [] }),
      fixtureHost({ host: "a-online", hostname: "a", status: "online", agents: [] }),
      fixtureHost({ host: "local", hostname: "box", status: "online", agents: [] }),
      fixtureHost({ host: "c-offline", hostname: "c", status: "offline", agents: [] }),
    ];
    const html = renderToString(<VendorRosterCards hosts={scrambled} nodes={[]} collapsed={new Set()} />);
    const idx = (host: string) => html.indexOf(`data-testid="charter-roster-group-${host}"`);
    expect(idx("local")).toBeGreaterThanOrEqual(0);
    expect(idx("local")).toBeLessThan(idx("a-online"));
    expect(idx("a-online")).toBeLessThan(idx("b-offline")); // online before offline
    expect(idx("b-offline")).toBeLessThan(idx("c-offline")); // offline bucket keeps its own order
  });

  it("(c) an offline section's cards stay hidden while collapsed and appear once expanded", () => {
    const withSatellite: RosterHost[] = [
      fixtureHost({
        host: "smoke-self",
        hostname: "claude-dev-04",
        status: "offline",
        agents: [fixtureAgent({ vendor: "codex" })],
      }),
    ];
    const collapsedHtml = renderToString(
      <VendorRosterCards hosts={withSatellite} nodes={nodes} collapsed={new Set(["smoke-self"])} />,
    );
    expect(collapsedHtml).not.toContain('data-testid="charter-roster-cards-smoke-self"');
    expect(collapsedHtml).not.toContain('data-testid="charter-roster-card-smoke-self-codex"');

    const expandedHtml = renderToString(
      <VendorRosterCards hosts={withSatellite} nodes={nodes} collapsed={new Set()} />,
    );
    expect(expandedHtml).toContain('data-testid="charter-roster-cards-smoke-self"');
    expect(expandedHtml).toContain('data-testid="charter-roster-card-smoke-self-codex"');
  });

  it("(c) the group toggle button fires onToggleCollapse with that host's id", () => {
    const withSatellite: RosterHost[] = [
      fixtureHost({ host: "smoke-self", status: "offline", agents: [fixtureAgent({ vendor: "codex" })] }),
    ];
    const onToggleCollapse = vi.fn();
    const el = VendorRosterCards({
      hosts: withSatellite,
      nodes,
      collapsed: new Set(["smoke-self"]),
      onToggleCollapse,
    });
    const toggle = findByTestId(el, "charter-roster-group-toggle-smoke-self");
    expect(toggle?.onClick).toBeTypeOf("function");
    toggle!.onClick!();
    expect(onToggleCollapse).toHaveBeenCalledWith("smoke-self");
  });

  it("(d)/(e) clicking a host's remove button fires onRemoveClick(host, online) — never for local", () => {
    const withHosts: RosterHost[] = [
      fixtureHost({ host: "local", status: "online" }),
      fixtureHost({ host: "smoke-self", status: "offline", agents: [] }),
      fixtureHost({ host: "dxa347", status: "online", agents: [] }),
    ];
    const onRemoveClick = vi.fn();
    const el = VendorRosterCards({ hosts: withHosts, nodes, collapsed: new Set(), onRemoveClick });
    expect(findByTestId(el, "charter-roster-remove-local")).toBeNull();
    findByTestId(el, "charter-roster-remove-smoke-self")!.onClick!();
    expect(onRemoveClick).toHaveBeenCalledWith("smoke-self", false);
    findByTestId(el, "charter-roster-remove-dxa347")!.onClick!();
    expect(onRemoveClick).toHaveBeenCalledWith("dxa347", true);
  });

  it("confirmingHost flips that host's remove button to the confirm label only", () => {
    const withHosts: RosterHost[] = [
      fixtureHost({ host: "dxa347", status: "online", agents: [] }),
      fixtureHost({ host: "smoke-self", status: "offline", agents: [] }),
    ];
    const html = renderToString(
      <VendorRosterCards hosts={withHosts} nodes={nodes} collapsed={new Set()} confirmingHost="dxa347" />,
    );
    // dxa347's button is armed…
    expect(html).toMatch(/data-testid="charter-roster-remove-dxa347">确定移除\?</);
    // …smoke-self's is untouched.
    expect(html).toMatch(/data-testid="charter-roster-remove-smoke-self">移除</);
  });

  /** One offline satellite whose last heartbeat was `hours` before NOW_MS. */
  const offlineFor = (hours: number): RosterHost[] => [
    fixtureHost({
      host: "smoke-self",
      hostname: "claude-dev-04",
      status: "offline",
      agents: [],
      last_heartbeat_unix: hoursAgo(hours),
    }),
  ];

  it("(TEAM-8) an offline host says HOW LONG it has been out of touch", () => {
    const html = renderToString(
      <VendorRosterCards hosts={offlineFor(3)} nodes={[]} collapsed={new Set()} nowMs={NOW_MS} />,
    );
    expect(html).toContain('data-testid="charter-roster-age-smoke-self"');
    expect(html).toContain("已离线 3 小时");
    // Recent enough → no cleanup suggestion, and the remove button stays plain.
    expect(html).not.toContain('data-testid="charter-roster-stale-smoke-self"');
    expect(html).not.toContain("charter-roster-remove warn");
  });

  it("(TEAM-8) past the stale threshold it reads in days and SUGGESTS cleanup (warn button)", () => {
    const html = renderToString(
      <VendorRosterCards
        hosts={offlineFor(24 * 8)}
        nodes={[]}
        collapsed={new Set()}
        nowMs={NOW_MS}
      />,
    );
    expect(html).toContain("已离线 8 天");
    expect(html).toContain('data-testid="charter-roster-stale-smoke-self"');
    expect(html).toContain("建议移除");
    expect(html).toContain("charter-roster-remove warn");
    // A suggestion only — the button still just calls back, nothing auto-runs.
    expect(html).toContain('data-testid="charter-roster-remove-smoke-self"');
  });

  it("(TEAM-8) no age for an ONLINE host, nor for an offline one the API sent no heartbeat for", () => {
    const onlineHtml = renderToString(
      <VendorRosterCards hosts={hosts} nodes={nodes} collapsed={new Set()} nowMs={NOW_MS} />,
    );
    expect(onlineHtml).not.toContain("charter-roster-age-"); // local is always online
    const noBeat = renderToString(
      <VendorRosterCards
        hosts={[fixtureHost({ host: "mystery", status: "offline", agents: [] })]}
        nodes={[]}
        collapsed={new Set()}
        nowMs={NOW_MS}
      />,
    );
    // Silence beats an invented age; removal is still offered.
    expect(noBeat).not.toContain("charter-roster-age-");
    expect(noBeat).toContain('data-testid="charter-roster-remove-mystery"');
    expect(noBeat).not.toContain("charter-roster-remove warn");
  });

  it("(TEAM-8) the age line is i18n'd, with the unit pluralized in en", () => {
    const one = renderToString(
      <VendorRosterCards
        hosts={offlineFor(1)}
        nodes={[]}
        collapsed={new Set()}
        lang="en"
        nowMs={NOW_MS}
      />,
    );
    expect(one).toContain("offline for 1 hour");
    expect(one).not.toContain("1 hours");
    const many = renderToString(
      <VendorRosterCards
        hosts={offlineFor(24 * 9)}
        nodes={[]}
        collapsed={new Set()}
        lang="en"
        nowMs={NOW_MS}
      />,
    );
    expect(many).toContain("offline for 9 days");
    expect(many).toContain("consider removing");
  });

  // TEAM-10 — "is this the current build?" on the version line. `latests` is
  // injected here exactly as `CharterPanel` prop-drills it from
  // `lib/vendorLatest.ts` (whose npm mapping decides which vendors can ever
  // have an entry: claude/codex/grok/opencode — kimi has no npm channel).
  it("(TEAM-10) an installed agent behind its published release says so", () => {
    const html = renderToString(
      <VendorRosterCards
        hosts={hosts}
        nodes={nodes}
        collapsed={new Set()}
        latests={{ claude: "2.1.40" }}
      />,
    );
    expect(html).toContain('data-testid="charter-roster-update-local-claude"');
    expect(html).toContain("↑ 2.1.40 可更新");
    // The installed version stays the headline — the hint only appends.
    expect(html).toContain("2.1.34");
    // codex has no entry in the map → its card says nothing about updates.
    expect(html).not.toContain('data-testid="charter-roster-update-local-codex"');
  });

  it("(TEAM-10) silence when the catalog has no answer, the version matches, or nothing is installed", () => {
    // (a) no map at all (the default) — no hint anywhere.
    expect(renderToString(<VendorRosterCards hosts={hosts} nodes={nodes} />)).not.toContain(
      "charter-roster-update-",
    );
    // (b) latest === installed.
    expect(
      renderToString(
        <VendorRosterCards hosts={hosts} nodes={nodes} latests={{ claude: "2.1.34" }} />,
      ),
    ).not.toContain("charter-roster-update-");
    // (c) not installed: a version-shaped catalog entry must not turn 未安装
    //     into an upgrade nag.
    const absent = [
      fixtureHost({
        agents: [fixtureAgent({ vendor: "grok", installed: false, version: null })],
      }),
    ];
    const absentHtml = renderToString(
      <VendorRosterCards hosts={absent} nodes={[]} latests={{ grok: "9.9.9" }} />,
    );
    expect(absentHtml).toContain("未安装");
    expect(absentHtml).not.toContain("charter-roster-update-");
    // (d) an unparseable installed version — no comparison, no claim.
    const murky = [fixtureHost({ agents: [fixtureAgent({ version: "unknown" })] })];
    expect(
      renderToString(<VendorRosterCards hosts={murky} nodes={[]} latests={{ claude: "2.1.40" }} />),
    ).not.toContain("charter-roster-update-");
  });

  it("(TEAM-10) the update hint speaks the shell language", () => {
    const en = renderToString(
      <VendorRosterCards hosts={hosts} nodes={nodes} lang="en" latests={{ claude: "2.1.40" }} />,
    );
    expect(en).toContain("↑ 2.1.40 available");
    expect(en).not.toContain("可更新");
  });

  it("empty roster renders nothing", () => {
    expect(renderToString(<VendorRosterCards hosts={[]} nodes={nodes} />)).toBe("");
  });

  // TEAM-7 — "what is this vendor doing?" → the caller's topology filter.
  it("(TEAM-7) a card hands its own vendor up on click, and on Enter like a tree row", () => {
    const onVendorPick = vi.fn();
    const el = VendorRosterCards({ hosts, nodes, collapsed: new Set(), onVendorPick });

    const codex = findByTestId(el, "charter-roster-card-local-codex")!;
    expect(codex.role).toBe("button");
    expect(codex.tabIndex).toBe(0);
    expect(codex.title).toBe("查看该 harness 的会话拓扑");
    codex.onClick!();
    expect(onVendorPick).toHaveBeenCalledWith("codex");

    // Keyboard parity with AgentsView's tree rows: Enter picks, nothing else.
    const onKeyDown = codex.onKeyDown as (event: { key: string }) => void;
    onKeyDown({ key: "a" });
    expect(onVendorPick).toHaveBeenCalledTimes(1);
    onKeyDown({ key: "Enter" });
    expect(onVendorPick).toHaveBeenCalledTimes(2);

    // Each card carries its OWN vendor (never the section's first one).
    findByTestId(el, "charter-roster-card-local-claude")!.onClick!();
    expect(onVendorPick).toHaveBeenLastCalledWith("claude");
  });

  it("(TEAM-7) without the callback the card stays pure display (no role, no pointer)", () => {
    const plain = renderToString(<VendorRosterCards hosts={hosts} nodes={nodes} />);
    expect(plain).toContain('data-testid="charter-roster-card-local-claude"');
    expect(plain).not.toContain('role="button"');
    expect(plain).not.toContain("charter-roster-card pickable");
    expect(plain).not.toContain("查看该 harness 的会话拓扑");
    expect(findByTestId(VendorRosterCards({ hosts, nodes }), "charter-roster-card-local-claude")!.onClick)
      .toBeUndefined();

    // With it, the hover/pointer affordance comes from the `pickable` modifier.
    const picky = renderToString(
      <VendorRosterCards hosts={hosts} nodes={nodes} onVendorPick={() => {}} />,
    );
    expect(picky).toContain("charter-roster-card pickable");
    expect(picky).toContain('role="button"');
    expect(picky).toContain('tabindex="0"');
  });

  it("(TEAM-7) the pick hint speaks the shell language", () => {
    const en = renderToString(
      <VendorRosterCards hosts={hosts} nodes={nodes} lang="en" onVendorPick={() => {}} />,
    );
    expect(en).toContain("View this vendor&#x27;s sessions in the topology");
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

/** Collect every react-router `Link` in a (hook-free) component's element
 *  tree, in render order — proves the CTA target + one-shot state payload. */
function collectLinks(
  el: unknown,
  out: { to: unknown; state: unknown }[] = [],
): { to: unknown; state: unknown }[] {
  if (el == null || typeof el !== "object") return out;
  if (Array.isArray(el)) {
    for (const child of el) collectLinks(child, out);
    return out;
  }
  const node = el as {
    type?: unknown;
    props?: { to?: unknown; state?: unknown; children?: unknown };
  };
  if (node.props) {
    if (node.type === Link) out.push({ to: node.props.to, state: node.props.state });
    collectLinks(node.props.children, out);
  }
  return out;
}

describe("PlaybookCards (编队起手 formations)", () => {
  it("renders one card per shared playbook: icon+name+description+lineup+起手", () => {
    const html = renderToString(
      <MemoryRouter>
        <PlaybookCards />
      </MemoryRouter>,
    );
    expect(html).toContain('data-testid="charter-playbooks"');
    expect(html).toContain("编队起手");
    for (const pb of PLAYBOOKS) {
      expect(html).toContain(`data-testid="playbook-${pb.id}"`);
      expect(html).toContain(`data-testid="playbook-launch-${pb.id}"`);
    }
    expect(html).toContain("总控-工班");
    expect(html).toContain("金字塔用工");
    // Lineup chips span all five harnesses across the deck.
    for (const vendor of ["claude", "codex", "grok", "kimi", "opencode"]) {
      expect(html).toContain(`data-vendor="${vendor}"`);
    }
    // The CTA is a real link into the Home launcher.
    expect(html).toContain('href="/"');
    // Honesty line sits under the cards: prefill only, orchestration
    // happens in-session via session_* — never a shipped prompt.
    expect(html).toContain('data-testid="playbook-honesty"');
    expect(html).toContain("session_*");
  });

  it("every 起手 CTA targets `/` with its own one-shot { playbook } state", () => {
    const links = collectLinks(PlaybookCards({}));
    expect(links).toHaveLength(PLAYBOOKS.length);
    links.forEach((link, i) => {
      expect(link.to).toBe("/");
      expect(link.state).toEqual({ playbook: PLAYBOOKS[i]!.id });
    });
  });

  it("speaks the shell language (en)", () => {
    const html = renderToString(
      <MemoryRouter>
        <PlaybookCards lang="en" />
      </MemoryRouter>,
    );
    expect(html).toContain("Formation playbooks");
    expect(html).toContain("Commander + crews");
    expect(html).toContain("Pyramid staffing");
    expect(html).toContain("orchestration happens inside the session");
  });
});

describe("CharterPanel (shell smoke)", () => {
  it("renders roster/playbooks/editor scaffolding + the standing honesty note", () => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
    const html = renderToString(
      <MemoryRouter>
        <CharterPanel nodes={[]} />
      </MemoryRouter>,
    );
    expect(html).toContain('data-testid="charter-panel"');
    expect(html).toContain('data-testid="charter-playbooks"');
    expect(html).toContain('data-testid="charter-honesty"');
    expect(html).toContain("MCP status");
    expect(html).toContain("分工宪章");
  });

  it("renders in English when lang='en'", () => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
    const html = renderToString(
      <MemoryRouter>
        <CharterPanel nodes={[]} lang="en" />
      </MemoryRouter>,
    );
    expect(html).toContain("Division-of-labor charter");
    expect(html).toContain("never injected");
  });
});
