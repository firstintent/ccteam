// v0.9.15 — DSH (DeepSeek Harness) as a first-class ccteam-web page. The shell
// route `/dsh` mounts this; it embeds the identity's `dsh web` instance through
// the daemon's DSH companion port (same ccteam auth, per-tenant isolation via
// DSH_HOME — the SPA never reaches `dsh web` directly, redline §四). The page
// is a thin lifecycle skin over `GET/POST /api/v1/dsh/*`: a status head
// (state dot · meta chips · start/stop/restart · operator "open native ↗")
// with stopped/starting/disabled/error empty states.
//
// v0.10.2 (WEB-DSH-1) — keep-alive split: the <iframe> itself moved to
// DshFrameHost (rendered persistently by ChatConsole, hidden off-route), so
// navigating away no longer re-boots the DSH SPA. This view keeps the head +
// empty states and renders in place; the shared status lives in dshStore (one
// source, one starting-poll) — mounting here marks the page `visited`, which
// is what gates ALL dsh traffic (zero requests for users who never open it).

import { useCallback, useEffect, useMemo, useState } from "react";
import { Blocks, ExternalLink, Play, RotateCw, Square, TriangleAlert } from "lucide-react";
import {
  embedSrc,
  isDisabled,
  nativeHref,
  startDsh,
  stopDsh,
  type DshStatus,
} from "../lib/dshApi";
import { dshStore, useDshStatus } from "../hooks/dshStore";
import { makeT, type Lang } from "../lib/i18n";

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
  // One shared status source (dshStore): this head, the empty states, and the
  // persistent DshFrameHost iframe all read it — no duplicate polling.
  const { status, loading, fetchError } = useDshStatus();
  const [busy, setBusy] = useState(false);
  // The persistent host derives the same src; a serving instance yields a
  // stable string, so the keep-alive <iframe> is never reloaded by revisits.
  const src = embedSrc(status);

  // Mounting DshView == the user opened /dsh: flip the lazy `visited` gate and
  // (re)validate the status. A revisit revalidates in the background while the
  // store's last status keeps the head + iframe on screen immediately.
  useEffect(() => {
    dshStore.visit();
  }, []);

  const runAction = useCallback(async (action: () => Promise<DshStatus>) => {
    setBusy(true);
    try {
      await dshStore.runAction(action);
    } finally {
      setBusy(false);
    }
  }, []);

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
    // When the instance is serving, the iframe is the persistent DshFrameHost
    // sibling below this view — so this collapses to the head (dsh-view--flat)
    // and renders NO stage of its own. When there is no embed src, the stage
    // here carries the loading/empty/error states exactly as before.
    <div className={src ? "dsh-view dsh-view--flat" : "dsh-view"} data-testid="dsh-view">
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

      {src ? null : (
        <div className="dsh-stage" data-testid="dsh-stage">
          <DshEmpty
            lang={lang}
            status={status}
            loading={loading}
            fetchError={fetchError}
            disabled={disabled}
            busy={busy}
            onStart={onStart}
            onRetry={() => void dshStore.refresh()}
          />
        </div>
      )}
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
