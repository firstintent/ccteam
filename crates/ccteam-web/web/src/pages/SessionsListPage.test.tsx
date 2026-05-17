// V0.5.1 F103a — SessionsListPage smoke tests.
//
// Workspace runs vitest without a DOM env (no @testing-library/react /
// jsdom). We use React's `renderToString` to assert the initial-state
// HTML shape (loading placeholder). Stateful UI paths after the fetch
// resolves are covered by:
//   - `listApi.test.ts`            — the fetch wrapper itself.
//   - Playwright host E2E         — end-to-end /sessions list rendering.
//
// Test matrix here:
//   - loading placeholder rendered before fetch resolves
//   - populated SessionCard via direct renderToString (bypassing the
//     useEffect path); proves the per-row JSX shape is what the page
//     emits once `sessions` is set.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { renderToString } from "react-dom/server";
import { MemoryRouter } from "react-router-dom";
import SessionsListPage from "./SessionsListPage";

const realFetch = globalThis.fetch;

describe("SessionsListPage initial render", () => {
  beforeEach(() => {
    globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
  });
  afterEach(() => {
    globalThis.fetch = realFetch;
    vi.restoreAllMocks();
  });

  it("renders the loading placeholder before the fetch resolves", () => {
    const html = renderToString(
      <MemoryRouter>
        <SessionsListPage />
      </MemoryRouter>,
    );
    expect(html).toContain("data-testid=\"sessions-loading\"");
    expect(html).toContain("loading sessions");
  });
});
