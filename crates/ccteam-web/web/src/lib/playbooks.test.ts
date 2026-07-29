// v0.9.11 TEAM-3 — formation playbook invariants. `lib/playbooks.ts` is the
// ONE definition module behind both the Home launcher grid and the Team page
// 分工 tab cards, so the shape is pinned here once: 6 owner-approved
// formations, unique ids, known vendors only, and a complete zh+en i18n
// triple per card (a hole would silently fall back zh → key in the UI).
// The pure helpers carry the whole Team→Home handoff chain: router state →
// playbook id (`playbookFromState`) → composer patch (`applyPlaybook`).

import { describe, expect, it } from "vitest";

import { applyPlaybook, playbookFromState, PLAYBOOKS } from "./playbooks";
import { I18N } from "./i18n";
import { VENDORS } from "./vendors";

describe("PLAYBOOKS (shared home/team formation definitions)", () => {
  it("holds the 6 owner-approved formations, unique ids, display order pinned", () => {
    expect(PLAYBOOKS.map((p) => p.id)).toEqual([
      "commander",
      "advisor",
      "crossreview",
      "bakeoff",
      "triangulate",
      "pyramid",
    ]);
    expect(new Set(PLAYBOOKS.map((p) => p.id)).size).toBe(PLAYBOOKS.length);
    // The retired single-vendor cards must not resurface.
    for (const dead of ["team", "compare", "review", "code", "fast", "bulk"]) {
      expect(PLAYBOOKS.some((p) => p.id === dead)).toBe(false);
    }
  });

  it("every lineup entry is a known VendorId and every lineup has a lead", () => {
    const known = new Set<string>(VENDORS.map((v) => v.id));
    for (const pb of PLAYBOOKS) {
      expect(pb.vendors.length, pb.id).toBeGreaterThan(0);
      for (const vendor of pb.vendors) {
        expect(known.has(vendor), `${pb.id}: ${vendor}`).toBe(true);
      }
    }
    // Multi-vendor delegation is the point: every formation fields ≥2 vendors.
    for (const pb of PLAYBOOKS) {
      expect(pb.vendors.length, pb.id).toBeGreaterThanOrEqual(2);
    }
  });

  it("each T/D/P i18n key resolves in zh AND en (no zh-fallback holes)", () => {
    for (const pb of PLAYBOOKS) {
      for (const suffix of ["T", "D", "P"] as const) {
        const key = `${pb.key}${suffix}`;
        expect(I18N.zh[key], `zh ${key}`).toBeTruthy();
        expect(I18N.en[key], `en ${key}`).toBeTruthy();
      }
    }
    // The Team page section chrome resolves in both languages too.
    for (const key of ["playbookSection", "playbookLaunch", "playbookHonesty"]) {
      expect(I18N.zh[key], `zh ${key}`).toBeTruthy();
      expect(I18N.en[key], `en ${key}`).toBeTruthy();
    }
  });

  it("applyPlaybook computes the composer patch: `<key>P` prefill + lead vendor", () => {
    const patch = applyPlaybook("commander", "zh");
    expect(patch).toEqual({ text: I18N.zh.tplCommanderP, vendor: "claude" });
    // The pyramid formation leads with the cheap harness, per the escalation
    // story; language picks the localized prefill.
    expect(applyPlaybook("pyramid", "en")).toEqual({
      text: I18N.en.tplPyramidP,
      vendor: "kimi",
    });
    // Unknown id → null (the handoff simply no-ops; nothing is invented).
    expect(applyPlaybook("nope", "zh")).toBeNull();
  });

  it("playbookFromState extracts only a string playbook id from router state", () => {
    expect(playbookFromState({ playbook: "advisor" })).toBe("advisor");
    expect(playbookFromState(null)).toBeNull();
    expect(playbookFromState(undefined)).toBeNull();
    expect(playbookFromState({})).toBeNull();
    expect(playbookFromState({ playbook: 7 })).toBeNull();
    expect(playbookFromState("advisor")).toBeNull();
  });
});
