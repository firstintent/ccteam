// V0.5.0 F96 — MailboxStream pure-helper tests.

import { describe, expect, it } from "vitest";
import { filterMessages } from "./MailboxStream";
import type { InboxMessage } from "../lib/teamsApi";

function msg(overrides: Partial<InboxMessage>): InboxMessage {
  return {
    from: "a",
    to: "b",
    text: "hello",
    timestamp: "2026-05-16T10:00:00Z",
    color: null,
    read: true,
    summary: null,
    is_idle_notification: false,
    ...overrides,
  };
}

describe("filterMessages", () => {
  it("drops idle notifications regardless of other filters", () => {
    const out = filterMessages(
      [msg({ is_idle_notification: true }), msg({ from: "x" })],
      {},
    );
    expect(out).toHaveLength(1);
    expect(out[0].from).toBe("x");
  });
  it("matches teammate against from OR to fields", () => {
    const list = [
      msg({ from: "team-lead", to: "researcher" }),
      msg({ from: "researcher", to: "team-lead" }),
      msg({ from: "pm", to: "team-lead" }),
    ];
    const out = filterMessages(list, { teammate: "researcher" });
    expect(out).toHaveLength(2);
    expect(out.every((m) => m.from === "researcher" || m.to === "researcher")).toBe(
      true,
    );
  });
  it("substring searches text + summary + from + to (case-insensitive)", () => {
    const list = [
      msg({ text: "Hello WORLD" }),
      msg({ text: "goodnight", summary: "hello" }),
      msg({ text: "nothing related", from: "Helly" }),
      msg({ text: "no match" }),
    ];
    const out = filterMessages(list, { search: "hell" });
    // Picks up "Hello", "hello" in summary, "Helly" in from.
    expect(out).toHaveLength(3);
  });
  it("returns all non-idle messages when filters are empty", () => {
    const list = [msg({ from: "a" }), msg({ from: "b" })];
    expect(filterMessages(list, {})).toHaveLength(2);
  });
  it("trims whitespace on teammate and search inputs", () => {
    const list = [msg({ from: "researcher" })];
    expect(filterMessages(list, { teammate: " researcher " })).toHaveLength(1);
    expect(filterMessages(list, { search: " hello " })).toHaveLength(1);
  });
});
