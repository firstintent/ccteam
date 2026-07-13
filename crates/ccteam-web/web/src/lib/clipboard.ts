// Shared clipboard helper for every 「复制」 button in the SPA.
//
// The daemon is often served over plain http:// on a remote IP (not https /
// not localhost), where `navigator.clipboard` is undefined (non-secure
// context). Callers therefore must NOT use the async Clipboard API directly —
// go through [`copyText`], which falls back to a temporary off-screen
// <textarea> + `document.execCommand("copy")`.

/**
 * Copy `text` to the clipboard. Tries the async Clipboard API first, then the
 * legacy `execCommand` path. Resolves `true` on success — callers should show
 * visible feedback on `false` (never fail silently: the user just sees a dead
 * button).
 */
export async function copyText(text: string): Promise<boolean> {
  if (typeof navigator !== "undefined" && navigator.clipboard) {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      // Permission denied / non-secure quirk — fall through to the legacy path.
    }
  }
  return legacyCopy(text);
}

/**
 * Legacy clipboard copy via a temporary off-screen <textarea> +
 * `document.execCommand("copy")`. Returns `true` on success.
 */
export function legacyCopy(text: string): boolean {
  if (typeof document === "undefined") return false;
  const ta = document.createElement("textarea");
  ta.value = text;
  // Keep it off-screen and unfocusable visually, but still selectable.
  ta.setAttribute("readonly", "");
  ta.style.position = "fixed";
  ta.style.top = "-9999px";
  ta.style.left = "-9999px";
  ta.style.opacity = "0";
  document.body.appendChild(ta);
  try {
    ta.select();
    ta.setSelectionRange(0, text.length); // iOS / mobile Safari
    return document.execCommand("copy");
  } catch {
    return false;
  } finally {
    document.body.removeChild(ta);
  }
}
