// v0.8.9 Phase 4 — CostPill smoke test.
//
// No DOM env: renderToString the pill in its initial (pre-fetch) state. The
// status fetch + the cost/severity formatting are covered by statusApi.test.ts
// + marketplaceFormat.test.ts. We assert the stable data-testid (layout
// contract the shell relies on) + the dim placeholder before data arrives.

import { describe, expect, it, vi, afterEach, beforeEach } from "vitest";
import { renderToString } from "react-dom/server";
import { MemoryRouter } from "react-router-dom";
import CostPill, { CostPillButton } from "./CostPill";

const realFetch = globalThis.fetch;

function visibleText(html: string): string {
  return html.replace(/<!-- -->/g, "").replace(/<[^>]*>/g, "");
}

describe("CostPill initial render", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("renders the cost-pill slot with the dim placeholder before data loads", () => {
    const html = renderToString(
      <MemoryRouter>
        <CostPill />
      </MemoryRouter>,
    );
    expect(html).toContain('data-testid="cost-pill"');
    expect(html).toContain("今日");
    // Pre-fetch: em-dash placeholder, not a real number.
    expect(html).toContain("$—");
  });
});

describe("CostPillButton loaded states", () => {
  it("renders a loaded null-cap state without a fake NaN or placeholder cap", () => {
    const html = renderToString(
      <CostPillButton
        snap={{
          daemon_healthy: true,
          sessions_live: 1,
          sessions_idle: 0,
          cost_24h_usd: 1.25,
          cost_24h_by_vendor: { claude: 1.25 },
          budget_cap_24h: null,
        }}
        onOpenStatus={() => {}}
      />,
    );
    expect(html).toContain("$1.25");
    expect(html).not.toContain("$—");
    expect(html).not.toContain("NaN");
  });

  it("renders a loaded cap state with the configured cap", () => {
    const html = renderToString(
      <CostPillButton
        snap={{
          daemon_healthy: true,
          sessions_live: 1,
          sessions_idle: 0,
          cost_24h_usd: 2,
          cost_24h_by_vendor: { claude: 2 },
          budget_cap_24h: 10,
        }}
        onOpenStatus={() => {}}
      />,
    );
    expect(visibleText(html)).toContain("$2.00 / $10.00");
    expect(html).not.toContain("NaN");
  });
});
