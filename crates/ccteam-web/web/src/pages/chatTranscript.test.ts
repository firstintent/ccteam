// v0.8.7 W4 (DD.1) — per-sid transcript keying tests.
//
// THE invariant under test: two gateway sessions (`s1`, `s2`) never share a
// transcript buffer (the v0.8.3 flat-localStorage bug). Pure module → runs
// under node env (no jsdom) with an injected in-memory store.

import { describe, expect, it } from "vitest";

import {
  appendEvent,
  appendRow,
  eventToRow,
  foldActivity,
  emptyFold,
  historyToRows,
  loadRows,
  renderFold,
  rowsKeyFor,
  saveRows,
  ROWS_CAP,
  type TranscriptRow,
} from "./chatTranscript";
import type { SessionActivity, SessionEvent } from "../hooks/useSessionEvents";
import type { SessionHistoryEvent } from "../lib/sessionsApi";

/** Minimal in-memory Storage for node-env tests. */
function memStore(): Storage {
  const map = new Map<string, string>();
  return {
    get length() {
      return map.size;
    },
    clear: () => map.clear(),
    getItem: (k: string) => map.get(k) ?? null,
    key: (i: number) => Array.from(map.keys())[i] ?? null,
    removeItem: (k: string) => map.delete(k),
    setItem: (k: string, v: string) => void map.set(k, v),
  };
}

const row = (id: string, content: string): TranscriptRow => ({
  id,
  kind: "assistant",
  content,
});

describe("chatTranscript per-sid keying", () => {
  it("derives a distinct localStorage key per sid (no shared flat buffer)", () => {
    expect(rowsKeyFor("s1")).not.toBe(rowsKeyFor("s2"));
    // and it is NOT the old flat key that mixed sessions.
    expect(rowsKeyFor("s1")).not.toBe("ccteam.chat.rows.v1");
    expect(rowsKeyFor("s1")).toContain("s1");
  });

  it("two sids do not mix: save/load are isolated", () => {
    const store = memStore();
    saveRows("s1", [row("a", "hello from s1")], store);
    saveRows("s2", [row("b", "hello from s2")], store);

    const s1 = loadRows("s1", store);
    const s2 = loadRows("s2", store);

    expect(s1).toHaveLength(1);
    expect(s2).toHaveLength(1);
    expect(s1[0].content).toBe("hello from s1");
    expect(s2[0].content).toBe("hello from s2");
    // s1's buffer never contains s2's row and vice versa.
    expect(s1.some((r) => r.content.includes("s2"))).toBe(false);
    expect(s2.some((r) => r.content.includes("s1"))).toBe(false);
  });

  it("loadRows returns [] for an unknown sid (clean switch, nothing stale)", () => {
    const store = memStore();
    saveRows("s1", [row("a", "x")], store);
    expect(loadRows("s9", store)).toEqual([]);
  });

  it("loadRows tolerates garbage / missing storage", () => {
    const store = memStore();
    store.setItem(rowsKeyFor("s1"), "not-json");
    expect(loadRows("s1", store)).toEqual([]);
    // no store at all (node default) → []
    expect(loadRows("s1", undefined)).toEqual([]);
  });

  it("appendRow caps the ring buffer at ROWS_CAP (oldest drop)", () => {
    let rows: TranscriptRow[] = [];
    for (let i = 0; i < ROWS_CAP + 25; i++) {
      rows = appendRow(rows, row(`r${i}`, String(i)));
    }
    expect(rows).toHaveLength(ROWS_CAP);
    // oldest (r0..) dropped; newest kept.
    expect(rows[rows.length - 1].content).toBe(String(ROWS_CAP + 24));
    expect(rows[0].content).toBe(String(25));
  });
});

describe("chatTranscript eventToRow", () => {
  it("does not render session lifecycle frames as conversation content", () => {
    expect(
      eventToRow(ev({ kind: "session_lifecycle", content: "session evicted: s4" })),
    ).toBeNull();
  });

  const ev = (e: Partial<SessionEvent>): SessionEvent => ({
    kind: "answer",
    content: "",
    ...e,
  });

  it("maps an answer with content to an assistant bubble", () => {
    const r = eventToRow(ev({ kind: "answer", content: "hi there", id: "e1" }));
    expect(r).not.toBeNull();
    expect(r!.kind).toBe("assistant");
    expect(r!.content).toBe("hi there");
    expect(r!.id).toBe("e1");
  });

  it("maps an event with options to an approval row carrying token + ids (R-H1)", () => {
    const r = eventToRow(
      ev({
        kind: "answer",
        content: "session s2 wants to run rm -rf",
        options: [
          { label: "✅ Approve", id: "allow" },
          { label: "⛔ Deny", id: "deny" },
        ],
        token: "pcafef00d",
      }),
    );
    expect(r).not.toBeNull();
    expect(r!.kind).toBe("approval");
    expect(r!.options).toEqual([
      { label: "✅ Approve", id: "allow" },
      { label: "⛔ Deny", id: "deny" },
    ]);
    // The token rides onto the row so the resolve POST can carry it (R-H1).
    expect(r!.token).toBe("pcafef00d");
    expect(r!.content).toContain("wants to run");
  });

  it("maps an activity event to a compact activity row carrying the payload (v0.8.19)", () => {
    const r = eventToRow(
      ev({
        kind: "activity",
        content: "Bash(ls -la)",
        id: "act1",
        activity: {
          kind: "tool_call",
          name: "Bash",
          summary: "Bash(ls -la)",
          status: "started",
          item_id: "t1",
        },
      }),
    );
    expect(r).not.toBeNull();
    expect(r!.kind).toBe("activity");
    expect(r!.content).toBe("Bash(ls -la)");
    expect(r!.id).toBe("act1");
    // The structured activity rides onto the row (so SessionView picks an icon).
    expect(r!.activity).toMatchObject({ kind: "tool_call", name: "Bash" });
  });

  it("falls back to the event content when an activity frame has no summary", () => {
    const r = eventToRow(ev({ kind: "activity", content: "$ cargo build" }));
    expect(r).not.toBeNull();
    expect(r!.kind).toBe("activity");
    expect(r!.content).toBe("$ cargo build");
  });

  it("drops an activity frame with nothing to show", () => {
    expect(eventToRow(ev({ kind: "activity", content: "" }))).toBeNull();
  });

  it("drops empty non-final progress (status churn is noise)", () => {
    expect(eventToRow(ev({ kind: "progress", content: "" }))).toBeNull();
  });

  it("surfaces a finalizing progress with text as a system note", () => {
    const r = eventToRow(ev({ kind: "progress", content: "turn done", done: true }));
    expect(r).not.toBeNull();
    expect(r!.kind).toBe("system");
  });
});

describe("chatTranscript activity fold (v0.8.21 — mirrors IM ProgressFold)", () => {
  const act = (a: Partial<SessionActivity>): SessionActivity => ({
    kind: "tool_call",
    name: "Bash",
    summary: "",
    status: "started",
    item_id: "",
    ...a,
  });
  const actEv = (a: Partial<SessionActivity>, id?: string): SessionEvent => ({
    kind: "activity",
    content: "",
    id,
    activity: act(a),
  });

  it("folds a run of tool steps into ONE counter row by category", () => {
    let rows: TranscriptRow[] = [];
    for (let i = 0; i < 40; i++)
      rows = appendEvent(rows, actEv({ name: "Bash", item_id: `b${i}` }));
    for (let i = 0; i < 15; i++)
      rows = appendEvent(rows, actEv({ name: "Read", item_id: `r${i}` }));
    for (let i = 0; i < 16; i++)
      rows = appendEvent(rows, actEv({ name: "Edit", item_id: `e${i}` }));
    // 71 activity frames → exactly ONE folded row.
    expect(rows).toHaveLength(1);
    expect(rows[0].kind).toBe("activity");
    expect(rows[0].content).toBe("⏳ working… · 🔧 bash ×40 · 📖 read ×15 · ✏️ edit ×16");
  });

  it("counts each tool ONCE across its start↔complete pair (dedup by item_id)", () => {
    let rows: TranscriptRow[] = [];
    rows = appendEvent(rows, actEv({ name: "Bash", item_id: "t1", status: "started" }));
    rows = appendEvent(rows, actEv({ name: "Bash", item_id: "t1", status: "completed" }));
    expect(rows).toHaveLength(1);
    expect(rows[0].content).toBe("⏳ working… · 🔧 bash ×1");
  });

  it("folds command_exec/file_change/web_search into bash/edit/web buckets", () => {
    let rows: TranscriptRow[] = [];
    rows = appendEvent(rows, actEv({ kind: "command_exec", name: "bash", item_id: "c1" }));
    rows = appendEvent(rows, actEv({ kind: "file_change", name: "modified", item_id: "f1" }));
    rows = appendEvent(rows, actEv({ kind: "web_search", name: "web", item_id: "w1" }));
    expect(rows[0].content).toBe("⏳ working… · 🔧 bash ×1 · ✏️ edit ×1 · 🔎 web ×1");
  });

  it("folds an unknown tool under the wrench keeping its lowercased name", () => {
    let rows: TranscriptRow[] = [];
    rows = appendEvent(rows, actEv({ name: "Agent", item_id: "a1" }));
    expect(rows[0].content).toBe("⏳ working… · 🔧 agent ×1");
  });

  it("renders 💭 thinking… when only reasoning has happened (no tool bucket)", () => {
    let rows: TranscriptRow[] = [];
    rows = appendEvent(rows, actEv({ kind: "thinking", name: "", item_id: "rz" }));
    rows = appendEvent(rows, actEv({ kind: "thinking", name: "", item_id: "rz" })); // same item → no-op
    expect(rows).toHaveLength(1);
    expect(rows[0].content).toBe("💭 thinking…");
    // a tool after thinking flips the head to ⏳ working… (thinking not counted).
    rows = appendEvent(rows, actEv({ name: "Bash", item_id: "b1" }));
    expect(rows[0].content).toBe("⏳ working… · 🔧 bash ×1");
  });

  it("a non-activity event closes the fold — the next activity starts a fresh one", () => {
    let rows: TranscriptRow[] = [];
    rows = appendEvent(rows, actEv({ name: "Bash", item_id: "b1" }));
    rows = appendEvent(rows, { kind: "answer", content: "done", id: "a1" });
    rows = appendEvent(rows, actEv({ name: "Read", item_id: "r1" }));
    expect(rows.map((r) => r.kind)).toEqual(["activity", "assistant", "activity"]);
    expect(rows[0].content).toBe("⏳ working… · 🔧 bash ×1");
    expect(rows[2].content).toBe("⏳ working… · 📖 read ×1");
  });

  it("a bare activity frame (no payload) falls back to a single content row", () => {
    const rows = appendEvent([], { kind: "activity", content: "$ cargo build" });
    expect(rows).toHaveLength(1);
    expect(rows[0].kind).toBe("activity");
    expect(rows[0].content).toBe("$ cargo build");
    expect(rows[0].fold).toBeUndefined();
  });

  it("foldActivity/renderFold are pure (no mutation of prev)", () => {
    const a = emptyFold();
    const b = foldActivity(a, act({ name: "Bash", item_id: "x" }));
    expect(a.buckets).toHaveLength(0); // prev untouched
    expect(renderFold(b)).toBe("⏳ working… · 🔧 bash ×1");
  });
});

describe("chatTranscript historyToRows", () => {
  it("expands mirrored turns into user+assistant rows", () => {
    const events: SessionHistoryEvent[] = [
      { turn_id: "t1", ts: "2026-06-06T00:00:00Z", role: "cto", user: "hi", assistant: "hello" },
      { turn_id: "t2", ts: "2026-06-06T00:01:00Z", role: "cto", user: "", assistant: "just a reply" },
    ];
    const rows = historyToRows(events);
    expect(rows).toHaveLength(3);
    expect(rows[0]).toMatchObject({ kind: "user", content: "hi" });
    expect(rows[1]).toMatchObject({ kind: "assistant", content: "hello" });
    expect(rows[2]).toMatchObject({ kind: "assistant", content: "just a reply" });
  });

  it("renders an attachment-only mirrored turn after reload", () => {
    const events: SessionHistoryEvent[] = [
      {
        turn_id: "t-file",
        ts: "2026-08-02T00:00:00Z",
        role: "cto",
        user: "",
        assistant: "",
        attachments: [
          { id: "1780000000000-chart.png", name: "chart.png", kind: "image", size: 42 },
        ],
      },
    ];
    expect(historyToRows(events)).toEqual([
      {
        id: "t-file-a",
        kind: "assistant",
        content: "",
        attachments: [
          { id: "1780000000000-chart.png", name: "chart.png", kind: "image", size: 42 },
        ],
      },
    ]);
  });
});
