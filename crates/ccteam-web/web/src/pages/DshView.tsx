// v0.9.15 — DSH (DeepSeek Harness) as a first-class ccteam-web page. The shell
// route `/dsh` mounts this; it embeds the identity's `dsh web` instance through
// the daemon's DSH companion port (same ccteam auth, per-tenant isolation via
// DSH_HOME — the SPA never reaches `dsh web` directly, redline §四). The page
// is a thin lifecycle skin over `GET/POST /api/v1/dsh/*`: a status head
// (state dot · meta chips · start/stop/restart · operator "open native ↗") over
// a full-bleed <iframe>, with stopped/starting/disabled/error empty states.

import { useCallback, useEffect, useMemo, useState } from "react";
import { Blocks, ExternalLink, Play, RotateCw, Square, TriangleAlert } from "lucide-react";
import {
  embedSrc,
  getDshStatus,
  isDisabled,
  nativeHref,
  startDsh,
  stopDsh,
  type DshStatus,
} from "../lib/dshApi";
import { makeT, type Lang } from "../lib/i18n";

/** How often to re-poll while the instance is booting (`starting`). */
const STARTING_POLL_MS = 1500;

function stateDotClass(status: DshStatus | null, fetchError: boolean): string {
  if (fetchError) return "dot err";
  switch (status?.state) {
    case "running":
    case "attached":
      return "dot on";
    case "starting":
      return "dot busy";
    default:
      return "dot off";
  }
}

export default function DshView({ lang = "zh" }: { lang?: Lang }) {
  const t = makeT(lang);
  const [status, setStatus] = useState<DshStatus | null>(null);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [fetchError, setFetchError] = useState<string | null>(null);
  // Derived embed origin. It is a stable string while serving (running/attached
  // don't poll), so React keeps the same <iframe src> and never reloads the DSH
  // SPA out from under the user.
  const src = embedSrc(status);

  const refresh = useCallback(async () => {
    try {
      const next = await getDshStatus();
      setStatus(next);
      setFetchError(null);
    } catch (e) {
      if (e instanceof Error && e.message === "UNAUTHENTICATED") return;
      setFetchError(e instanceof Error ? e.message : String(e));
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    queueMicrotask(() => void refresh());
  }, [refresh]);

  // Poll only while booting — a running/attached/stopped instance is quiescent.
  useEffect(() => {
    if (status?.state !== "starting") return;
    const id = window.setInterval(() => queueMicrotask(() => void refresh()), STARTING_POLL_MS);
    return () => window.clearInterval(id);
  }, [status?.state, refresh]);

  const runAction = useCallback(
    async (action: () => Promise<DshStatus>) => {
      setBusy(true);
      try {
        const next = await action();
        setStatus(next);
        setFetchError(null);
      } catch (e) {
        if (!(e instanceof Error && e.message === "UNAUTHENTICATED")) {
          setFetchError(e instanceof Error ? e.message : String(e));
        }
      } finally {
        setBusy(false);
      }
    },
    [],
  );

  const onStart = useCallback(() => void runAction(startDsh), [runAction]);
  const onStop = useCallback(() => void runAction(stopDsh), [runAction]);
  const onRestart = useCallback(
    () => void runAction(async () => {
      await stopDsh();
      return startDsh();
    }),
    [runAction],
  );

  const native = nativeHref(status);
  const running = status?.state === "running" || status?.state === "attached";
  const disabled = isDisabled(status);

  const metaChips = useMemo(() => {
    if (!status || disabled) return null;
    const chips: React.ReactNode[] = [];
    if (status.state === "attached") {
      chips.push(
        <span key="att" className="dsh-chip attached" title={t("dshAttachedHint")}>
          {t("dshAttached")}
        </span>,
      );
    }
    if (status.home_kind) {
      chips.push(
        <span key="home" className="dsh-chip">
          {status.home_kind === "own" ? t("dshHomeOwn") : t("dshHomeManaged")}
        </span>,
      );
    }
    if (running && status.port != null) {
      chips.push(
        <span key="port" className="dsh-chip mono">
          :{status.port}
        </span>,
      );
    }
    if (status.dsh_version) {
      chips.push(
        <span key="ver" className="dsh-chip mono">
          {status.dsh_version}
        </span>,
      );
    }
    return chips;
  }, [status, disabled, running, t]);

  return (
    <div className="dsh-view" data-testid="dsh-view">
      <div className="dsh-head">
        <span className="dsh-title">
          <Blocks size={16} style={{ color: "var(--dsh)" }} />
          {t("dsh")}
        </span>
        {!loading && !disabled ? (
          <span className={stateDotClass(status, !!fetchError)} aria-hidden />
        ) : null}
        <span className="dsh-meta">{metaChips}</span>

        <div className="dsh-actions">
          {running ? (
            <>
              {native ? (
                <a
                  className="btn ghost mini"
                  href={native}
                  target="_blank"
                  rel="noreferrer"
                  data-testid="dsh-open-native"
                >
                  <ExternalLink size={14} />
                  {t("dshOpenNative")}
                </a>
              ) : null}
              <button
                type="button"
                className="btn ghost mini"
                onClick={onRestart}
                disabled={busy}
                data-testid="dsh-restart"
              >
                <RotateCw size={14} />
                {t("dshRestart")}
              </button>
              <button
                type="button"
                className="btn ghost mini"
                onClick={onStop}
                disabled={busy}
                data-testid="dsh-stop"
              >
                <Square size={13} />
                {t("dshStop")}
              </button>
            </>
          ) : status?.state === "starting" ? (
            <button
              type="button"
              className="btn ghost mini"
              onClick={onStop}
              disabled={busy}
              data-testid="dsh-cancel"
            >
              <Square size={13} />
              {t("dshStop")}
            </button>
          ) : status && !disabled ? (
            <button
              type="button"
              className="btn primary mini"
              onClick={onStart}
              disabled={busy}
              data-testid="dsh-start"
            >
              <Play size={14} />
              {t("dshStart")}
            </button>
          ) : null}
        </div>
      </div>

      <div className="dsh-stage" data-testid="dsh-stage">
        {src ? (
          <iframe
            className="dsh-frame"
            src={src}
            title="DeepSeek Harness"
            data-testid="dsh-frame"
            // DSH agents run bash under the user's approval — this is the same
            // trust level as the tenant's own chat sessions (redline §五).
            allow="clipboard-read; clipboard-write"
          />
        ) : (
          <DshEmpty
            lang={lang}
            status={status}
            loading={loading}
            fetchError={fetchError}
            disabled={disabled}
            busy={busy}
            onStart={onStart}
            onRetry={() => void refresh()}
          />
        )}
      </div>
    </div>
  );
}

function DshEmpty({
  lang,
  status,
  loading,
  fetchError,
  disabled,
  busy,
  onStart,
  onRetry,
}: {
  lang: Lang;
  status: DshStatus | null;
  loading: boolean;
  fetchError: string | null;
  disabled: boolean;
  busy: boolean;
  onStart: () => void;
  onRetry: () => void;
}) {
  const t = makeT(lang);

  if (loading) {
    return (
      <div className="dsh-empty" data-testid="dsh-empty">
        <div className="dsh-spin" aria-label={t("loading")} />
      </div>
    );
  }

  if (fetchError && !status) {
    return (
      <div className="dsh-empty" data-testid="dsh-empty">
        <span className="dsh-empty-icon">
          <TriangleAlert size={22} />
        </span>
        <h2>{t("dshErrorTitle")}</h2>
        <pre className="dsh-error-tail">{fetchError}</pre>
        <button type="button" className="btn ghost mini" onClick={onRetry}>
          {t("dshRetry")}
        </button>
      </div>
    );
  }

  if (disabled) {
    return (
      <div className="dsh-empty" data-testid="dsh-empty">
        <span className="dsh-empty-icon">
          <Blocks size={22} />
        </span>
        <h2>{t("dshDisabledTitle")}</h2>
        <p>{t("dshDisabledHint")}</p>
        <code>ccteam start --dsh-web-bind &lt;addr:port&gt;</code>
      </div>
    );
  }

  if (status?.state === "starting") {
    return (
      <div className="dsh-empty" data-testid="dsh-empty">
        <div className="dsh-spin" aria-label={t("dshStarting")} />
        <p>{t("dshStartingHint")}</p>
        {status.error_tail ? <pre className="dsh-error-tail">{status.error_tail}</pre> : null}
      </div>
    );
  }

  // stopped (with an optional crash tail from the last run)
  return (
    <div className="dsh-empty" data-testid="dsh-empty">
      <span className="dsh-empty-icon">
        <Blocks size={22} />
      </span>
      <h2>{t("dshStoppedTitle")}</h2>
      <p>{t("dshDesc")}</p>
      <button
        type="button"
        className="btn primary"
        onClick={onStart}
        disabled={busy}
        data-testid="dsh-start-empty"
      >
        <Play size={15} />
        {t("dshStart")}
      </button>
      {status?.error_tail ? <pre className="dsh-error-tail">{status.error_tail}</pre> : null}
    </div>
  );
}
