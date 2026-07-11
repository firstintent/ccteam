// v0.8.24 Track A — the 4-way vendor registry driving the composer's
// model+effort+protocol menu (prototype `VENDORS`), extended with opencode
// (the prototype predates the 4th vendor; owner call: never collapse opencode
// into codex/grok colors).
//
// Dependency-free + pure so the menu structure / gating is unit-testable
// without the React import chain.

export type VendorId = "claude" | "codex" | "grok" | "opencode";

/** One selectable wire protocol for a vendor. `wire` is the value POSTed to
 *  `POST /projects/{slug}/sessions` (`protocol` field); `label` is what the
 *  menu shows (codex's app-server IS the stream-json wire value). */
export interface ProtocolOption {
  id: string;
  label: string;
  /** Menu sub-caption (transport hint). */
  sub: string;
  wire: "stream-json" | "terminal" | "acp";
  /** Frozen/beta surfaces (claude terminal) are admin-only in the UI. */
  adminOnly?: boolean;
}

export interface VendorSpec {
  id: VendorId;
  label: string;
  /** Model choices shown in the composer menu. The FIRST entry is the vendor
   *  default: picking it sends no `/model` switch. */
  models: string[];
  protocols: ProtocolOption[];
}

export const VENDORS: VendorSpec[] = [
  {
    id: "claude",
    label: "claude",
    models: ["fable-5", "opus-4.8", "sonnet-5", "haiku-4.5"],
    protocols: [
      { id: "stream-json", label: "stream-json", sub: "NDJSON", wire: "stream-json" },
      { id: "terminal", label: "terminal", sub: "tmux", wire: "terminal", adminOnly: true },
    ],
  },
  {
    id: "codex",
    label: "codex",
    models: ["gpt-5.2-codex", "gpt-5.1"],
    protocols: [
      { id: "app-server", label: "app-server", sub: "JSON-RPC", wire: "stream-json" },
    ],
  },
  {
    id: "grok",
    label: "grok",
    models: ["grok-4", "grok-code"],
    protocols: [{ id: "acp", label: "acp", sub: "JSON-RPC stdio", wire: "acp" }],
  },
  {
    id: "opencode",
    label: "opencode",
    // OpenCode is provider-agnostic and picks its own initial model
    // (session/new carries no model) — offer only the honest default.
    models: ["默认 (opencode 自选)"],
    protocols: [{ id: "acp", label: "acp", sub: "JSON-RPC stdio", wire: "acp" }],
  },
];

export function vendorSpec(id: string): VendorSpec {
  return VENDORS.find((v) => v.id === id) ?? VENDORS[0]!;
}

/** Effort levels. `effDefault` (first) wires nothing — the vendor's own
 *  default holds; an explicit pick rides the create form's `effort` field
 *  (see {@link wireEffort} for the per-vendor token map). */
export const EFFORT_KEYS = ["effDefault", "effLow", "effMid", "effHigh", "effMax"] as const;
export type EffortKey = (typeof EFFORT_KEYS)[number];

/** Per-vendor effort token for `POST .../sessions` (`effort` field), or null
 *  when nothing should be sent:
 *  - `effDefault` → null for every vendor (vendor default, honest);
 *  - claude: `low|medium|high|max` (verified `--effort` levels);
 *  - codex: `low|medium|high|xhigh` (its `ReasoningEffort` set has no `max`);
 *  - grok: never (its `--reasoning-effort` value set is undocumented — the
 *    backend drops it too);
 *  - opencode: never from the UI (effort values are per-model "variants" we
 *    cannot enumerate; the adapter seam exists for API callers). */
export function wireEffort(draft: Pick<ComposerDraft, "vendor" | "effortKey">): string | null {
  if (draft.effortKey === "effDefault") return null;
  const claude: Record<string, string> = {
    effLow: "low",
    effMid: "medium",
    effHigh: "high",
    effMax: "max",
  };
  const codex: Record<string, string> = { ...claude, effMax: "xhigh" };
  switch (draft.vendor) {
    case "claude":
      return claude[draft.effortKey] ?? null;
    case "codex":
      return codex[draft.effortKey] ?? null;
    default:
      return null;
  }
}

/** Protocols a caller may pick for `vendor` — the claude `terminal` protocol
 *  is frozen (maintenance-only) and admin-gated in the UI (beta-gate; not a
 *  security boundary — the backend route is unchanged). */
export function visibleProtocols(vendor: string, isAdmin: boolean): ProtocolOption[] {
  return vendorSpec(vendor).protocols.filter((p) => isAdmin || !p.adminOnly);
}

/** The composer draft — what the Home lazy-create POSTs from. */
export interface ComposerDraft {
  vendor: VendorId;
  model: string;
  effortKey: EffortKey;
  /** Protocol option id (menu row), NOT the wire value. */
  protocol: string;
  hitl: boolean;
}

export function defaultDraft(): ComposerDraft {
  return {
    vendor: "claude",
    model: VENDORS[0]!.models[0]!,
    // Default = vendor default (wires nothing); the old "effMax" default
    // displayed 极高 while sending nothing — dishonest either way once the
    // effort field is really wired.
    effortKey: "effDefault",
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
 *  `set_config_option`). Opencode stays null — it self-selects. */
export function modelSwitchFor(draft: Pick<ComposerDraft, "vendor" | "model">): string | null {
  const spec = vendorSpec(draft.vendor);
  if (spec.id === "opencode") return null; // opencode self-selects
  if (!draft.model || draft.model === spec.models[0]!) return null;
  return spec.models.includes(draft.model) ? draft.model : null;
}

/** After a vendor/model pick, the protocol must remain one this vendor offers. */
export function normalizeDraft(draft: ComposerDraft): ComposerDraft {
  const spec = vendorSpec(draft.vendor);
  const model = spec.models.includes(draft.model) ? draft.model : spec.models[0]!;
  const protocol = spec.protocols.some((p) => p.id === draft.protocol)
    ? draft.protocol
    : spec.protocols[0]!.id;
  return { ...draft, model, protocol };
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
