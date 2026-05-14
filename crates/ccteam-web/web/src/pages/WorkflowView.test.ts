// V0.4.0 F68 — unit tests for the workflow view event reducer.
//
// `applyEventToSummary` is the pure delta-merge used by `WorkflowView`
// to patch the locally-shadowed `WorkflowSummary` with incoming SSE
// progress events. We exercise it directly (cheap, framework-free) so
// the live-update path stays correct even when refactored. The SPA
// render layer is covered by the rust-side integration tests in
// `crates/ccteam-web/tests/api_v1_workflow_test.rs` (DTO shape +
// regression guards for the dropped phase fields).

import { describe, it, expect } from "vitest";
import { applyEventToSummary } from "./WorkflowView";
import type {
  AgentStatus,
  WorkflowSummary,
} from "../lib/detailApi";
import type { ProgressEvent } from "../hooks/useProgressStream";

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

function baseSummary(): WorkflowSummary {
  return {
    workflow_name: "dev-team",
    agents: [agent("planner"), agent("coder")],
    artifact_counts: { "artifacts/plan": 1, "artifacts/code": 2 },
    total_cost_usd: 0,
    escalation_count: 0,
    gate_states: { "review-gate": "waiting" },
  };
}

function ev(extra: Partial<ProgressEvent> & { event: string }): ProgressEvent {
  return {
    ts: "2026-05-14T10:00:00Z",
    event: extra.event,
    detail: "",
    ...extra,
  };
}

describe("applyEventToSummary", () => {
  it("agent_spawn bumps running_count and sets last_session_status to running", () => {
    const before = baseSummary();
    const after = applyEventToSummary(
      before,
      ev({ event: "agent_spawn", role: "planner" }),
    );
    const planner = after.agents.find((a) => a.role === "planner")!;
    expect(planner.running_count).toBe(1);
    expect(planner.last_session_status).toEqual({ status: "running" });
    // Untouched fields stay equal-by-value
    expect(after.escalation_count).toBe(0);
    expect(after.gate_states).toEqual(before.gate_states);
  });

  it("agent_done flips to done + adds cost", () => {
    const before = baseSummary();
    before.agents[0].running_count = 1;
    before.agents[0].last_session_status = { status: "running" };
    const after = applyEventToSummary(
      before,
      ev({
        event: "agent_done",
        role: "planner",
        status: "completed",
        cost_usd: 0.25,
      }),
    );
    const planner = after.agents.find((a) => a.role === "planner")!;
    expect(planner.running_count).toBe(0);
    expect(planner.last_session_status).toEqual({
      status: "done",
      cost_usd: 0.25,
    });
    expect(planner.total_cost_usd).toBeCloseTo(0.25);
    expect(after.total_cost_usd).toBeCloseTo(0.25);
  });

  it("agent_done with non-completed status flips to errored", () => {
    const before = baseSummary();
    before.agents[1].running_count = 1;
    const after = applyEventToSummary(
      before,
      ev({
        event: "agent_done",
        role: "coder",
        status: "error",
        cost_usd: 0.1,
      }),
    );
    const coder = after.agents.find((a) => a.role === "coder")!;
    expect(coder.last_session_status).toEqual({
      status: "errored",
      cost_usd: 0.1,
    });
    expect(coder.running_count).toBe(0);
  });

  it("gate_triggered flips the matching gate to fired", () => {
    const before = baseSummary();
    const after = applyEventToSummary(
      before,
      ev({ event: "gate_triggered", role: "review-gate" }),
    );
    expect(after.gate_states["review-gate"]).toBe("fired");
  });

  it("escalation bumps the escalation_count", () => {
    const before = baseSummary();
    const after = applyEventToSummary(
      before,
      ev({ event: "escalation", role: "planner" }),
    );
    expect(after.escalation_count).toBe(1);
  });

  it("ignores events with no role", () => {
    const before = baseSummary();
    const after = applyEventToSummary(
      before,
      ev({ event: "agent_spawn" }),
    );
    expect(after).toBe(before);
  });

  it("ignores unrelated event kinds", () => {
    const before = baseSummary();
    const after = applyEventToSummary(
      before,
      ev({ event: "PostToolUse", role: "planner" }),
    );
    expect(after).toBe(before);
  });

  it("synthesizes a card for an orphan role (not in workflow.yaml)", () => {
    const before = baseSummary();
    const after = applyEventToSummary(
      before,
      ev({ event: "agent_spawn", role: "stray" }),
    );
    const stray = after.agents.find((a) => a.role === "stray");
    expect(stray).toBeDefined();
    expect(stray!.running_count).toBe(1);
  });
});
