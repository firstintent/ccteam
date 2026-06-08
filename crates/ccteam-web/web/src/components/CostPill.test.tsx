// v0.8.9 Phase 4 — CostPill smoke test.
//
// No DOM env: renderToString the pill in its initial (pre-fetch) state. The
// status fetch + the cost/severity formatting are covered by statusApi.test.ts
// + marketplaceFormat.test.ts. We assert the stable data-testid (layout
// contract the shell relies on) + the dim placeholder before data arrives.

import { describe, expect, it, vi, afterEach, beforeEach } from "vitest";
import { renderToString } from "react-dom/server";
import { MemoryRouter } from "react-router-dom";
import CostPill from "./CostPill";

const realFetch = globalThis.fetch;

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
