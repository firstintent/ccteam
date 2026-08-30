// v0.9.11 TEAM-3 — 编队起手 formation playbooks: the ONE definition module
// for ccteam's multi-vendor delegation patterns, consumed by BOTH the Home
// launcher's 快速开始 template grid (HomeView) AND the Team page 分工 tab's
// card section (CharterPanel), which hands off here via router state.
//
// Content red line: ccteam ships no persona/prompt content — an entry is UI
// documentation only (id, i18n key stem, icon, vendor lineup); picking one
// merely prefills the composer and aims the vendor draft (the established
// HomeView template mechanism), while the actual orchestration happens inside
// the spawned session via the `agent` tool.

import { Crown, Lightbulb, Pyramid, Radar, ShieldCheck, Trophy } from "lucide-react";
import { makeT, type Lang } from "./i18n";
import type { VendorId } from "./vendors";

export interface Playbook {
  /** Stable id — card testids (`tpl-<id>`), Team→Home router-state handoff. */
  id: string;
  /** i18n key stem: `<key>T` title, `<key>D` description, `<key>P` prefill. */
  key: string;
  Icon: typeof Crown;
  /** Brand-chip lineup; `vendors[0]` is the harness the spawn aims at. */
  vendors: readonly VendorId[];
}

/** The 6 owner-approved formations (v0.9.11). Order = display order. */
export const PLAYBOOKS: ReadonlyArray<Playbook> = [
  { id: "commander", key: "tplCommander", Icon: Crown, vendors: ["claude", "codex", "grok"] },
  { id: "advisor", key: "tplAdvisor", Icon: Lightbulb, vendors: ["grok", "claude"] },
  { id: "crossreview", key: "tplCrossreview", Icon: ShieldCheck, vendors: ["claude", "codex"] },
  { id: "bakeoff", key: "tplBakeoff", Icon: Trophy, vendors: ["claude", "codex", "grok"] },
  { id: "triangulate", key: "tplTriangulate", Icon: Radar, vendors: ["grok", "claude", "codex"] },
  { id: "pyramid", key: "tplPyramid", Icon: Pyramid, vendors: ["kimi", "opencode", "claude"] },
];

/** The composer patch a playbook applies — prefill text + the lead vendor to
 *  aim the spawn at. Pure and node-env testable; BOTH entry paths (Home card
 *  click, Team page 起手 handoff) go through it. Unknown id → null. */
export function applyPlaybook(id: string, lang: Lang): { text: string; vendor: VendorId } | null {
  const pb = PLAYBOOKS.find((p) => p.id === id);
  if (!pb) return null;
  return { text: makeT(lang)(`${pb.key}P`), vendor: pb.vendors[0]! };
}

/** One-shot router-state extraction for the Team→Home handoff: the 起手 CTA
 *  navigates to `/` with `{ state: { playbook: id } }`; anything else → null. */
export function playbookFromState(state: unknown): string | null {
  if (state && typeof state === "object" && "playbook" in state) {
    const id = (state as { playbook?: unknown }).playbook;
    if (typeof id === "string") return id;
  }
  return null;
}
