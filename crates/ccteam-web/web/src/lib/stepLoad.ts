// What the step inspector shows for a step, before and after its full record
// is fetched. Pure + DOM-free so the decisions are unit-testable.

import type { StepRecord } from "./sessionsApi";

export type StepLoad =
  | { state: "loading" }
  | { state: "ready"; record: StepRecord }
  | { state: "running" }
  | { state: "unavailable" }
  | { state: "error"; message: string };

/** How often an open panel re-asks for a step the server has not recorded
 *  yet — a live completion frame can be missed (a reload mid-tool), and a
 *  reasoning step may show before its text does. */
export const STEP_RETRY_MS = 3000;

/** A step with no id has no record anywhere: nothing to ask. `null` = ask
 *  the server (it, not the possibly stale local list, knows what finished). */
export function initialLoad(step: { step_id: string }): StepLoad | null {
  return step.step_id ? null : { state: "unavailable" };
}

/** A fetch failure as panel state. A 404 means "not recorded yet": the step
 *  is still running when the local list says so, else it has no record. */
export function failedLoad(err: unknown, done: boolean): StepLoad {
  const message = err instanceof Error ? err.message : String(err);
  if (message !== "NOT_FOUND") return { state: "error", message };
  return done ? { state: "unavailable" } : { state: "running" };
}

/** Whether to ask again later: only while the record may still appear. */
export function shouldRetry(load: StepLoad): boolean {
  return load.state === "running" || load.state === "unavailable";
}
