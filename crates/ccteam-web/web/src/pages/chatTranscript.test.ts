// v0.8.7 W4 (DD.1) — per-sid transcript keying tests.
//
// THE invariant under test: two gateway sessions (`s1`, `s2`) never share a
// transcript buffer (the v0.8.3 flat-localStorage bug). Pure module → runs
// under node env (no jsdom) with an injected in-memory store.

import { describe, expect, it } from "vitest";

import {
  appendRow,
  eventToRow,
  historyToRows,
  loadRows,
  rowsKeyFor,
  saveRows,
  ROWS_CAP,
  type TranscriptRow,
} from "./chatTranscript";
import type { SessionEvent } from "../hooks/useSessionEvents";
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

  it("maps an event with options to an approval row (W2 ChoicePrompt)", () => {
    const r = eventToRow(
      ev({ kind: "answer", content: "session s2 wants to run rm -rf", options: ["Approve", "Deny"] }),
    );
    expect(r).not.toBeNull();
    expect(r!.kind).toBe("approval");
    expect(r!.options).toEqual(["Approve", "Deny"]);
    expect(r!.content).toContain("wants to run");
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
});
