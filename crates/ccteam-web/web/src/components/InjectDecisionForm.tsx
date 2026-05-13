// V0.3.2 F58 — `/inject_decision` write-action form.
//
// Operator picks one of the orchestrator-published decision candidates
// (absolute paths under `<project>/.ccteam/`) and writes the body text
// to it. Wraps the F52 JSON contract (`POST /api/<slug>/inject_decision`).
// Decision is project-scoped — there's no `<sid>` variant.
//
// Body convention (orchestrator side): files starting with
// `**META-AGENT DECISION**:` get treated as authoritative resolutions
// to open phase escalations. We show a warning chip when the body
// doesn't start with that marker, but submission is allowed anyway
// — the orchestrator team has occasional out-of-band uses (e.g.,
// dropping a note for the next phase to read without escalating).
//
// TODO: F58 → F55 integration: import this in pages/ProjectDetail.tsx
//       and pass `candidates` from `ProjectSummary.decision_candidates`.
//
// Test plan (JSDoc — playwright deferred per V0.3.2 PRD):
//   - InjectDecisionForm: select candidate, type body starting with
//     `**META-AGENT DECISION**:`, submit → ok=true → toast + clear
//   - InjectDecisionForm: body missing the marker → warning chip shows,
//     submit still works
//   - InjectDecisionForm: server returns 5xx → error toast, form
//     preserved (path + body)
//
import { useState } from "react";
import { DECISION_BODY_MAX, postInjectDecision } from "../lib/api";
import { toastBus } from "../lib/toastBus";

const DECISION_PREFIX = "**META-AGENT DECISION**:";

interface Props {
  slug: string;
  /** Absolute paths under `~/projects/<slug>/.ccteam/` that the
   *  orchestrator has flagged as awaiting a decision. Sourced from
   *  `ProjectSummary.decision_candidates` (api_v1.rs). */
  candidates: string[];
  onSuccess?: () => void;
}

export function InjectDecisionForm({ slug, candidates, onSuccess }: Props) {
  const [path, setPath] = useState<string>(candidates[0] ?? "");
  const [body, setBody] = useState("");
  const [pending, setPending] = useState(false);

  const trimmedBody = body.trim();
  const overCap = body.length > DECISION_BODY_MAX;
  const hasPrefix = trimmedBody.startsWith(DECISION_PREFIX);
  const canSubmit = !pending && !!path && trimmedBody.length > 0 && !overCap;

  async function handleSubmit(e: React.FormEvent) {
    e.preventDefault();
    if (!canSubmit) return;
    setPending(true);
    try {
      await postInjectDecision(slug, path, body);
      toastBus.handler?.info("Decision injected");
      setBody("");
      onSuccess?.();
    } catch (err) {
      if (err instanceof Error && err.message === "UNAUTHENTICATED") return;
      const msg = err instanceof Error ? err.message : "inject_decision failed";
      toastBus.handler?.error(msg);
    } finally {
      setPending(false);
    }
  }

  if (candidates.length === 0) {
    return (
      <div className="text-xs text-text-dim font-mono">
        No decision candidates pending for {slug}.
      </div>
    );
  }

  return (
    <form onSubmit={handleSubmit} className="flex flex-col gap-2">
      <label htmlFor={`decision-path-${slug}`} className="text-xs text-text-muted font-medium">
        Decision target
      </label>
      <select
        id={`decision-path-${slug}`}
        value={path}
        onChange={(e) => setPath(e.target.value)}
        disabled={pending}
        className="w-full px-3 py-2 bg-surface-900 border border-surface-700/60 rounded-lg text-text-primary text-sm font-mono focus:outline-none focus:ring-2 focus:ring-brand-600 focus:border-transparent disabled:opacity-50"
      >
        {candidates.map((c) => (
          <option key={c} value={c}>
            {c}
          </option>
        ))}
      </select>

      <label htmlFor={`decision-body-${slug}`} className="text-xs text-text-muted font-medium mt-2">
        Body (1..={DECISION_BODY_MAX} chars)
      </label>
      <textarea
        id={`decision-body-${slug}`}
        value={body}
        onChange={(e) => setBody(e.target.value)}
        rows={8}
        disabled={pending}
        spellCheck={false}
        placeholder={`${DECISION_PREFIX} <your resolution here>`}
        className="w-full px-3 py-2 bg-surface-900 border border-surface-700/60 rounded-lg text-text-primary text-sm font-mono placeholder:text-text-dim focus:outline-none focus:ring-2 focus:ring-brand-600 focus:border-transparent disabled:opacity-50 resize-y"
      />

      {trimmedBody.length > 0 && !hasPrefix && (
        <div className="text-xs text-status-warn font-mono bg-surface-800 border border-status-warn/30 rounded px-2 py-1">
          Warning: body does not start with{" "}
          <code className="text-status-warn">{DECISION_PREFIX}</code> — orchestrator
          may not treat this as authoritative. Submit anyway?
        </div>
      )}

      <div className="flex items-center justify-between text-xs">
        <span className={overCap ? "text-status-error font-mono" : "text-text-dim font-mono"}>
          {body.length}/{DECISION_BODY_MAX}
        </span>
        <button
          type="submit"
          disabled={!canSubmit}
          className="px-3 py-1.5 bg-brand-600 hover:bg-brand-700 text-white text-xs font-medium rounded-lg transition-colors disabled:opacity-50 disabled:cursor-not-allowed cursor-pointer"
        >
          {pending ? "Injecting..." : "Inject decision"}
        </button>
      </div>
    </form>
  );
}
