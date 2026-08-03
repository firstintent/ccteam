// v0.8.24 Track A — the vendor registry driving the composer's
// model+effort+protocol menu (prototype VENDORS + the opencode extension),
// now a STATIC FALLBACK behind the live `GET /api/v1/models` catalog.

import { describe, expect, it } from "vitest";
import { readFileSync } from "node:fs";

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
  selectDraftModel,
  slugFromPath,
  statusDotClass,
  switchDraftVendor,
  vendorChipClass,
  vendorDotClass,
  vendorSpec,
  visibleProtocols,
  wireProtocol,
  MODEL_DEFAULT,
  VENDORS,
} from "./vendors";

/** What the daemon reports for a box with Pi + kimi + opencode + grok observed. */
const CATALOG: VendorCatalog = {
  kimi: {
    models: [
      { id: "kimi-code/k3" },
      { id: "kimi-code/k3-256k" },
      { id: "kimi-code/kimi-for-coding" },
      { id: "kimi-code/kimi-for-coding-highspeed" },
    ],
    efforts: ["low", "high", "max"],
  },
  grok: { models: [{ id: "grok-4.5" }], efforts: ["low", "medium", "high"] },
  opencode: {
    models: [{ id: "anthropic/claude-opus-5" }, { id: "openai/gpt-5.5" }],
    efforts: [],
  },
  pi: {
    models: [
      {
        id: "anthropic/claude-opus-4-6",
        efforts: ["off", "minimal", "low", "medium", "high", "xhigh"],
      },
      { id: "anthropic/claude-sonnet-4-6", efforts: [] },
      { id: "openai/gpt-5.6", efforts: ["off", "low", "medium", "high", "max"] },
    ],
    efforts: ["off", "minimal", "low", "medium", "high", "xhigh", "max"],
  },
};

describe("VENDORS registry (6-way)", () => {
  it("lists exactly six distinct harnesses — never collapses Pi", () => {
    expect(VENDORS.map((v) => v.id)).toEqual([
      "claude",
      "codex",
      "grok",
      "opencode",
      "kimi",
      "pi",
    ]);
    expect(new Set(VENDORS.map((v) => v.label)).size).toBe(6);
    expect(vendorSpec("pi").label).toBe("Pi");
    const css = readFileSync(new URL("../index.css", import.meta.url), "utf8");
    const color = (vendor: string) => css.match(new RegExp(`--${vendor}:\\s*(#[0-9A-F]+)`, "i"))?.[1];
    const pi = color("pi");
    expect(pi).toBeTruthy();
    expect(["claude", "codex", "grok", "opencode", "kimi"].map(color)).not.toContain(pi);
  });

  it("claude offers stream-json (default) + terminal (frozen)", () => {
    const claude = vendorSpec("claude");
    expect(claude.protocols.map((p) => p.id)).toEqual(["stream-json", "terminal"]);
  });

  it("Pi uses stream-json / Pi RPC JSONL while the ACP vendors remain ACP", () => {
    expect(vendorSpec("codex").protocols.map((p) => `${p.id}:${p.wire}`)).toEqual([
      "app-server:stream-json",
    ]);
    expect(vendorSpec("grok").protocols.map((p) => p.wire)).toEqual(["acp"]);
    expect(vendorSpec("opencode").protocols.map((p) => p.wire)).toEqual(["acp"]);
    expect(vendorSpec("kimi").protocols.map((p) => p.wire)).toEqual(["acp"]);
    expect(vendorSpec("pi").protocols).toEqual([
      { id: "stream-json", label: "stream-json", sub: "Pi RPC JSONL", wire: "stream-json" },
    ]);
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
    for (const vendor of ["codex", "grok", "opencode", "kimi", "pi"] as const) {
      expect(modelRowsFor(vendor)).toEqual([MODEL_DEFAULT]);
    }
  });

  it("the live catalog supersedes the static list, in the vendor's own order", () => {
    expect(modelRowsFor("kimi", CATALOG)).toEqual([
      MODEL_DEFAULT,
      ...CATALOG.kimi!.models.map((model) => model.id),
    ]);
    expect(modelRowsFor("grok", CATALOG)).toEqual([MODEL_DEFAULT, "grok-4.5"]);
    expect(modelRowsFor("opencode", CATALOG)).toEqual([
      MODEL_DEFAULT,
      ...CATALOG.opencode!.models.map((model) => model.id),
    ]);
    expect(modelRowsFor("pi", CATALOG)).toEqual([
      MODEL_DEFAULT,
      "anthropic/claude-opus-4-6",
      "anthropic/claude-sonnet-4-6",
      "openai/gpt-5.6",
    ]);
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

  it("prefers the selected model, including an explicit empty non-reasoning axis", () => {
    expect(effortRowsFor("pi", CATALOG, "anthropic/claude-opus-4-6")).toEqual([
      "",
      "off",
      "minimal",
      "low",
      "medium",
      "high",
      "xhigh",
    ]);
    expect(effortRowsFor("pi", CATALOG, "openai/gpt-5.6")).toEqual([
      "",
      "off",
      "low",
      "medium",
      "high",
      "max",
    ]);
    expect(effortRowsFor("pi", CATALOG, "anthropic/claude-sonnet-4-6")).toEqual([""]);
  });

  it("uses the vendor union only before a model is selected or metadata exists", () => {
    expect(effortRowsFor("pi", CATALOG, MODEL_DEFAULT)).toEqual([
      "",
      ...CATALOG.pi!.efforts,
    ]);
    expect(
      effortRowsFor("kimi", {
        kimi: { models: [{ id: "kimi-code/k3" }], efforts: ["low", "max"] },
      }, "kimi-code/k3"),
    ).toEqual(["", "low", "max"]);
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

  it("passes explicit values through because the advisory catalog is not a whitelist", () => {
    expect(modelSwitchFor({ vendor: "codex", model: "made-up" })).toBe("made-up");
    expect(modelSwitchFor({ vendor: "kimi", model: "opus" }, CATALOG)).toBe("opus");
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

  it("passes explicit values through for adapter-side validation", () => {
    expect(effortSwitchFor({ vendor: "kimi", effort: "medium" }, CATALOG)).toBe("medium");
    expect(effortSwitchFor({ vendor: "grok", effort: "max" }, CATALOG)).toBe("max");
    expect(effortSwitchFor({ vendor: "opencode", effort: "high" }, CATALOG)).toBe("high");
  });
});

describe("draft transitions (catalog is advisory, vendor changes are explicit)", () => {
  it("repairs a cross-vendor model/protocol after a vendor switch", () => {
    const next = switchDraftVendor({
      vendor: "claude",
      model: "opus",
      effort: "xhigh",
      protocol: "terminal",
      hitl: false,
    }, "codex");
    expect(next.model).toBe(MODEL_DEFAULT);
    expect(next.protocol).toBe("app-server");
    expect(next.effort).toBe("");
  });

  it("clears only an effort explicitly unsupported by the newly picked model", () => {
    const next = selectDraftModel({
      vendor: "pi",
      model: "anthropic/claude-opus-4-6",
      effort: "xhigh",
      protocol: "stream-json",
      hitl: false,
    }, "pi", "openai/gpt-5.6", CATALOG);
    expect(next.effort).toBe("");
    expect(next.model).toBe("openai/gpt-5.6");
  });

  it("resets vendor-owned axes on a vendor switch", () => {
    const claude = { ...defaultDraft(), effort: "medium" };
    expect(switchDraftVendor(claude, "kimi").effort).toBe("");
    expect(switchDraftVendor(claude, "kimi").model).toBe(MODEL_DEFAULT);
  });

  it("keeps explicit models with or without advisory catalog data", () => {
    const draft = { ...defaultDraft(), vendor: "kimi" as const, model: "kimi-code/k3", protocol: "acp" };
    expect(normalizeDraft(draft, CATALOG).model).toBe("kimi-code/k3");
    expect(normalizeDraft(draft).model).toBe("kimi-code/k3");
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
  it("emits prototype vendor classes for all six vendors", () => {
    expect(vendorDotClass("claude")).toBe("dot claude");
    expect(vendorDotClass("opencode")).toBe("dot opencode");
    expect(vendorDotClass("kimi")).toBe("dot kimi");
    expect(vendorChipClass("grok")).toBe("chip grok");
    expect(vendorChipClass("codex")).toBe("chip codex");
    expect(vendorChipClass("kimi")).toBe("chip kimi");
    expect(vendorDotClass("pi")).toBe("dot pi");
    expect(vendorChipClass("pi")).toBe("chip pi");
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
