// V0.5.1 F103b — WorkflowView SPA render tests for the running-badge.
//
// Workspace runs vitest without a DOM env (no jsdom), so we rely on
// `renderToString` to assert the initial-render HTML. The auto-expand
// logic fires after `fetchActiveSessions` resolves, which is past the
// renderToString boundary — coverage for that path lives in:
//   - the host Playwright E2E suite (workflow `/p/<slug>` deep-link)
//   - and the visual symptom acceptance in PRD F103b §3
//
// The pure event reducer tests live in the sibling `WorkflowView.test.ts`;
// this file isolates the JSX path.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { renderToString } from "react-dom/server";
import { MemoryRouter } from "react-router-dom";
import WorkflowView from "./WorkflowView";
import type { AgentStatus, WorkflowSummary } from "../lib/detailApi";

const realFetch = globalThis.fetch;
const realEventSource = globalThis.EventSource;

function agent(role: string, overrides: Partial<AgentStatus> = {}): AgentStatus {
  return {
    role,
    running_count: 0,
    queued_count: 0,
    total_cost_usd: 0,
    last_session_status: null,
    ...overrides,
  };
}

function summary(agents: AgentStatus[]): WorkflowSummary {
  return {
    workflow_name: "dex-ui",
    agents,
    artifact_counts: {},
    total_cost_usd: 0,
    escalation_count: 0,
    gate_states: {},
  };
}

beforeEach(() => {
  globalThis.fetch = vi.fn().mockReturnValue(new Promise(() => {}));
  class FakeEventSource {
    onopen: ((e: Event) => void) | null = null;
    onerror: ((e: Event) => void) | null = null;
    onmessage: ((e: MessageEvent) => void) | null = null;
    addEventListener(): void {}
    removeEventListener(): void {}
    close(): void {}
  }
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  (globalThis as any).EventSource = FakeEventSource;
});
afterEach(() => {
  globalThis.fetch = realFetch;
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  (globalThis as any).EventSource = realEventSource;
  vi.restoreAllMocks();
});

describe("WorkflowView running badge (F103b)", () => {
  it("renders 'N running' badge for cards with running_count > 0", () => {
    const html = renderToString(
      <MemoryRouter>
        <WorkflowView
          slug="dex-ui"
          summary={summary([
            agent("planner", { running_count: 1 }),
            agent("coder", { running_count: 0 }),
          ])}
        />
      </MemoryRouter>,
    );
    // React server-renders dynamic numeric children with sentinel
    // comments (`>1<!-- --> running<`). Use a regex to bridge that.
    expect(html).toContain("data-testid=\"running-badge-planner\"");
    expect(html).toMatch(/data-testid="running-badge-planner"[\s\S]*?running</);
    // Coder card (running_count=0) has no badge.
    expect(html).not.toContain("data-testid=\"running-badge-coder\"");
  });

  it("omits the badge for all-idle workflows", () => {
    const html = renderToString(
      <MemoryRouter>
        <WorkflowView
          slug="dex-ui"
          summary={summary([agent("planner"), agent("coder")])}
        />
      </MemoryRouter>,
    );
    expect(html).not.toContain("data-testid=\"running-badge-");
  });

  it("renders multi-running count when running_count > 1", () => {
    const html = renderToString(
      <MemoryRouter>
        <WorkflowView
          slug="dex-ui"
          summary={summary([agent("planner", { running_count: 3 })])}
        />
      </MemoryRouter>,
    );
    expect(html).toContain("data-testid=\"running-badge-planner\"");
    expect(html).toMatch(/running-badge-planner[\s\S]*?>3</);
  });
});
