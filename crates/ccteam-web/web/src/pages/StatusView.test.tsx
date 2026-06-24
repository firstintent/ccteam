// v0.8.9 Phase 4 — StatusView smoke tests.
//
// No DOM env: renderToString for the loading shell + the seeded StatusCards
// sub-component (the success state). The /api/v1/status fetch + mapping is
// covered by statusApi.test.ts; the cost/budget formatting by
// marketplaceFormat.test.ts.

import { describe, expect, it, vi, afterEach, beforeEach } from "vitest";
import { renderToString } from "react-dom/server";
import StatusView, { StatusCards } from "./StatusView";
import type { StatusSnapshot } from "../lib/statusApi";
import type { SessionView } from "../lib/sessionsApi";

const realFetch = globalThis.fetch;

function visibleText(html: string): string {
  return html.replace(/<!-- -->/g, "");
}

const RAIL: SessionView[] = [
  {
    sid: "s5",
    project: "ideas",
    role: "architect",
    vendor: "claude",
    permission_mode: "skip",
    current: true,
    status: "live",
    last_activity_seconds: 45,
  },
  {
    sid: "s7",
    project: "ideas",
    role: "cto",
    vendor: "codex",
    permission_mode: "skip",
    current: false,
    status: "live",
    last_activity_seconds: 3700,
  },
];

describe("StatusView initial render", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("renders the view + loading placeholder before the status fetch resolves", () => {
    const html = renderToString(<StatusView rail={[]} />);
    expect(html).toContain('data-testid="status-view"');
    expect(html).toContain('data-testid="status-loading"');
    expect(html).toContain("运维总览");
  });
});

describe("StatusCards (seeded success state)", () => {
  it("renders daemon-healthy + sessions + cost with vendor split", () => {
    const snap: StatusSnapshot = {
      daemon_healthy: true,
      sessions_live: 2,
      sessions_idle: 0,
      cost_24h_usd: 2.14,
      cost_24h_by_vendor: { claude: 1.62, codex: 0.52 },
      budget_cap_24h: 20,
    };
    const html = renderToString(<StatusCards status={snap} rail={RAIL} />);
    const text = visibleText(html);
    expect(html).toContain('data-testid="status-daemon"');
    expect(html).toContain("daemon healthy");
    expect(html).toContain('data-testid="status-sessions"');
    // SSR splits interpolated {N} with comment markers, so assert the static
    // bits around the counts rather than the whole "2 live · 0 idle" phrase.
    expect(html).toContain("会话");
    expect(html).toContain("live ·");
    expect(html).toContain("idle");
    expect(html).toContain("architect");
    expect(html).toContain("最近活动");
    expect(text).toContain("45 秒前");
    expect(text).toContain("1 小时前");
    expect(html).toContain('data-testid="status-cost"');
    expect(html).toContain("$2.14 / $20.00");
    expect(html).toContain("claude $1.62 · codex $0.52");
  });

  it("renders per-session cost from the fleet list, and '—' for an unpriced session", () => {
    // Determinism: s5 has a real priced cost ($1.25); s7 priced nothing
    // (cost_usd: null) → the cost cell must show "—", NOT $0.00.
    const snap: StatusSnapshot = {
      daemon_healthy: true,
      sessions_live: 2,
      sessions_idle: 0,
      cost_24h_usd: 1.25,
      cost_24h_by_vendor: { claude: 1.25 },
      budget_cap_24h: null,
      sessions: [
        { sid: "s5", project: "ideas", role: "architect", vendor: "claude", status: "live", cost_usd: 1.25 },
        { sid: "s7", project: "ideas", role: "cto", vendor: "codex", status: "live", cost_usd: null, unpriced_turns: 2 },
      ],
    };
    const html = renderToString(<StatusCards status={snap} rail={RAIL} />);
    // s5's real cost renders as a dollar figure.
    expect(html).toContain('data-testid="session-cost-s5"');
    expect(visibleText(html)).toContain("$1.25");
    // s7's null cost renders the em-dash placeholder, never "$0.00".
    const s7Cell = html.slice(html.indexOf('data-testid="session-cost-s7"'));
    expect(s7Cell).toContain("—");
    expect(s7Cell.slice(0, 120)).not.toContain("$0.00");
  });

  it("shows the daemon-down state when unhealthy", () => {
    const snap: StatusSnapshot = {
      daemon_healthy: false,
      sessions_live: 0,
      sessions_idle: 1,
      cost_24h_usd: 0,
      cost_24h_by_vendor: {},
      budget_cap_24h: null,
    };
    const html = renderToString(<StatusCards status={snap} rail={[]} />);
    expect(html).toContain("daemon down");
    expect(html).toContain("MCP sock unreachable");
    // No vendor cost → the empty-cost line.
    expect(html).toContain("本窗口暂无计费记录");
    // No sessions → the empty-sessions line.
    expect(html).toContain("没有活动会话");
  });

  it("surfaces a near-budget warning at the warn threshold", () => {
    const snap: StatusSnapshot = {
      daemon_healthy: true,
      sessions_live: 1,
      sessions_idle: 0,
      cost_24h_usd: 18, // 90% of 20 → warn
      cost_24h_by_vendor: { claude: 18 },
      budget_cap_24h: 20,
    };
    const html = renderToString(<StatusCards status={snap} rail={RAIL} />);
    expect(html).toContain('data-testid="status-budget-warn"');
    expect(html).toContain("接近 24h 预算");
  });

  it("surfaces an over-budget warning at/over the cap", () => {
    const snap: StatusSnapshot = {
      daemon_healthy: true,
      sessions_live: 1,
      sessions_idle: 0,
      cost_24h_usd: 21,
      cost_24h_by_vendor: { claude: 21 },
      budget_cap_24h: 20,
    };
    const html = renderToString(<StatusCards status={snap} rail={RAIL} />);
    expect(html).toContain('data-testid="status-budget-warn"');
    expect(html).toContain("已达/超 24h 预算");
  });

  it("renders each session's live/idle badge from its own s.status, not global daemon health", () => {
    // daemon is healthy, but the rail mixes a live + an idle session: the
    // per-row badge must follow s.status (P2 fix), so the idle row reads
    // "idle" even though the daemon is up.
    const snap: StatusSnapshot = {
      daemon_healthy: true,
      sessions_live: 1,
      sessions_idle: 1,
      cost_24h_usd: 0,
      cost_24h_by_vendor: {},
      budget_cap_24h: null,
    };
    const mixedRail: SessionView[] = [
      { ...RAIL[0], sid: "s5", status: "live" },
      { ...RAIL[1], sid: "s7", status: "idle" },
    ];
    const html = renderToString(<StatusCards status={snap} rail={mixedRail} />);
    // Both badge texts present — proves the badge is per-session, not derived
    // from the (healthy) global daemon flag (which would force every row live).
    expect(html).toContain(">live<");
    expect(html).toContain(">idle<");
    // The idle row gets the brand (idle) badge color, the live row the running
    // color — both color classes coexist in the same healthy-daemon card.
    expect(html).toContain("text-status-running");
    expect(html).toContain("text-brand-400");
  });

  it("renders file-backed stuck session status as a distinct badge", () => {
    const snap: StatusSnapshot = {
      daemon_healthy: true,
      sessions_live: 1,
      sessions_idle: 0,
      cost_24h_usd: 0,
      cost_24h_by_vendor: {},
      budget_cap_24h: null,
    };
    const html = renderToString(
      <StatusCards status={snap} rail={[{ ...RAIL[0], status: "stuck" }]} />,
    );
    expect(html).toContain("疑似卡");
    expect(html).toContain("text-status-error");
  });

  it("renders stale session status with the warning badge", () => {
    const snap: StatusSnapshot = {
      daemon_healthy: true,
      sessions_live: 1,
      sessions_idle: 0,
      cost_24h_usd: 0,
      cost_24h_by_vendor: {},
      budget_cap_24h: null,
    };
    const html = renderToString(
      <StatusCards status={snap} rail={[{ ...RAIL[0], status: "stale" }]} />,
    );
    expect(html).toContain("疑似慢");
    expect(html).toContain("text-status-waiting");
  });

  it("shows no budget warning when no cap is configured", () => {
    const snap: StatusSnapshot = {
      daemon_healthy: true,
      sessions_live: 1,
      sessions_idle: 0,
      cost_24h_usd: 999,
      cost_24h_by_vendor: { claude: 999 },
      budget_cap_24h: null,
    };
    const html = renderToString(<StatusCards status={snap} rail={RAIL} />);
    expect(html).not.toContain('data-testid="status-budget-warn"');
  });

  it("renders per-session cost from status.sessions joined to the rail by sid", () => {
    // v0.8.18 柱1 — the fleet cost column: each rail row shows its sid's cost
    // from status.sessions. A rail sid absent from status.sessions shows $0.00.
    const snap: StatusSnapshot = {
      daemon_healthy: true,
      sessions_live: 2,
      sessions_idle: 0,
      cost_24h_usd: 1.68,
      cost_24h_by_vendor: { claude: 1.23, codex: 0.45 },
      budget_cap_24h: null,
      sessions: [
        { sid: "s5", project: "ideas", role: "architect", vendor: "claude", status: "live", cost_usd: 1.23 },
        { sid: "s7", project: "ideas", role: "cto", vendor: "codex", status: "live", cost_usd: 0.45 },
      ],
    };
    const html = renderToString(<StatusCards status={snap} rail={RAIL} />);
    const text = visibleText(html);
    expect(html).toContain('data-testid="session-cost-s5"');
    expect(html).toContain('data-testid="session-cost-s7"');
    expect(text).toContain("$1.23");
    expect(text).toContain("$0.45");
  });

  it("shows $0.00 cost for a rail session with no cost row yet", () => {
    const snap: StatusSnapshot = {
      daemon_healthy: true,
      sessions_live: 1,
      sessions_idle: 0,
      cost_24h_usd: 0,
      cost_24h_by_vendor: {},
      budget_cap_24h: null,
      sessions: [],
    };
    const html = renderToString(<StatusCards status={snap} rail={[RAIL[0]]} />);
    expect(visibleText(html)).toContain("$0.00");
  });
});
