// @vitest-environment jsdom
// The two-layer step view, mounted for real: a folded activity row expands
// into its step list, and picking a step opens the inspector with the full
// record fetched by its `step_id` (a step not recorded yet reads as running).

import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { MemoryRouter } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

vi.mock("../lib/authedEventSource", () => ({
  createAuthedEventSource: () => ({
    close() {},
    addEventListener() {},
    removeEventListener() {},
    onmessage: null,
    onerror: null,
    onopen: null,
    readyState: 1,
  }),
}));

import SessionView from "./SessionView";
import { appendEvent, rowsKeyFor, type TranscriptRow } from "./chatTranscript";
import type { SessionEvent } from "../hooks/useSessionEvents";

// eslint-disable-next-line @typescript-eslint/no-explicit-any
(globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;

const activity = (a: {
  kind?: string;
  name?: string;
  summary: string;
  status: string;
  item_id: string;
}): SessionEvent => ({
  kind: "activity",
  content: a.summary,
  activity: { kind: "tool_call", name: "Bash", step_id: `n-1-${a.item_id}`, ...a },
});

describe("SessionView step view", () => {
  let host: HTMLDivElement;
  let root: Root;
  let fetched: string[];

  beforeEach(() => {
    fetched = [];
    let rows: TranscriptRow[] = [];
    rows = appendEvent(rows, activity({ summary: "Bash(ls)", status: "started", item_id: "t1" }));
    rows = appendEvent(rows, activity({ summary: "Bash(ls)", status: "completed", item_id: "t1" }));
    rows = appendEvent(rows, activity({ name: "Read", summary: "Read(a.rs)", status: "started", item_id: "t2" }));
    localStorage.setItem(rowsKeyFor("s9"), JSON.stringify(rows));
    globalThis.fetch = vi.fn((input: RequestInfo | URL) => {
      const url = String(input);
      if (url.includes("/steps/")) {
        fetched.push(url);
        if (!url.endsWith("/n-1-t1")) {
          return Promise.resolve(new Response("{}", { status: 404 }));
        }
        return Promise.resolve(
          new Response(
            JSON.stringify({
              step_id: "n-1-t1",
              ts: "2026-10-19T00:00:00Z",
              kind: "tool_call",
              name: "Bash",
              input: '{\n  "command": "ls"\n}',
              output: "a.txt\nb.txt",
              truncated: false,
            }),
            { status: 200, headers: { "Content-Type": "application/json" } },
          ),
        );
      }
      return new Promise<Response>(() => {});
    }) as typeof fetch;
    host = document.createElement("div");
    document.body.appendChild(host);
    root = createRoot(host);
  });

  afterEach(() => {
    act(() => root.unmount());
    host.remove();
    localStorage.clear();
    vi.restoreAllMocks();
  });

  const q = (id: string) => host.querySelector(`[data-testid="${id}"]`);
  const all = (id: string) => [...host.querySelectorAll<HTMLElement>(`[data-testid="${id}"]`)];

  it("expands the fold, then opens a finished step in full", async () => {
    await act(async () => {
      root.render(
        <MemoryRouter>
          <SessionView sid="s9" session={null} />
        </MemoryRouter>,
      );
    });
    expect(q("fold-steps")).toBeNull();
    await act(async () => (q("fold-toggle") as HTMLElement).click());
    const steps = all("fold-step");
    expect(steps.map((s) => s.textContent)).toEqual(["🔧Bash(ls)✓", "📖Read(a.rs)…"]);

    // The running step: not recorded yet, so the panel says it still runs.
    await act(async () => steps[1]!.click());
    expect(q("step-inspector")).not.toBeNull();
    expect(fetched).toEqual(["/api/v1/sessions/s9/steps/n-1-t2"]);
    expect(q("step-state")?.textContent).toContain("仍在运行");

    // The finished step: its record, fetched by step id.
    await act(async () => steps[0]!.click());
    expect(fetched.at(-1)).toBe("/api/v1/sessions/s9/steps/n-1-t1");
    expect(q("step-input")?.textContent).toContain('"command": "ls"');
    expect(q("step-output")?.textContent).toBe("a.txt\nb.txt");

    await act(async () => (q("step-inspector-close") as HTMLElement).click());
    expect(q("step-inspector")).toBeNull();
  });
});
