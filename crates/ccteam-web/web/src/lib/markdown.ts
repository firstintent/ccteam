// v0.8.19 W1 — chat markdown renderer. The assistant emits Markdown; the
// transcript used to show it raw ({row.content} as a plain string). Render it
// with marked, then DOMPurify-sanitize before it reaches
// dangerouslySetInnerHTML — the SAME defense-in-depth MarketplaceView's
// renderBody uses (marked@18 does NOT sanitize on its own).
//
// `breaks: true` so a single newline in an agent reply becomes a <br> (chat
// convention), unlike the GFM-doc default used for hub READMEs.

import { marked } from "marked";
import DOMPurify from "dompurify";
import type { UponSanitizeAttributeHook } from "dompurify";

/** Chat links may use ordinary relative URLs or a small set of explicit
 * schemes. `data:`/`blob:` and script-like schemes never qualify. */
export const CHAT_ALLOWED_URI_REGEXP =
  /^(?!data:)(?:(?:(?:f|ht)tps?|mailto|tel):|[^a-z]|[a-z+.-]+(?:[^a-z+.-:]|$))/i;

const URI_ATTRIBUTES = new Set([
  "href",
  "xlink:href",
  "src",
  "srcset",
  "action",
  "formaction",
  "poster",
]);

function isUriWhitespace(codePoint: number): boolean {
  return (
    codePoint <= 0x20 ||
    codePoint === 0x00a0 ||
    codePoint === 0x1680 ||
    codePoint === 0x180e ||
    (codePoint >= 0x2000 && codePoint <= 0x2029) ||
    codePoint === 0x205f ||
    codePoint === 0x3000
  );
}

const stripUnsafeUri: UponSanitizeAttributeHook = (_node, hook) => {
  if (!URI_ATTRIBUTES.has(hook.attrName.toLowerCase())) return;
  const normalized = Array.from(hook.attrValue)
    .filter((char) => !isUriWhitespace(char.codePointAt(0) ?? 0))
    .join("");
  // DOMPurify's default DATA_URI_TAGS separately permits data: on `<img>`
  // even when ALLOWED_URI_REGEXP rejects it. The hook closes that explicit
  // exception while the config below governs every ordinary URI.
  if (normalized.toLowerCase().includes("data:") || !CHAT_ALLOWED_URI_REGEXP.test(normalized)) {
    hook.keepAttr = false;
  }
};

function escapeHtml(text: string): string {
  return text
    .replaceAll("&", "&amp;")
    .replaceAll("<", "&lt;")
    .replaceAll(">", "&gt;")
    .replaceAll('"', "&quot;")
    .replaceAll("'", "&#39;");
}

/** Render chat Markdown → sanitized HTML (synchronous). Safe to feed to
 *  dangerouslySetInnerHTML. */
export function renderMarkdown(md: string): string {
  const html = marked.parse(md ?? "", { async: false, gfm: true, breaks: true }) as string;
  if (typeof DOMPurify.sanitize !== "function") return escapeHtml(md ?? "");
  DOMPurify.addHook("uponSanitizeAttribute", stripUnsafeUri);
  try {
    return DOMPurify.sanitize(html, { ALLOWED_URI_REGEXP: CHAT_ALLOWED_URI_REGEXP });
  } finally {
    DOMPurify.removeHook("uponSanitizeAttribute", stripUnsafeUri);
  }
}
