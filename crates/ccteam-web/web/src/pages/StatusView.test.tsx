import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { renderToString } from "react-dom/server";
import StatusView, { StatusCards } from "./StatusView";
import type { StatusSnapshot } from "../lib/statusApi";

const realFetch = globalThis.fetch;
const SNAP: StatusSnapshot = {
  daemon_healthy: true,
  sessions_live: 2,
  sessions_idle: 1,
  cost_24h_usd: 2.14,
  cost_24h_by_vendor: { claude: 1.62, codex: 0.52 },
  budget_cap_24h: 20,
};

describe("StatusView initial render", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("renders the loading shell", () => {
    const html = renderToString(<StatusView />);
    expect(html).toContain('data-testid="status-view"');
    expect(html).toContain('data-testid="status-loading"');
  });
});

describe("StatusCards", () => {
  it("keeps daemon, session, and cost stat tiles but removes the fleet table", () => {
    const html = renderToString(<StatusCards status={SNAP} />);
    expect(html).toContain('data-testid="status-daemon"');
    expect(html).toContain('data-testid="status-session-stat"');
    expect(html).toContain('data-testid="status-cost"');
    expect(html).not.toContain('data-testid="status-sessions"');
    expect(html).not.toContain('data-testid="fleet-table"');
  });

  it("shows aggregate live/idle counts and vendor cost split", () => {
    const html = renderToString(<StatusCards status={SNAP} />);
    expect(html).toContain("live ·");
    expect(html).toContain("idle");
    expect(html).toContain("$2.14 / $20.00");
    expect(html).toContain("claude $1.62 · codex $0.52");
  });

  it("shows daemon-down and the empty cost line", () => {
    const html = renderToString(<StatusCards status={{ ...SNAP, daemon_healthy: false, cost_24h_usd: 0, cost_24h_by_vendor: {}, budget_cap_24h: null }} />);
    expect(html).toContain("daemon down");
    expect(html).toContain("MCP sock unreachable");
    expect(html).toContain("本窗口暂无计费记录");
  });

  it("retains budget warnings", () => {
    const html = renderToString(<StatusCards status={{ ...SNAP, cost_24h_usd: 21 }} />);
    expect(html).toContain('data-testid="status-budget-warn"');
    expect(html).toContain("已达/超 24h 预算");
  });
});
