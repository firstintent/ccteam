// v0.8.18 柱2/UI + v0.8.24 Track A — i18n helper + whole-shell dictionary tests.

import { describe, expect, it } from "vitest";

import { I18N, makeT, navLabel, t, tr, tShowMore, tStopped } from "./i18n";

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
    expect(navLabel("settings", "zh")).toBe("设置");
    expect(navLabel("workflow", "zh")).toBe("工作流");
    expect(navLabel("workflow", "en")).toBe("Workflow");
  });

  it("navLabel falls back to the key for an unknown view", () => {
    expect(navLabel("nope", "zh")).toBe("nope");
    expect(navLabel("nope", "en")).toBe("nope");
  });
});

// v0.8.24 Track A — table-driven whole-shell dictionary (prototype I18N keys).
describe("I18N dictionary", () => {
  it("covers zh and en with the same key set", () => {
    const zhKeys = Object.keys(I18N.zh).sort();
    const enKeys = Object.keys(I18N.en).sort();
    expect(enKeys).toEqual(zhKeys);
    expect(zhKeys.length).toBeGreaterThan(60);
  });

  it("t() resolves per language and falls back to the key when unknown", () => {
    expect(t("zh", "homeTitle")).toBe("开工吧!");
    expect(t("en", "homeTitle")).toBe("Let's build!");
    expect(t("en", "definitely-not-a-key")).toBe("definitely-not-a-key");
  });

  it("makeT curries the language", () => {
    const tt = makeT("en");
    expect(tt("quickStart")).toBe("Quick start");
    expect(t("zh", "setOps")).toBe("运维总览");
    expect(tt("setOps")).toBe("Ops & Hosts");
  });

  it("parameterized phrases interpolate per language", () => {
    expect(tShowMore("zh", 3)).toBe("展开显示(还有 3 个)");
    expect(tShowMore("en", 3)).toBe("Show more (3 more)");
    expect(tStopped("zh", "s9")).toContain("s9");
    expect(tStopped("en", "s9")).toContain("Stopped s9");
  });

  it("keeps the HITL permission-mode semantics in both languages", () => {
    expect(t("zh", "hitlOn")).toContain("--permission-mode default");
    expect(t("en", "hitlOff")).toContain("--dangerously-skip-permissions");
  });
});
