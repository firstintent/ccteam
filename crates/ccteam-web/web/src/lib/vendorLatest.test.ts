import { afterEach, describe, expect, it, vi } from "vitest";
import {
  __resetVendorLatestCacheForTests,
  extractVersion,
  fetchNpmLatest,
  fetchVendorLatests,
  isOutdated,
  npmPackageForVendor,
} from "./vendorLatest";

const realFetch = globalThis.fetch;

afterEach(() => {
  globalThis.fetch = realFetch;
  __resetVendorLatestCacheForTests();
  vi.restoreAllMocks();
});

describe("npmPackageForVendor", () => {
  it("maps the four npm-distributed vendors and leaves kimi unknown", () => {
    expect(npmPackageForVendor("claude")).toBe("@anthropic-ai/claude-code");
    expect(npmPackageForVendor("codex")).toBe("@openai/codex");
    expect(npmPackageForVendor("grok")).toBe("@xai-official/grok");
    expect(npmPackageForVendor("opencode")).toBe("opencode-ai");
    expect(npmPackageForVendor("kimi")).toBeNull();
    expect(npmPackageForVendor("unknown")).toBeNull();
  });
});

describe("extractVersion / isOutdated", () => {
  it("pulls the first dotted version out of probe strings", () => {
    expect(extractVersion("claude 2.1.220")).toBe("2.1.220");
    expect(extractVersion("codex-cli 0.144.1")).toBe("0.144.1");
    expect(extractVersion("grok 0.2.112 (9bbd559) [stable]")).toBe("0.2.112");
    expect(extractVersion(null)).toBeNull();
  });

  it("flags strictly-newer latest as outdated", () => {
    expect(isOutdated("claude 2.1.200", "2.1.220")).toBe(true);
    expect(isOutdated("2.1.220", "2.1.220")).toBe(false);
    expect(isOutdated("2.1.220", "2.1.200")).toBe(false);
    expect(isOutdated(null, "1.0.0")).toBe(false);
  });
});

describe("fetchNpmLatest / fetchVendorLatests", () => {
  it("reads version from the npm registry JSON and caches it", async () => {
    const fetchMock = vi.fn().mockResolvedValue({
      ok: true,
      json: async () => ({ version: "2.1.220" }),
    });
    globalThis.fetch = fetchMock as unknown as typeof fetch;

    await expect(fetchNpmLatest("@anthropic-ai/claude-code")).resolves.toBe("2.1.220");
    await expect(fetchNpmLatest("@anthropic-ai/claude-code")).resolves.toBe("2.1.220");
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("returns a vendor→version map and skips unmapped vendors", async () => {
    globalThis.fetch = vi.fn().mockImplementation(async (url: string) => {
      if (String(url).includes("claude-code")) {
        return { ok: true, json: async () => ({ version: "9.9.9" }) };
      }
      return { ok: false, json: async () => ({}) };
    }) as unknown as typeof fetch;

    const got = await fetchVendorLatests(["claude", "kimi", "claude"]);
    expect(got).toEqual({ claude: "9.9.9" });
    expect(got.kimi).toBeUndefined();
  });
});
