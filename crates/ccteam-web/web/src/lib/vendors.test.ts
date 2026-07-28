// v0.8.24 Track A — the 5-way vendor registry driving the composer's
// model+effort+protocol menu (prototype VENDORS + the opencode extension).

import { describe, expect, it } from "vitest";

import {
  defaultDraft,
  modelSwitchFor,
  wireEffort,
  normalizeDraft,
  slugFromPath,
  statusDotClass,
  vendorChipClass,
  vendorDotClass,
  vendorSpec,
  visibleProtocols,
  wireProtocol,
  VENDORS,
} from "./vendors";

describe("VENDORS registry (5-way)", () => {
  it("lists exactly claude / codex / grok / opencode / kimi — never collapses", () => {
    expect(VENDORS.map((v) => v.id)).toEqual(["claude", "codex", "grok", "opencode", "kimi"]);
  });

  it("claude offers stream-json (default) + terminal (frozen)", () => {
    const claude = vendorSpec("claude");
    expect(claude.protocols.map((p) => p.id)).toEqual(["stream-json", "terminal"]);
  });

  it("codex = app-server (wire stream-json); grok/opencode/kimi = acp", () => {
    expect(vendorSpec("codex").protocols.map((p) => `${p.id}:${p.wire}`)).toEqual([
      "app-server:stream-json",
    ]);
    expect(vendorSpec("grok").protocols.map((p) => p.wire)).toEqual(["acp"]);
    expect(vendorSpec("opencode").protocols.map((p) => p.wire)).toEqual(["acp"]);
    expect(vendorSpec("kimi").protocols.map((p) => p.wire)).toEqual(["acp"]);
  });

  it("falls back to claude for an unknown vendor", () => {
    expect(vendorSpec("nope").id).toBe("claude");
  });
});

describe("visibleProtocols (no admin gate — same menu for every user)", () => {
  // v0.9.11 — the protocol menu used to hide claude `terminal` from tenants.
  // Function surfaces are open to every logged-in user; what a user may
  // actually reach is decided by identity × project ownership on the backend,
  // not by a hidden menu entry. The admin menu (Settings → 管理员) is the only
  // admin-scoped surface left.
  it("offers the full claude menu to every identity", () => {
    expect(visibleProtocols("claude").map((p) => p.id)).toEqual(["stream-json", "terminal"]);
  });

  it("matches the vendor registry for the acp vendors", () => {
    expect(visibleProtocols("opencode").map((p) => p.id)).toEqual(["acp"]);
    expect(visibleProtocols("codex").map((p) => p.id)).toEqual(["app-server"]);
  });

  it("no vendor hides a protocol from anyone (nothing re-introduces a UI gate)", () => {
    for (const vendor of VENDORS) {
      expect(visibleProtocols(vendor.id).map((p) => p.id)).toEqual(
        vendor.protocols.map((p) => p.id),
      );
    }
  });
});

describe("wireProtocol", () => {
  it("resolves the menu id to the wire value (app-server → stream-json)", () => {
    expect(wireProtocol({ vendor: "codex", protocol: "app-server" })).toBe("stream-json");
    expect(wireProtocol({ vendor: "claude", protocol: "terminal" })).toBe("terminal");
    expect(wireProtocol({ vendor: "grok", protocol: "acp" })).toBe("acp");
  });

  it("falls back to the vendor's first protocol for an unknown id", () => {
    expect(wireProtocol({ vendor: "claude", protocol: "acp" })).toBe("stream-json");
  });
});

describe("modelSwitchFor (lazy-create /model follow-up)", () => {
  it("is null for the vendor-default model (no /model turn)", () => {
    const d = defaultDraft();
    expect(modelSwitchFor(d)).toBeNull();
  });

  it("returns the model for a non-default pick", () => {
    expect(modelSwitchFor({ vendor: "claude", model: "sonnet" })).toBe("sonnet");
  });

  it("claude offers only CLI-documented --model aliases after the default", () => {
    // Every non-default entry is wired verbatim to `claude --model` — the
    // menu must never show a token the CLI rejects (old bug: "opus-4.8").
    expect(vendorSpec("claude").models.slice(1)).toEqual(["fable", "opus", "sonnet", "haiku"]);
  });

  it("codex/grok/opencode/kimi offer only the honest vendor default", () => {
    for (const vendor of ["codex", "grok", "opencode", "kimi"] as const) {
      expect(vendorSpec(vendor).models).toHaveLength(1);
      expect(modelSwitchFor({ vendor, model: vendorSpec(vendor).models[0]! })).toBeNull();
    }
  });

  it("never switches for opencode (self-selects its model)", () => {
    expect(modelSwitchFor({ vendor: "opencode", model: "anything" })).toBeNull();
  });

  it("is null for a model the vendor doesn't list", () => {
    expect(modelSwitchFor({ vendor: "codex", model: "made-up" })).toBeNull();
  });
});

describe("wireEffort (A-U3 create-form effort field)", () => {
  it("effDefault wires nothing for every vendor (vendor default holds)", () => {
    for (const vendor of ["claude", "codex", "grok", "opencode", "kimi"] as const) {
      expect(wireEffort({ vendor, effortKey: "effDefault" })).toBeNull();
    }
  });

  it("maps claude to its verified --effort levels (max stays max)", () => {
    expect(wireEffort({ vendor: "claude", effortKey: "effLow" })).toBe("low");
    expect(wireEffort({ vendor: "claude", effortKey: "effMid" })).toBe("medium");
    expect(wireEffort({ vendor: "claude", effortKey: "effHigh" })).toBe("high");
    expect(wireEffort({ vendor: "claude", effortKey: "effMax" })).toBe("max");
  });

  it("maps codex 极高 to xhigh (its ReasoningEffort set has no max)", () => {
    expect(wireEffort({ vendor: "codex", effortKey: "effMax" })).toBe("xhigh");
    expect(wireEffort({ vendor: "codex", effortKey: "effLow" })).toBe("low");
  });

  it("never wires grok/opencode/kimi effort (undocumented / per-model / unwired value sets)", () => {
    expect(wireEffort({ vendor: "grok", effortKey: "effMax" })).toBeNull();
    expect(wireEffort({ vendor: "opencode", effortKey: "effHigh" })).toBeNull();
    expect(wireEffort({ vendor: "kimi", effortKey: "effHigh" })).toBeNull();
  });

  it("defaultDraft starts at effDefault (nothing wired until picked)", () => {
    expect(defaultDraft().effortKey).toBe("effDefault");
    expect(wireEffort(defaultDraft())).toBeNull();
  });
});

describe("normalizeDraft", () => {
  it("repairs a cross-vendor model/protocol after a vendor switch", () => {
    const next = normalizeDraft({
      vendor: "codex",
      model: "opus", // claude model — invalid for codex
      effortKey: "effMax",
      protocol: "terminal", // claude protocol — invalid for codex
      hitl: false,
    });
    expect(next.model).toBe(vendorSpec("codex").models[0]!);
    expect(next.protocol).toBe("app-server");
  });
});

describe("dot / chip classes", () => {
  it("emits prototype vendor classes for all five vendors", () => {
    expect(vendorDotClass("claude")).toBe("dot claude");
    expect(vendorDotClass("opencode")).toBe("dot opencode");
    expect(vendorDotClass("kimi")).toBe("dot kimi");
    expect(vendorChipClass("grok")).toBe("chip grok");
    expect(vendorChipClass("codex")).toBe("chip codex");
    expect(vendorChipClass("kimi")).toBe("chip kimi");
  });

  it("maps live status to prototype dot states", () => {
    expect(statusDotClass("working")).toBe("dot busy");
    expect(statusDotClass("stale")).toBe("dot busy");
    expect(statusDotClass("stuck")).toBe("dot err");
    expect(statusDotClass("idle")).toBe("dot on");
    expect(statusDotClass(undefined)).toBe("dot on");
    expect(statusDotClass("live", { off: true })).toBe("dot off");
  });
});

describe("slugFromPath (inline 新建项目 — slug derives from the basename)", () => {
  it("slugifies the path basename", () => {
    expect(slugFromPath("~/work/My App")).toBe("my-app");
    expect(slugFromPath("/srv/demo_shop/")).toBe("demo-shop");
    expect(slugFromPath("~/work/my-app")).toBe("my-app");
  });

  it("strips leading/trailing hyphens and collapses runs", () => {
    expect(slugFromPath("/x/--Weird---Name--")).toBe("weird-name");
  });

  it("returns empty for an unusable path", () => {
    expect(slugFromPath("")).toBe("");
    expect(slugFromPath("///")).toBe("");
  });
});
