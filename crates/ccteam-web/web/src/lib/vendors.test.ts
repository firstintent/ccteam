// v0.8.24 Track A — the 5-way vendor registry driving the composer's
// model+effort+protocol menu (prototype VENDORS + the opencode extension),
// now a STATIC FALLBACK behind the live `GET /api/v1/models` catalog.

import { describe, expect, it } from "vitest";

import { I18N } from "./i18n";
import type { VendorCatalog } from "./modelsApi";
import {
  defaultDraft,
  effortRowLabel,
  effortRowsFor,
  effortSwitchFor,
  modelRowsFor,
  modelSwitchFor,
  normalizeDraft,
  slugFromPath,
  statusDotClass,
  vendorChipClass,
  vendorDotClass,
  vendorSpec,
  visibleProtocols,
  wireProtocol,
  MODEL_DEFAULT,
  VENDORS,
} from "./vendors";

/** What the daemon reports for a box with kimi + opencode + grok observed. */
const CATALOG: VendorCatalog = {
  kimi: {
    models: [
      "kimi-code/k3",
      "kimi-code/k3-256k",
      "kimi-code/kimi-for-coding",
      "kimi-code/kimi-for-coding-highspeed",
    ],
    efforts: ["low", "high", "max"],
  },
  grok: { models: ["grok-4.5"], efforts: ["low", "medium", "high"] },
  opencode: { models: ["anthropic/claude-opus-5", "openai/gpt-5.5"], efforts: [] },
};

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
  // Cross-user fix (2026-07-28) — the protocol menu used to hide claude `terminal` from tenants.
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

describe("modelRowsFor (menu rows = default + what the vendor declared)", () => {
  it("always leads with the vendor-default row", () => {
    for (const vendor of VENDORS) {
      expect(modelRowsFor(vendor.id)[0]).toBe(MODEL_DEFAULT);
      expect(modelRowsFor(vendor.id, CATALOG)[0]).toBe(MODEL_DEFAULT);
    }
  });

  it("static fallback: claude keeps its CLI-documented --model aliases", () => {
    // Every non-default entry is wired verbatim to `claude --model` — the
    // menu must never show a token the CLI rejects (old bug: "opus-4.8").
    expect(modelRowsFor("claude")).toEqual([MODEL_DEFAULT, "fable", "opus", "sonnet", "haiku"]);
  });

  it("static fallback: the live-only catalogs offer the default alone (404 daemon)", () => {
    for (const vendor of ["codex", "grok", "opencode", "kimi"] as const) {
      expect(modelRowsFor(vendor)).toEqual([MODEL_DEFAULT]);
    }
  });

  it("the live catalog supersedes the static list, in the vendor's own order", () => {
    expect(modelRowsFor("kimi", CATALOG)).toEqual([MODEL_DEFAULT, ...CATALOG.kimi!.models]);
    expect(modelRowsFor("grok", CATALOG)).toEqual([MODEL_DEFAULT, "grok-4.5"]);
    expect(modelRowsFor("opencode", CATALOG)).toEqual([MODEL_DEFAULT, ...CATALOG.opencode!.models]);
  });

  it("a vendor missing from (or empty in) the catalog falls back to static", () => {
    expect(modelRowsFor("claude", CATALOG)).toEqual(modelRowsFor("claude"));
    expect(modelRowsFor("codex", { codex: { models: [], efforts: [] } })).toEqual([MODEL_DEFAULT]);
  });
});

describe("effortRowsFor (there is NO global effort ladder)", () => {
  it("leads with the default row, whose draft value is the empty string", () => {
    for (const vendor of VENDORS) {
      expect(effortRowsFor(vendor.id)[0]).toBe("");
    }
    expect(effortRowLabel("")).toBe("default");
    expect(effortRowLabel("xhigh")).toBe("xhigh");
  });

  it("static fallback = each CLI's own verified set (note the gaps)", () => {
    expect(effortRowsFor("claude")).toEqual(["", "low", "medium", "high", "xhigh", "max"]);
    // codex tops out at xhigh; kimi has NO medium; grok has NO max.
    expect(effortRowsFor("codex")).toEqual(["", "low", "medium", "high", "xhigh"]);
    expect(effortRowsFor("kimi")).toEqual(["", "low", "high", "max"]);
    expect(effortRowsFor("grok")).toEqual(["", "low", "medium", "high"]);
  });

  it("opencode declares no effort axis → the default row alone (composer hides the section)", () => {
    expect(effortRowsFor("opencode")).toEqual([""]);
    expect(effortRowsFor("opencode", CATALOG)).toEqual([""]);
  });

  it("the live catalog supersedes the static set", () => {
    expect(effortRowsFor("kimi", { kimi: { models: [], efforts: ["low", "max"] } })).toEqual([
      "",
      "low",
      "max",
    ]);
  });

  it("labels are language-independent by construction (no dictionary lookup)", () => {
    // 高 / 极高 are words no CLI takes and no statusline reports back — the
    // dictionary must never grow effort entries again.
    expect(I18N.zh.effLow).toBeUndefined();
    expect(I18N.en.effMax).toBeUndefined();
  });
});

describe("modelSwitchFor (create-form `model` field)", () => {
  it("is null for the vendor-default row, for every vendor", () => {
    expect(modelSwitchFor(defaultDraft())).toBeNull();
    for (const vendor of VENDORS) {
      expect(modelSwitchFor({ vendor: vendor.id, model: MODEL_DEFAULT }, CATALOG)).toBeNull();
      expect(modelSwitchFor({ vendor: vendor.id, model: "" }, CATALOG)).toBeNull();
    }
  });

  it("returns the model for a non-default pick", () => {
    expect(modelSwitchFor({ vendor: "claude", model: "sonnet" })).toBe("sonnet");
  });

  it("wires a live-catalog model for EVERY vendor — including opencode and kimi", () => {
    // The old code returned null for opencode ("self-selects") and kimi's menu
    // was default-only: both silently dropped the user's pick.
    expect(modelSwitchFor({ vendor: "opencode", model: "openai/gpt-5.5" }, CATALOG)).toBe(
      "openai/gpt-5.5",
    );
    expect(modelSwitchFor({ vendor: "kimi", model: "kimi-code/k3-256k" }, CATALOG)).toBe(
      "kimi-code/k3-256k",
    );
    expect(modelSwitchFor({ vendor: "grok", model: "grok-4.5" }, CATALOG)).toBe("grok-4.5");
  });

  it("is null for a model the vendor never declared", () => {
    expect(modelSwitchFor({ vendor: "codex", model: "made-up" })).toBeNull();
    expect(modelSwitchFor({ vendor: "kimi", model: "opus" }, CATALOG)).toBeNull();
  });
});

describe("effortSwitchFor (create-form `effort` field — pass-through, no remap)", () => {
  it("the default row wires nothing for every vendor", () => {
    for (const vendor of VENDORS) {
      expect(effortSwitchFor({ vendor: vendor.id, effort: "" }, CATALOG)).toBeNull();
    }
    expect(effortSwitchFor(defaultDraft())).toBeNull();
  });

  it("sends the vendor's own token verbatim for all five vendors", () => {
    // The regression this replaces: the `wireEffort` default arm returned null
    // for grok/opencode/kimi, so their menu pick evaporated on the way out.
    expect(effortSwitchFor({ vendor: "claude", effort: "max" })).toBe("max");
    expect(effortSwitchFor({ vendor: "codex", effort: "xhigh" })).toBe("xhigh");
    expect(effortSwitchFor({ vendor: "grok", effort: "high" })).toBe("high");
    expect(effortSwitchFor({ vendor: "kimi", effort: "max" }, CATALOG)).toBe("max");
    expect(
      effortSwitchFor({ vendor: "opencode", effort: "high" }, {
        opencode: { models: [], efforts: ["high"] },
      }),
    ).toBe("high");
  });

  it("never invents a rung the vendor doesn't have", () => {
    expect(effortSwitchFor({ vendor: "kimi", effort: "medium" }, CATALOG)).toBeNull();
    expect(effortSwitchFor({ vendor: "grok", effort: "max" }, CATALOG)).toBeNull();
    expect(effortSwitchFor({ vendor: "opencode", effort: "high" }, CATALOG)).toBeNull();
  });
});

describe("normalizeDraft (the one validity gate)", () => {
  it("repairs a cross-vendor model/protocol after a vendor switch", () => {
    const next = normalizeDraft({
      vendor: "codex",
      model: "opus", // claude model — invalid for codex
      effort: "xhigh",
      protocol: "terminal", // claude protocol — invalid for codex
      hitl: false,
    });
    expect(next.model).toBe(MODEL_DEFAULT);
    expect(next.protocol).toBe("app-server");
    expect(next.effort).toBe("xhigh"); // codex really does take xhigh
  });

  it("drops an effort the NEW vendor doesn't offer (kimi has no medium)", () => {
    const claude = { ...defaultDraft(), effort: "medium" };
    expect(normalizeDraft({ ...claude, vendor: "kimi" }, CATALOG).effort).toBe("");
    // …and keeps one it does.
    expect(normalizeDraft({ ...claude, vendor: "kimi", effort: "max" }, CATALOG).effort).toBe("max");
  });

  it("keeps a live-catalog model that the static registry has never heard of", () => {
    const draft = { ...defaultDraft(), vendor: "kimi" as const, model: "kimi-code/k3", protocol: "acp" };
    expect(normalizeDraft(draft, CATALOG).model).toBe("kimi-code/k3");
    // Without the catalog we know nothing about kimi's models → default row.
    expect(normalizeDraft(draft).model).toBe(MODEL_DEFAULT);
  });

  it("degrades a stale draft persisted by an older SPA (effortKey) instead of crashing", () => {
    // v0.9.11 shape: `{effortKey:"effMax"}` and no `effort` at all.
    const stale = { vendor: "claude", model: "opus", effortKey: "effMax", protocol: "stream-json", hitl: false };
    const next = normalizeDraft(stale as unknown as ReturnType<typeof defaultDraft>);
    expect(next.effort).toBe("");
    expect(next.model).toBe("opus");
    // The retired key is not round-tripped back into localStorage.
    expect(Object.keys(next).sort()).toEqual(["effort", "hitl", "model", "protocol", "vendor"]);
  });

  it("defaultDraft is the vendor default on both axes", () => {
    expect(defaultDraft().model).toBe(MODEL_DEFAULT);
    expect(defaultDraft().effort).toBe("");
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
