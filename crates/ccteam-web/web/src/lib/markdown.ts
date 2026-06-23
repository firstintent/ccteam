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

/** Render chat Markdown → sanitized HTML (synchronous). Safe to feed to
 *  dangerouslySetInnerHTML. */
export function renderMarkdown(md: string): string {
  const html = marked.parse(md ?? "", { async: false, gfm: true, breaks: true }) as string;
  return typeof DOMPurify.sanitize === "function" ? DOMPurify.sanitize(html) : html;
}
