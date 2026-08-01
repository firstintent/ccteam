// v0.8.24 Track A — the 5-way vendor registry driving the composer's
// model+effort+protocol menu (prototype `VENDORS`), extended with opencode
// (the prototype predates the 4th vendor) and kimi (the 5th; owner call:
// never collapse a vendor into another vendor's colors).
//
// Dependency-free + pure so the menu structure / gating is unit-testable
// without the React import chain.

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
  /** Model choices shown in the composer menu. The FIRST entry is the vendor
   *  default: picking it sends no `/model` switch. */
  models: string[];
  protocols: ProtocolOption[];
}

/** The "vendor default" menu entry — wires nothing (the CLI picks its own
 *  model). Every OTHER entry is sent verbatim to the vendor CLI, so the menu
 *  must never show a name the CLI would reject: the old catalog offered
 *  "fable-5"/"opus-4.8"/"grok-4" etc., which are neither valid aliases nor
 *  full model ids — picking them errored (or warned) at spawn. */
export const MODEL_DEFAULT = "默认";

export const VENDORS: VendorSpec[] = [
  {
    id: "claude",
    label: "claude",
    // Exactly the tokens `claude --model` documents: an alias for the latest
    // model of each family ('fable', 'opus', 'sonnet', 'haiku'). Full ids
    // (claude-fable-5) also work but the aliases track "latest" honestly.
    models: [MODEL_DEFAULT, "fable", "opus", "sonnet", "haiku"],
    protocols: [
      { id: "stream-json", label: "stream-json", sub: "NDJSON", wire: "stream-json" },
      { id: "terminal", label: "terminal", sub: "tmux", wire: "terminal" },
    ],
  },
  {
    id: "codex",
    label: "codex",
    // codex `-m` takes a free-form model id we cannot enumerate from the CLI
    // — offer only the honest default (codex's own configured model).
    models: [MODEL_DEFAULT],
    protocols: [
      { id: "app-server", label: "app-server", sub: "JSON-RPC", wire: "stream-json" },
    ],
  },
  {
    id: "grok",
    label: "grok",
    // Same as codex: `-m` is free-form and undocumented — default only.
    models: [MODEL_DEFAULT],
    protocols: [{ id: "acp", label: "acp", sub: "JSON-RPC stdio", wire: "acp" }],
  },
  {
    id: "opencode",
    label: "opencode",
    // OpenCode is provider-agnostic and picks its own initial model
    // (session/new carries no model) — offer only the honest default.
    models: [MODEL_DEFAULT],
    protocols: [{ id: "acp", label: "acp", sub: "JSON-RPC stdio", wire: "acp" }],
  },
  {
    id: "kimi",
    label: "Kimi",
    // Kimi's model catalog arrives live via ACP `availableModels` (the
    // in-session `/model` picker); `kimi acp` takes no model argv — offer
    // only the honest default.
    models: [MODEL_DEFAULT],
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
 *    cannot enumerate; the adapter seam exists for API callers);
 *  - kimi: never (thinking axis not wired this version — the backend drops
 *    it too). */
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

/** How an effort level is SHOWN, everywhere (composer pill + menu, team
 *  topology): the vendor's own token — `low` / `medium` / `high` / `max` |
 *  `xhigh`, and `default` for "wire nothing". Never translated: 高 / 极高
 *  read as ccteam vocabulary while the thing the CLI takes (and the
 *  statusline reports back) is `high` / `xhigh`, so the menu, the pill and
 *  the topology column all showed different words for one value. The label
 *  is identical in zh and en by design.
 *
 *  Vendors whose effort ccteam never wires (grok / opencode / kimi — see
 *  {@link wireEffort}) fall back to the generic ladder, since there is no
 *  vendor token to name. */
export function effortLabel(key: EffortKey, vendor?: string): string {
  if (key === "effDefault") return "default";
  const generic: Record<Exclude<EffortKey, "effDefault">, string> = {
    effLow: "low",
    effMid: "medium",
    effHigh: "high",
    effMax: "max",
  };
  return wireEffort({ vendor: (vendor ?? "claude") as VendorId, effortKey: key }) ?? generic[key];
}

/** The inverse of {@link wireEffort}: a backend effort token (from the live
 *  statusline — `GET /sessions/{sid}/status`) → the ladder key, so a live
 *  session's reported effort can select the composer's menu row.
 *  Vendor-agnostic (`xhigh` and `max` are the same top rung across
 *  codex/claude). Unknown/absent → `null` (the composer then reads
 *  `default`) — never a fake level. */
export function effortKeyOf(effort: string | null | undefined): EffortKey | null {
  switch ((effort ?? "").toLowerCase()) {
    case "low":
      return "effLow";
    case "medium":
      return "effMid";
    case "high":
      return "effHigh";
    case "max":
    case "xhigh":
      return "effMax";
    default:
      return null;
  }
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
 *  `set_config_option`, kimi `session/set_model`). Opencode stays null — it
 *  self-selects; kimi's menu is default-only (its catalog is live via ACP),
 *  so the includes-check below already holds it to null. */
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
