// v0.8.24 Track A — the vendor registry driving the composer's
// model+effort+protocol menu. Every harness owns a distinct label, transport,
// and visual identity; vendors never collapse into one another.
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

export type VendorId = "claude" | "codex" | "grok" | "opencode" | "kimi" | "pi";

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
  {
    id: "pi",
    label: "Pi",
    // Pi reports provider-scoped ids live; a static list would be an account-
    // specific whitelist and would collide across providers.
    models: [],
    // Pi's effort axis is model-specific. The live vendor union is only a
    // cold-start fallback until a model is selected.
    efforts: [],
    protocols: [
      { id: "stream-json", label: "stream-json", sub: "Pi RPC JSONL", wire: "stream-json" },
    ],
  },
];

export function vendorSpec(id: string): VendorSpec {
  return VENDORS.find((v) => v.id === id) ?? VENDORS[0]!;
}

/** The model rows the composer shows for `vendor`: the default row first, then
 *  the vendor's OWN ids — live catalog when the daemon reported any, else the
 *  static registry (claude's `--model` aliases; nothing for the vendors whose
 *  catalog is only knowable live). This controls picker rows only; it is not
 *  an adapter-side whitelist. */
export function modelRowsFor(vendor: string, catalog?: VendorCatalog | null): string[] {
  const spec = vendorSpec(vendor);
  const live = catalog?.[spec.id]?.models ?? [];
  return [MODEL_DEFAULT, ...(live.length > 0 ? live.map((model) => model.id) : spec.models)];
}

/** The effort rows for `vendor`: the default row (`""` — wire nothing) first,
 *  then the vendor's own tokens verbatim. A vendor with no effort axis gets
 *  the default row ALONE, which the composer reads as "render no effort
 *  section" — the point of the whole change is to stop offering a menu that
 *  does nothing. */
export function effortRowsFor(
  vendor: string,
  catalog?: VendorCatalog | null,
  selectedModel?: string | null,
): string[] {
  const spec = vendorSpec(vendor);
  const liveCatalog = catalog?.[spec.id];
  if (selectedModel && selectedModel !== MODEL_DEFAULT) {
    const model = liveCatalog?.models.find((entry) => entry.id === selectedModel);
    if (model?.efforts !== undefined) return ["", ...model.efforts];
  }
  const live = liveCatalog?.efforts ?? [];
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
  /** An explicit model id, or {@link MODEL_DEFAULT} for the vendor's own. */
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
 *  `set_config_option`, kimi `session/set_model`, Pi RPC state). Applies to EVERY vendor —
 *  the old per-vendor "return null for opencode/kimi" arms were a silent drop
 *  of the user's pick, which is the same lie as an inert menu. */
export function modelSwitchFor(
  draft: Pick<ComposerDraft, "vendor" | "model">,
  catalog?: VendorCatalog | null,
): string | null {
  void catalog; // advisory picker data must never gate an explicit value
  if (!draft.model || draft.model === MODEL_DEFAULT) return null;
  return draft.model;
}

/** The effort token to send (`effort` field), or null for the vendor default.
 * Pure pass-through with no ccteam-side remapping or advisory-catalog gate;
 * the adapter must reject or confirm the explicit value with the vendor. */
export function effortSwitchFor(
  draft: Pick<ComposerDraft, "vendor" | "effort">,
  catalog?: VendorCatalog | null,
): string | null {
  void catalog; // advisory picker data must never gate an explicit value
  if (!draft.effort) return null;
  return draft.effort;
}

/** Reset vendor-owned axes only at the moment the user actually changes
 * vendor. Model catalogs are advisory, so ordinary normalization must never
 * erase an explicit value merely because the daemon has not observed it. */
export function switchDraftVendor(draft: ComposerDraft, vendor: string): ComposerDraft {
  const spec = vendorSpec(vendor);
  if (draft.vendor === spec.id) return normalizeDraft(draft);
  return {
    vendor: spec.id,
    model: MODEL_DEFAULT,
    effort: "",
    protocol: spec.protocols[0]!.id,
    hitl: !!draft.hitl,
  };
}

/** Apply one picker model choice. Per-model metadata may clear a menu-picked
 * effort that the newly selected model explicitly does not support; absent
 * metadata leaves the value alone for adapter-side validation. */
export function selectDraftModel(
  draft: ComposerDraft,
  vendor: string,
  model: string,
  catalog?: VendorCatalog | null,
): ComposerDraft {
  const next = switchDraftVendor(draft, vendor);
  const modelEntry = catalog?.[next.vendor]?.models.find((entry) => entry.id === model);
  return {
    ...next,
    model,
    effort:
      modelEntry?.efforts !== undefined && !["", ...modelEntry.efforts].includes(next.effort)
        ? ""
        : next.effort,
  };
}

/** Normalize structural draft state without treating the advisory catalog as
 * a whitelist. Protocol remains registry-owned; explicit model and effort
 * strings survive until the adapter validates them. Actual vendor changes go
 * through {@link switchDraftVendor}, which resets vendor-owned axes once.
 *
 *  The result is built field-by-field rather than spread over `draft` on
 *  purpose: a draft persisted by an older SPA carries the retired `effortKey`,
 *  and rebuilding drops it instead of round-tripping it back into
 *  localStorage. Such a draft degrades to the vendor default (`effort: ""`),
 *  never to a crash. */
export function normalizeDraft(draft: ComposerDraft, catalog?: VendorCatalog | null): ComposerDraft {
  void catalog; // callers may have it, but it is not a validity authority
  const spec = vendorSpec(draft.vendor);
  return {
    vendor: spec.id,
    model: typeof draft.model === "string" && draft.model ? draft.model : MODEL_DEFAULT,
    effort: typeof draft.effort === "string" ? draft.effort : "",
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
