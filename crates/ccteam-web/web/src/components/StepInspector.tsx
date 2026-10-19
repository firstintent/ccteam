// Step inspector — the second layer of the web activity view. The folded
// activity row expands into its step list (layer one, in the transcript);
// picking a step opens this panel with the step in FULL: a tool call's input
// and output, or a reasoning block's text. The full record is read on demand
// from the session's step log (`GET /api/v1/sessions/{sid}/steps/{step_id}`),
// so megabytes of tool output never ride the SSE stream or localStorage.
// Wide screens dock it as a right column; narrow ones overlay it (CSS).

import { useEffect, useState } from "react";
import { X } from "lucide-react";
import { makeT, type Lang } from "../lib/i18n";
import { getStep } from "../lib/sessionsApi";
import { failedLoad, initialLoad, shouldRetry, STEP_RETRY_MS, type StepLoad } from "../lib/stepLoad";
import { stepIcon, type FoldStep } from "../pages/chatTranscript";

export function StepInspector({
  sid,
  step,
  lang,
  onClose,
}: {
  sid: string;
  step: FoldStep;
  lang: Lang;
  onClose: () => void;
}) {
  const t = makeT(lang);
  // The fetched result is kept with the key it answers, so a step switch or
  // a running step finishing reads as "loading" until its own answer lands —
  // derived, never reset from inside the effect. A step the server has not
  // recorded yet is asked for again every few seconds while the panel is
  // open: the local list can be stale (a reload mid-tool missed the
  // completion frame), so the server's answer is the one that counts.
  const key = `${sid}|${step.step_id}|${step.done}`;
  const early = initialLoad(step);
  const [fetched, setFetched] = useState<{ key: string; load: StepLoad } | null>(null);
  useEffect(() => {
    if (!step.step_id) return;
    const ctrl = new AbortController();
    let timer: ReturnType<typeof setTimeout> | undefined;
    const ask = () => {
      getStep(sid, step.step_id, { signal: ctrl.signal })
        .then((record) => setFetched({ key, load: { state: "ready", record } }))
        .catch((err: unknown) => {
          if (ctrl.signal.aborted) return;
          const load = failedLoad(err, step.done);
          setFetched({ key, load });
          if (shouldRetry(load)) timer = setTimeout(ask, STEP_RETRY_MS);
        });
    };
    ask();
    return () => {
      ctrl.abort();
      if (timer) clearTimeout(timer);
    };
  }, [key, sid, step.step_id, step.done]);
  const load: StepLoad =
    early ?? (fetched?.key === key ? fetched.load : { state: "loading" });

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const thinking = step.kind === "thinking";
  const title = thinking ? t("stepThinking") : step.name || step.kind;

  return (
    <aside className="step-inspector" data-testid="step-inspector" aria-label={t("stepInspector")}>
      <header className="step-inspector-head">
        <span className="step-icon" aria-hidden>
          {stepIcon(step)}
        </span>
        <span className="step-title">{title}</span>
        <span className={`step-state ${step.done ? "done" : "running"}`}>
          {step.done ? "✓" : "…"}
        </span>
        <button
          type="button"
          className="icon-btn sm"
          data-testid="step-inspector-close"
          aria-label={t("close")}
          title={t("close")}
          onClick={onClose}
        >
          <X />
        </button>
      </header>
      <div className="step-inspector-body">
        {load.state === "ready" ? (
          <>
            {load.record.truncated ? <p className="step-note">{t("stepTruncated")}</p> : null}
            {load.record.input ? (
              <section>
                <h3>{t("stepInput")}</h3>
                <pre data-testid="step-input">{load.record.input}</pre>
              </section>
            ) : null}
            <section>
              <h3>{thinking ? t("stepThinking") : t("stepOutput")}</h3>
              <pre data-testid="step-output" className={thinking ? "thinking" : undefined}>
                {load.record.output || t("stepNoOutput")}
              </pre>
            </section>
          </>
        ) : (
          <>
            <section>
              <h3>{thinking ? t("stepThinking") : t("stepInput")}</h3>
              <pre className={thinking ? "thinking" : undefined}>{step.summary}</pre>
            </section>
            <p className="step-note" data-testid="step-state">
              {load.state === "loading"
                ? t("stepLoading")
                : load.state === "running"
                  ? t("stepRunning")
                  : load.state === "unavailable"
                    ? t("stepUnavailable")
                    : load.message}
            </p>
          </>
        )}
      </div>
    </aside>
  );
}
