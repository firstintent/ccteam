// v0.8.24 Track A — the 5-way vendor registry driving the composer's
// model+effort+protocol menu (prototype `VENDORS`), extended with opencode
// (the prototype predates the 4th vendor) and kimi (the 5th; owner call:
// never collapse a vendor into another vendor's colors).
//
// What lives here is the STATIC fallback: what ccteam knows about a vendor
// with no daemon on the line. The live truth is `GET /api/v1/models` (see
// `lib/modelsApi.ts`), which reports what each installed CLI actually
// declares; every menu below takes that catalog as an argument and only falls
// back to this file when the route is unavailable (older daemon → 404) or the
// vendor was never observed.
//
// Dependency-free + pure so the menu structure / gating is unit-testable
// without the React import chain (the `VendorCatalog` import is type-only).

import type { VendorCatalog } from "./modelsApi";

export type VendorId = "claude" | "codex" | "grok" | "opencode" | "kimi";

/** One selectable wire protocol for a vendor. `wire` is the value POSTed to
 *  `POST /projects/{slug}/sessions` (`protocol` field); `label` is what the
 *  menu shows (codex's app-server IS the stream-json wire value). */
export interface ProtocolOption {
  id: string;
  label: string;
  /** Menu sub-caption (transport hint). */
  sub: string;
  wire: "stream-json" | "terminal" | "acp";
}

export interface VendorSpec {
  id: VendorId;
  label: string;
  /** Model ids ccteam knows WITHOUT the daemon, i.e. the ones baked into the
   *  CLI itself. Does NOT carry the default row — {@link modelRowsFor} adds
   *  it. Empty ⇒ this vendor's catalog is only knowable live. */
  models: string[];
  /** Reasoning-effort tokens this vendor's CLI accepts (verified 2026-08-02
   *  against the installed CLIs). Unlike models, an effort set is a small
   *  closed enum the CLI ships with — the daemon pins the same fallback
   *  server-side — so it is safe to keep a static copy. Empty ⇒ the vendor
   *  declares no effort axis and the menu shows no effort section. */
  efforts: string[];
  protocols: ProtocolOption[];
}

/** The "vendor default" menu entry — wires nothing (the CLI picks its own
 *  model). Every OTHER entry is sent verbatim to the vendor CLI, so the menu
 *  must never show a name the CLI would reject: the old catalog offered
 *  "fable-5"/"opus-4.8"/"grok-4" etc., which are neither valid aliases nor
 *  full model ids — picking them errored (or warned) at spawn. */
export const MODEL_DEFAULT = "默认";

/** The effort counterpart of {@link MODEL_DEFAULT}. The DRAFT value is the
 *  empty string (wire nothing); `default` is only how that row reads, and it
 *  reads the same in zh and en — see {@link effortRowLabel}. */
export const EFFORT_DEFAULT_LABEL = "default";

export const VENDORS: VendorSpec[] = [
  {
    id: "claude",
    label: "claude",
    // Exactly the tokens `claude --model` documents: an alias for the latest
    // model of each family ('fable', 'opus', 'sonnet', 'haiku'). Full ids
    // (claude-fable-5) also work but the aliases track "latest" honestly.
    models: ["fable", "opus", "sonnet", "haiku"],
    efforts: ["low", "medium", "high", "xhigh", "max"],
    protocols: [
      { id: "stream-json", label: "stream-json", sub: "NDJSON", wire: "stream-json" },
      { id: "terminal", label: "terminal", sub: "tmux", wire: "terminal" },
    ],
  },
  {
    id: "codex",
    label: "codex",
    // codex `-m` takes a free-form model id we cannot enumerate from the CLI.
    models: [],
    // codex's `ReasoningEffort` set has no `max` — its top rung is `xhigh`.
    efforts: ["low", "medium", "high", "xhigh"],
    protocols: [
      { id: "app-server", label: "app-server", sub: "JSON-RPC", wire: "stream-json" },
    ],
  },
  {
    id: "grok",
    label: "grok",
    // grok declares its models in the ACP handshake — live-only.
    models: [],
    // …and stops at `high`: offering `max` here would send a token it rejects.
    efforts: ["low", "medium", "high"],
    protocols: [{ id: "acp", label: "acp", sub: "JSON-RPC stdio", wire: "acp" }],
  },
  {
    id: "opencode",
    label: "opencode",
    // OpenCode is provider-agnostic: its ~9 models come from the user's own
    // provider config, so nothing is knowable without the daemon.
    models: [],
    // No effort axis declared today — an effort menu here would be theatre.
    efforts: [],
    protocols: [{ id: "acp", label: "acp", sub: "JSON-RPC stdio", wire: "acp" }],
  },
  {
    id: "kimi",
    label: "Kimi",
    // Kimi's model catalog arrives live via ACP `availableModels` (the
    // in-session `/model` picker) and is account/plan dependent.
    models: [],
    // Note the gap: kimi has NO `medium`. There is no global ladder.
    efforts: ["low", "high", "max"],
    protocols: [{ id: "acp", label: "acp", sub: "JSON-RPC stdio", wire: "acp" }],
  },
];

export function vendorSpec(id: string): VendorSpec {
  return VENDORS.find((v) => v.id === id) ?? VENDORS[0]!;
}

/** The model rows the composer shows for `vendor`: the default row first, then
 *  the vendor's OWN ids — live catalog when the daemon reported any, else the
 *  static registry (claude's `--model` aliases; nothing for the vendors whose
 *  catalog is only knowable live). Never a name the vendor didn't declare. */
export function modelRowsFor(vendor: string, catalog?: VendorCatalog | null): string[] {
  const spec = vendorSpec(vendor);
  const live = catalog?.[spec.id]?.models ?? [];
  return [MODEL_DEFAULT, ...(live.length > 0 ? live : spec.models)];
}

/** The effort rows for `vendor`: the default row (`""` — wire nothing) first,
 *  then the vendor's own tokens verbatim. A vendor with no effort axis gets
 *  the default row ALONE, which the composer reads as "render no effort
 *  section" — the point of the whole change is to stop offering a menu that
 *  does nothing. */
export function effortRowsFor(vendor: string, catalog?: VendorCatalog | null): string[] {
  const spec = vendorSpec(vendor);
  const live = catalog?.[spec.id]?.efforts ?? [];
  return ["", ...(live.length > 0 ? live : spec.efforts)];
}

/** How an effort row reads. The token is shown VERBATIM — `high`/`xhigh`/`max`
 *  are what the CLI takes and what the statusline reports back, so 高/极高
 *  would be a third spelling of a value nobody uses. Identical in zh and en by
 *  construction (no dictionary lookup); `""` reads `default`. */
export function effortRowLabel(effort: string): string {
  return effort || EFFORT_DEFAULT_LABEL;
}

/** Protocols a caller may pick for `vendor`. Cross-user fix (2026-07-28) — no admin gate: every
 *  logged-in user gets the same functional surface, and what they may reach is
 *  decided by identity × project ownership on the backend, not by hiding menu
 *  entries. (The claude `terminal` protocol stays frozen/maintenance-only —
 *  that is a roadmap fact, not a per-user permission.) */
export function visibleProtocols(vendor: string): ProtocolOption[] {
  return vendorSpec(vendor).protocols;
}

/** The composer draft — what the Home lazy-create POSTs from. */
export interface ComposerDraft {
  vendor: VendorId;
  /** A model id the vendor declared, or {@link MODEL_DEFAULT} for its own. */
  model: string;
  /** The vendor's OWN effort token, verbatim; `""` = vendor default (wire
   *  nothing). Deliberately a plain string and not a ccteam-side enum: there
   *  is no global ladder to enumerate — kimi has no `medium`, grok has no
   *  `max` — so any shared key set would have to lie about somebody. */
  effort: string;
  /** Protocol option id (menu row), NOT the wire value. */
  protocol: string;
  hitl: boolean;
}

export function defaultDraft(): ComposerDraft {
  return {
    vendor: "claude",
    model: MODEL_DEFAULT,
    effort: "",
    protocol: VENDORS[0]!.protocols[0]!.id,
    hitl: false,
  };
}

/** Resolve the draft's wire protocol; unknown/hidden ids fall back to the
 *  vendor's first (stable) protocol. */
export function wireProtocol(draft: Pick<ComposerDraft, "vendor" | "protocol">): "stream-json" | "terminal" | "acp" {
  const spec = vendorSpec(draft.vendor);
  const found = spec.protocols.find((p) => p.id === draft.protocol);
  return (found ?? spec.protocols[0]!).wire;
}

/** The model to send on the create form (`model` field), or null for the
 *  vendor default: only a NON-default pick wires anything (v0.8.24 A-U3 —
 *  this used to drive a post-spawn `/model` control turn; it now rides
 *  `POST .../sessions` directly and the vendor-native spawn seam applies it:
 *  claude `--model`, codex turn/start override, grok `-m`, opencode
 *  `set_config_option`, kimi `session/set_model`). Applies to EVERY vendor —
 *  the old per-vendor "return null for opencode/kimi" arms were a silent drop
 *  of the user's pick, which is the same lie as an inert menu. */
export function modelSwitchFor(
  draft: Pick<ComposerDraft, "vendor" | "model">,
  catalog?: VendorCatalog | null,
): string | null {
  if (!draft.model || draft.model === MODEL_DEFAULT) return null;
  return modelRowsFor(draft.vendor, catalog).includes(draft.model) ? draft.model : null;
}

/** The effort token to send (`effort` field), or null for the vendor default.
 *  Pure pass-through of the vendor's own token — no ccteam-side remapping,
 *  and no vendor is dropped. Same offer-only-what-was-declared guard as
 *  {@link modelSwitchFor}: a token this vendor never declared is not sent. */
export function effortSwitchFor(
  draft: Pick<ComposerDraft, "vendor" | "effort">,
  catalog?: VendorCatalog | null,
): string | null {
  if (!draft.effort) return null;
  return effortRowsFor(draft.vendor, catalog).includes(draft.effort) ? draft.effort : null;
}

/** The single validity gate for a draft: after a vendor switch (or a reload of
 *  a persisted draft) the model / effort / protocol must all be things THIS
 *  vendor offers — anything else falls back to its default row.
 *
 *  Pass the live catalog wherever it is available: without it only the static
 *  registry answers, so a perfectly good `kimi-code/k3` would be wiped for
 *  looking unknown. Callers that hold a catalog (Home, the composer) normalize
 *  at render, once it has loaded.
 *
 *  The result is built field-by-field rather than spread over `draft` on
 *  purpose: a draft persisted by an older SPA carries the retired `effortKey`,
 *  and rebuilding drops it instead of round-tripping it back into
 *  localStorage. Such a draft degrades to the vendor default (`effort: ""`),
 *  never to a crash. */
export function normalizeDraft(draft: ComposerDraft, catalog?: VendorCatalog | null): ComposerDraft {
  const spec = vendorSpec(draft.vendor);
  const models = modelRowsFor(spec.id, catalog);
  const efforts = effortRowsFor(spec.id, catalog);
  return {
    vendor: spec.id,
    model: models.includes(draft.model) ? draft.model : MODEL_DEFAULT,
    effort: efforts.includes(draft.effort) ? draft.effort : "",
    protocol: spec.protocols.some((p) => p.id === draft.protocol)
      ? draft.protocol
      : spec.protocols[0]!.id,
    hitl: !!draft.hitl,
  };
}

/** Prototype `.dot.{vendor}` class. */
export function vendorDotClass(vendor: string): string {
  return `dot ${vendorSpec(vendor).id}`;
}

/** Prototype `.chip.{vendor}` class. */
export function vendorChipClass(vendor: string): string {
  return `chip ${vendorSpec(vendor).id}`;
}

/** Session status → prototype dot state (`on` green / `busy` amber / `off`
 *  gray / `err` red). Live-but-unknown statuses read green (idle/live). */
export function statusDotClass(status: string | null | undefined, opts?: { off?: boolean }): string {
  if (opts?.off) return "dot off";
  switch (status) {
    case "working":
    case "stale":
      return "dot busy";
    case "stuck":
      return "dot err";
    default:
      return "dot on";
  }
}

/** Mirror of the backend slug grammar (`ccteam_core::validate_slug_format`):
 *  lowercase `[a-z0-9-]+`, ≤60, no leading/trailing `-`. Derives a slug from
 *  a new-project path's basename (prototype: path input only, no slug field). */
export function slugFromPath(path: string): string {
  const base = path.trim().replace(/\/+$/, "").split("/").pop() ?? "";
  const slug = base
    .toLowerCase()
    .replace(/[^a-z0-9-]+/g, "-")
    .replace(/-{2,}/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 60)
    .replace(/-+$/, "");
  return slug;
}
