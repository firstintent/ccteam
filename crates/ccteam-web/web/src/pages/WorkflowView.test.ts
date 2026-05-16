// V0.4.0 F68 — unit tests for the workflow view event reducer.
//
// `applyEventToSummary` is the pure delta-merge used by `WorkflowView`
// to patch the locally-shadowed `WorkflowSummary` with incoming SSE
// progress events. We exercise it directly (cheap, framework-free) so
// the live-update path stays correct even when refactored. The SPA
// render layer is covered by the rust-side integration tests in
// `crates/ccteam-web/tests/api_v1_workflow_test.rs` (DTO shape +
// regression guards for the dropped phase fields).
//
// V0.4.6 F90: extends with four panel-side unit tests for the pure
// helpers in `lib/workflowPanels.ts` + the `classify` helper from
// `EventsTimelinePanel`. The components themselves aren't rendered
// under vitest (no DOM testing-library wired in this workspace); the
// rust-side integration tests in
// `crates/ccteam-web/tests/api_v1_workflow_panels_test.rs` cover the
// fetch surface end-to-end.

import { describe, it, expect } from "vitest";
import { applyEventToSummary } from "./WorkflowView";
import {
  ageLabel,
  basename,
  sparklinePoints,
  type CostHistoryBucket,
} from "../lib/workflowPanels";
import { classify } from "../components/EventsTimelinePanel";
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

// ---------------- V0.4.6 F90 panel helper tests ----------------

describe("ageLabel (F90)", () => {
  it("renders sub-minute durations in seconds", () => {
    expect(ageLabel(0)).toBe("0s");
    expect(ageLabel(45)).toBe("45s");
  });
  it("rolls up minutes / hours / days", () => {
    expect(ageLabel(60)).toBe("1m");
    expect(ageLabel(3599)).toBe("59m");
    expect(ageLabel(3600)).toBe("1h");
    expect(ageLabel(60 * 60 * 24)).toBe("1d");
  });
  it("returns em-dash for null / nonsense", () => {
    expect(ageLabel(null)).toBe("—");
    expect(ageLabel(undefined)).toBe("—");
    expect(ageLabel(-5)).toBe("—");
    expect(ageLabel(Number.NaN)).toBe("—");
  });
});

describe("basename (F90)", () => {
  it("returns last component of a unix path", () => {
    expect(basename("/tmp/team-d")).toBe("team-d");
    expect(basename("/home/rob/projects/dev-foo")).toBe("dev-foo");
  });
  it("handles trailing slashes + edge cases", () => {
    expect(basename("/tmp/team-d/")).toBe("team-d");
    expect(basename("foo")).toBe("foo");
    expect(basename(null)).toBe("—");
    expect(basename("")).toBe("—");
  });
});

describe("sparklinePoints (F90)", () => {
  it("renders a polyline string with X/Y normalized to viewport", () => {
    const buckets: CostHistoryBucket[] = [
      { hour: "2026-05-15T00:00:00Z", cost_usd: 0 },
      { hour: "2026-05-15T01:00:00Z", cost_usd: 0.5 },
      { hour: "2026-05-15T02:00:00Z", cost_usd: 1.0 },
    ];
    const out = sparklinePoints(buckets, 100, 50);
    // 3 buckets → 3 space-separated points
    const points = out.split(" ");
    expect(points).toHaveLength(3);
    // First point at x=0; last at x=width.
    expect(points[0].startsWith("0.00,")).toBe(true);
    expect(points[2].startsWith("100.00,")).toBe(true);
    // Max cost mapped to top (y=0); zero cost mapped to bottom (y=h).
    expect(points[0].endsWith(",50.00")).toBe(true);
    expect(points[2].endsWith(",0.00")).toBe(true);
  });

  it("returns flat baseline for all-zero series", () => {
    const buckets: CostHistoryBucket[] = [
      { hour: "x", cost_usd: 0 },
      { hour: "y", cost_usd: 0 },
    ];
    const out = sparklinePoints(buckets, 100, 30);
    const points = out.split(" ");
    expect(points).toHaveLength(2);
    // baseline = h - 1 = 29
    expect(points[0].endsWith(",29.00")).toBe(true);
    expect(points[1].endsWith(",29.00")).toBe(true);
  });

  it("returns empty string for empty input", () => {
    expect(sparklinePoints([], 100, 30)).toBe("");
  });
});

describe("classify (F90 EventsTimelinePanel)", () => {
  function ev(extra: Partial<ProgressEvent> & { event: string }): ProgressEvent {
    return {
      ts: "2026-05-15T10:00:00Z",
      event: extra.event,
      detail: "",
      ...extra,
    };
  }
  it("agent_done completed/stopped → ok", () => {
    expect(classify(ev({ event: "agent_done", status: "completed" }))).toBe("ok");
    expect(classify(ev({ event: "agent_done", status: "stopped" }))).toBe("ok");
  });
  it("agent_done error → error", () => {
    expect(classify(ev({ event: "agent_done", status: "error" }))).toBe("error");
    expect(classify(ev({ event: "agent_done", status: "crashed" }))).toBe("error");
  });
  it("escalation → error", () => {
    expect(classify(ev({ event: "escalation" }))).toBe("error");
  });
  it("gate_triggered / budget_exceeded → warn", () => {
    expect(classify(ev({ event: "gate_triggered" }))).toBe("warn");
    expect(classify(ev({ event: "budget_exceeded" }))).toBe("warn");
  });
  it("workflow_done shutdown/completed → ok; others → warn", () => {
    expect(classify(ev({ event: "workflow_done", reason: "shutdown" }))).toBe("ok");
    expect(classify(ev({ event: "workflow_done", reason: "disabled" }))).toBe("warn");
  });
  it("workflow_start / agent_spawn / unknown → info", () => {
    expect(classify(ev({ event: "workflow_start" }))).toBe("info");
    expect(classify(ev({ event: "agent_spawn" }))).toBe("info");
    expect(classify(ev({ event: "wibble" }))).toBe("info");
  });
});
