// @vitest-environment jsdom

import { describe, expect, it } from "vitest";

import { CHAT_ALLOWED_URI_REGEXP, renderMarkdown } from "./markdown";

describe("chat markdown URI sanitizer", () => {
  it("strips data image URIs while preserving ordinary image URLs", () => {
    const rendered = renderMarkdown(
      [
        "![embedded](data:image/png;base64,AAAA)",
        "<img alt=\"raw\" src=\"data:image/png;base64,BBBB\">",
        "![served](https://example.test/chart.png)",
      ].join("\n\n"),
    );

    expect(rendered.toLowerCase()).not.toContain("data:");
    expect(rendered).not.toContain("AAAA");
    expect(rendered).not.toContain("BBBB");
    expect(rendered).toContain('src="https://example.test/chart.png"');
  });

  it("keeps the configured URI allowlist closed to data/blob/script schemes", () => {
    expect(CHAT_ALLOWED_URI_REGEXP.test("/api/v1/projects/demo/uploads/chart.png")).toBe(true);
    expect(CHAT_ALLOWED_URI_REGEXP.test("https://example.test/chart.png")).toBe(true);
    expect(CHAT_ALLOWED_URI_REGEXP.test("data:image/png;base64,AAAA")).toBe(false);
    expect(CHAT_ALLOWED_URI_REGEXP.test("blob:https://example.test/id")).toBe(false);
    expect(CHAT_ALLOWED_URI_REGEXP.test("javascript:alert(1)")).toBe(false);
  });
});
