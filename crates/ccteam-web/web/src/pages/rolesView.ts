// v0.8.8 F5 — pure, dependency-free helpers for the read-only Roles page.
//
// Split out of RolesPage.tsx so the component file only exports components
// (react-refresh/only-export-components) and so these can be unit-tested in
// node env without pulling in React.

/** Map a thrown fetch error message to human copy. UNAUTHENTICATED is handled
 *  by the global TokenEntryGate, so callers swallow it before calling this. */
export function humanError(msg: string): string {
  // 404 covers both an unknown project AND a role missing under a known one
  // (e.g. deleted between list + click), so keep the copy non-specific.
  if (msg === "NOT_FOUND") return "项目或角色不存在（可能已删除）";
  if (msg.startsWith("HTTP ")) return `加载失败（${msg}），可重试`;
  if (msg.startsWith("network")) return "网络错误，可重试";
  return `加载失败：${msg}`;
}

/** Render one frontmatter value for the key/value table. A scalar renders
 *  verbatim; anything structured (array / object) round-trips through JSON so
 *  the table never shows "[object Object]" (a red line — frontmatter values
 *  are NOT guaranteed to be strings). `null`/`undefined` render as an em dash. */
export function renderFrontmatterValue(v: unknown): string {
  if (v === null || v === undefined) return "—";
  if (typeof v === "string") return v;
  if (typeof v === "number" || typeof v === "boolean") return String(v);
  try {
    return JSON.stringify(v);
  } catch {
    return String(v);
  }
}
