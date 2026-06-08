// v0.8.8 bug — the project list (sidebar groups + new-session modal dropdown)
// must be ALL config.yaml-registered projects (the GET /api/v1/projects SoT)
// UNIONed with the projects that have a live session — NOT derived purely from
// sessions. Before the fix a project created via CLI `ccteam init` (registered,
// no session yet) was invisible, so its FIRST session could never be created
// from the web (chicken-and-egg: no session → not listed → can't make one).
//
// Kept in a dependency-free module (mirroring `chatDefaults.ts`) so the union
// is the single source of truth AND unit-testable without pulling the
// React/`window`-touching ChatConsole import chain — and so ChatConsole stays
// a components-only export (react-refresh/only-export-components).

/** Merge the registered-project slugs with the projects that have a live
 *  session into one sorted, de-duplicated list. `registered` is the config.yaml
 *  SoT (GET /api/v1/projects); `sessions` are the fanned-out live sessions
 *  (only their `project` slug matters here). A registered project with NO
 *  session still appears — that is the whole point of the union. */
export function mergeProjectSlugs(
  registered: readonly string[],
  sessions: readonly { project: string }[],
): string[] {
  return Array.from(new Set([...registered, ...sessions.map((s) => s.project)])).sort();
}
