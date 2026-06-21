// v0.8.18 柱2/UI — i18n helper tests.

import { describe, expect, it } from "vitest";

import { navLabel, tr } from "./i18n";

describe("i18n", () => {
  it("tr defaults to zh and picks en when chosen", () => {
    expect(tr("zh", "主机", "Hosts")).toBe("主机");
    expect(tr("en", "主机", "Hosts")).toBe("Hosts");
  });

  it("navLabel returns the per-language nav label", () => {
    expect(navLabel("hosts", "zh")).toBe("主机");
    expect(navLabel("hosts", "en")).toBe("Hosts");
    expect(navLabel("marketplace", "zh")).toBe("插件市场");
    expect(navLabel("marketplace", "en")).toBe("Plugins");
    expect(navLabel("status", "zh")).toBe("Status");
    expect(navLabel("settings", "en")).toBe("Settings");
  });

  it("navLabel falls back to the key for an unknown view", () => {
    expect(navLabel("nope", "zh")).toBe("nope");
    expect(navLabel("nope", "en")).toBe("nope");
  });
});
