// v0.8.24 Track A — pure display helpers for the sidebar rail + Home recents
// (extracted from the old ChatConsole; same direct-unit-test treatment).

import { describe, expect, it } from "vitest";

import { railSessionLabel, relativeTime, relativeTimeEn, relativeTimeZh } from "./railHelpers";

describe("railSessionLabel", () => {
  it("prefers the title when set", () => {
    expect(railSessionLabel({ title: "Fix the login bug", role: "reviewer" })).toBe(
      "Fix the login bug",
    );
  });

  it("falls back to the role when untitled", () => {
    expect(railSessionLabel({ title: null, role: "reviewer" })).toBe("reviewer");
    expect(railSessionLabel({ role: "reviewer" })).toBe("reviewer");
  });

  it("falls back to the roleless placeholder when neither is set", () => {
    expect(railSessionLabel({ title: null, role: "" })).toBe("(无 role)");
    expect(railSessionLabel({ title: "", role: "" })).toBe("(无 role)");
  });
});

describe("relativeTimeZh", () => {
  it("renders an em-dash for missing/unparseable input", () => {
    expect(relativeTimeZh(undefined)).toBe("—");
    expect(relativeTimeZh(null)).toBe("—");
    expect(relativeTimeZh("")).toBe("—");
    expect(relativeTimeZh("not-a-timestamp")).toBe("—");
  });

  it("buckets recent timestamps into 刚刚 / N分钟前 / N小时前", () => {
    const secondsAgo = (s: number) => new Date(Date.now() - s * 1000).toISOString();
    expect(relativeTimeZh(secondsAgo(10))).toBe("刚刚");
    expect(relativeTimeZh(secondsAgo(5 * 60))).toBe("5分钟前");
    expect(relativeTimeZh(secondsAgo(3 * 3600))).toBe("3小时前");
  });

  it("special-cases yesterday and buckets multi-day/week spans", () => {
    const secondsAgo = (s: number) => new Date(Date.now() - s * 1000).toISOString();
    expect(relativeTimeZh(secondsAgo(24 * 3600))).toBe("昨天");
    expect(relativeTimeZh(secondsAgo(3 * 24 * 3600))).toBe("3天前");
    expect(relativeTimeZh(secondsAgo(14 * 24 * 3600))).toBe("2周前");
  });

  it("falls back to an absolute date at >= 5 weeks", () => {
    const old = new Date(Date.now() - 40 * 24 * 3600 * 1000);
    expect(relativeTimeZh(old.toISOString())).toBe(old.toISOString().slice(0, 10));
  });
});

describe("relativeTimeEn (prototype compact card style)", () => {
  it("renders an em-dash for missing/unparseable input", () => {
    expect(relativeTimeEn(undefined)).toBe("—");
    expect(relativeTimeEn("garbage")).toBe("—");
  });

  it("buckets into now / Nm / Nh / Nd / Nw", () => {
    const secondsAgo = (s: number) => new Date(Date.now() - s * 1000).toISOString();
    expect(relativeTimeEn(secondsAgo(10))).toBe("now");
    expect(relativeTimeEn(secondsAgo(12 * 60))).toBe("12m");
    expect(relativeTimeEn(secondsAgo(2 * 3600))).toBe("2h");
    expect(relativeTimeEn(secondsAgo(3 * 24 * 3600))).toBe("3d");
    expect(relativeTimeEn(secondsAgo(14 * 24 * 3600))).toBe("2w");
  });
});

describe("relativeTime (language switch)", () => {
  it("delegates per language", () => {
    const secondsAgo = (s: number) => new Date(Date.now() - s * 1000).toISOString();
    expect(relativeTime("zh", secondsAgo(5 * 60))).toBe("5分钟前");
    expect(relativeTime("en", secondsAgo(5 * 60))).toBe("5m");
  });
});
