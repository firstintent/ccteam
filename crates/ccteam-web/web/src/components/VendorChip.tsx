import { vendorChipClass, vendorSpec } from "../lib/vendors";

/** Compact vendor mark (replaces the text label) so session rows / cards
 *  spend less horizontal space. Color still comes from `.chip.{vendor}`. */
function VendorMark({ vendor }: { vendor: string }) {
  const id = vendorSpec(vendor).id;
  // All marks share a 16×16 viewBox and `currentColor` so the chip's CSS
  // color drives the brand accent without per-vendor fill hacks.
  switch (id) {
    case "claude":
      // Anthropic-style starburst (Claude).
      return (
        <svg viewBox="0 0 16 16" aria-hidden className="vendor-ico">
          <path
            fill="currentColor"
            d="M8 1.2 9.4 5.4 13.8 6 10.5 8.9l1 4.3L8 11.1 4.5 13.2l1-4.3L2.2 6l4.4-.6L8 1.2z"
          />
        </svg>
      );
    case "codex":
      // OpenAI-ish hex knot (Codex).
      return (
        <svg viewBox="0 0 16 16" aria-hidden className="vendor-ico">
          <path
            fill="currentColor"
            d="M8 1.5 13 4.4v7.2L8 14.5 3 11.6V4.4L8 1.5zm0 2.2L5 5.4v5.2l3 1.7 3-1.7V5.4L8 3.7z"
          />
        </svg>
      );
    case "grok":
      // xAI-ish X mark (Grok).
      return (
        <svg viewBox="0 0 16 16" aria-hidden className="vendor-ico">
          <path
            fill="currentColor"
            d="M3.2 2.5h2.4L8 6.2l2.4-3.7h2.4L9.4 8l3.6 5.5h-2.4L8 9.8l-2.6 3.7H3.2L6.6 8 3.2 2.5z"
          />
        </svg>
      );
    case "opencode":
      // Code brackets (OpenCode).
      return (
        <svg viewBox="0 0 16 16" aria-hidden className="vendor-ico">
          <path
            fill="currentColor"
            d="M5.6 3.1 2 8l3.6 4.9h1.9L3.7 8 7.5 3.1H5.6zm4.8 0h1.9L16 8l-3.7 4.9h-1.9L14.3 8 10.4 3.1z"
          />
        </svg>
      );
    case "kimi":
      // Crescent moon (Kimi).
      return (
        <svg viewBox="0 0 16 16" aria-hidden className="vendor-ico">
          <path
            fill="currentColor"
            d="M10.2 2.2a6.4 6.4 0 1 0 3.4 11.4 5.2 5.2 0 1 1-3.4-11.4z"
          />
        </svg>
      );
    case "pi":
      // Pi's own mathematical mark; deliberately not another code/moon icon.
      return (
        <svg viewBox="0 0 16 16" aria-hidden className="vendor-ico">
          <path fill="currentColor" d="M2 3.2h12v2H11v8H8.9v-8H6.8v8H4.7v-8H2v-2z" />
        </svg>
      );
    case "dsh":
      // DeepSeek Harness: compact D mark, matching the icon-only chip pattern.
      return (
        <svg viewBox="0 0 16 16" aria-hidden className="vendor-ico">
          <path
            fill="currentColor"
            d="M3 2h5.1C11 2 13 4.4 13 8s-2 6-4.9 6H3V2zm2.2 2.1v7.8H8c1.7 0 2.8-1.5 2.8-3.9S9.7 4.1 8 4.1H5.2z"
          />
        </svg>
      );
    default:
      return (
        <svg viewBox="0 0 16 16" aria-hidden className="vendor-ico">
          <circle cx="8" cy="8" r="5" fill="currentColor" />
        </svg>
      );
  }
}

/** Shared compact harness badge. Shows a small brand icon (not the vendor
 *  name string) so dense lists (sidebar / conv head) stay
 *  short. `title` + `aria-label` keep the vendor name available on hover
 *  and for screen readers. */
export function VendorChip({ vendor }: { vendor: string }) {
  const label = vendorSpec(vendor).label;
  return (
    <span
      className={`${vendorChipClass(vendor)} vendor-chip`}
      data-vendor={vendor}
      title={label}
      aria-label={label}
      role="img"
    >
      <VendorMark vendor={vendor} />
    </span>
  );
}
