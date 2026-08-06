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
    expect(CHAT_ALLOWED_URI_REGEXP.test("docs/readme.md")).toBe(true);
    expect(CHAT_ALLOWED_URI_REGEXP.test("foo/bar")).toBe(true);
    expect(CHAT_ALLOWED_URI_REGEXP.test("abc123")).toBe(true);
    expect(CHAT_ALLOWED_URI_REGEXP.test("data:image/png;base64,AAAA")).toBe(false);
    expect(CHAT_ALLOWED_URI_REGEXP.test("blob:https://example.test/id")).toBe(false);
    expect(CHAT_ALLOWED_URI_REGEXP.test("javascript:alert(1)")).toBe(false);
  });

  it("preserves relative links while the sanitizer hook removes unsafe schemes", () => {
    const rendered = renderMarkdown(
      [
        "[docs](docs/readme.md)",
        "[nested](foo/bar)",
        "[slug](abc123)",
        "[blob](blob:https://example.test/id)",
        "[script](javascript:alert(1))",
      ].join("\n\n"),
    );

    expect(rendered).toContain('href="docs/readme.md"');
    expect(rendered).toContain('href="foo/bar"');
    expect(rendered).toContain('href="abc123"');
    expect(rendered).not.toContain('href="blob:');
    expect(rendered).not.toContain('href="javascript:');
  });
});

describe("chat markdown tables", () => {
  it("wraps GFM tables in the scroll container so wide content cannot overflow", () => {
    const rendered = renderMarkdown(
      ["| col a | col b |", "| --- | --- |", "| one | two |"].join("\n"),
    );

    expect(rendered).toContain('<div class="cockpit-table-wrap"><table>');
    expect(rendered).toContain("</table></div>");
    expect(rendered).toContain("<td>one</td>");
  });

  it("wraps every table in a multi-table message", () => {
    const table = ["| a |", "| --- |", "| 1 |"].join("\n");
    const rendered = renderMarkdown(`${table}\n\nbetween\n\n${table}`);

    expect(rendered.match(/cockpit-table-wrap/g)).toHaveLength(2);
  });

  it("does not double-wrap a table already inside the scroll container", () => {
    const rendered = renderMarkdown(
      '<div class="cockpit-table-wrap"><table><tbody><tr><td>x</td></tr></tbody></table></div>',
    );

    expect(rendered.match(/cockpit-table-wrap/g)).toHaveLength(1);
  });
});
