// V0.3.2 F55 — harness statusline snapshot panel.
//
// V0.4.0 F61 / F68: the V0.3.1 statusline-mirror pipeline
// (`~/.ccteam/harness/<slug>-<sid>.json` written by the statusline
// adapter) was retired. The Rust side (`crates/ccteam-core/harness.rs`)
// now reads Claude Code's native background-job state from
// `~/.claude/jobs/<job_id>/state.json` (`parse_cc_state_json`). The
// orchestrator periodically materializes a `HarnessSnapshot` envelope
// from that file and republishes it on the existing
// `/sse/harness/<slug>/<sid>` SSE topic — the SPA-side wire format and
// `HarnessSnapshotView` shape are unchanged, so this component does not
// need to re-target an endpoint.
//
// Why a local `useEventSource` rather than reusing F54's
// `useProgressStream`: harness snapshots arrive on a DIFFERENT SSE
// topic (`/sse/harness/<slug>/<sid>`) with a DIFFERENT event name
// (`harness_snapshot`) and a DIFFERENT payload (envelope wrapping
// `HarnessSnapshot`, not a progress.jsonl line). Forcing one hook
// to do both would couple two independent backend channels and
// leak `event` name into the hook's API.

import { useEffect, useRef, useState } from "react";
import type { HarnessSnapshotView } from "../lib/detailApi";

type Props = {
  slug: string;
  sid: string;
  /** Initial snapshot from REST (`SessionDetail.harness_snapshot`).
   *  May be null when no snapshot has been published yet — V0.4.0 F61
   *  derives this from `~/.claude/jobs/<job_id>/state.json` on the Rust
   *  side; pre-F61 builds used a statusline-mirrored file. */
  snapshot: HarnessSnapshotView | null;
};

const BACKOFF_INITIAL_MS = 1000;
const BACKOFF_MAX_MS = 30_000;

/** Locally-scoped EventSource hook for the harness_snapshot topic.
 *  Mirrors the shim's backoff + reconnect_hint handling, but for a
 *  single named SSE event with no event buffer (we keep just the
 *  latest snapshot via setState). */
function useHarnessSnapshotStream(
  slug: string,
  sid: string,
  onSnapshot: (snap: HarnessSnapshotView) => void,
): { connected: boolean; lastError: string | null } {
  const [connected, setConnected] = useState(false);
  const [lastError, setLastError] = useState<string | null>(null);
  const cbRef = useRef(onSnapshot);

  useEffect(() => {
    cbRef.current = onSnapshot;
  }, [onSnapshot]);

  useEffect(() => {
    if (!slug || !sid) return;
    const url = `/sse/harness/${encodeURIComponent(slug)}/${encodeURIComponent(sid)}`;
    let cancelled = false;
    let es: EventSource | null = null;
    let retryHandle: number | null = null;
    let backoff = BACKOFF_INITIAL_MS;

    const scheduleReconnect = () => {
      if (cancelled) return;
      try {
        es?.close();
      } catch {
        // ignore
      }
      es = null;
      const delay = backoff;
      backoff = Math.min(delay * 2, BACKOFF_MAX_MS);
      if (retryHandle != null) window.clearTimeout(retryHandle);
      retryHandle = window.setTimeout(connect, delay);
    };

    const connect = () => {
      if (cancelled) return;
      es = new EventSource(url);

      es.addEventListener("open", () => {
        setConnected(true);
        setLastError(null);
        backoff = BACKOFF_INITIAL_MS;
      });

      es.addEventListener("harness_snapshot", (raw) => {
        try {
          const data = JSON.parse((raw as MessageEvent).data);
          // Envelope shape per routes/harness_sse.rs:
          //   { slug, sid, snapshot: {...} }
          if (
            data &&
            typeof data === "object" &&
            data.snapshot &&
            typeof data.snapshot === "object"
          ) {
            cbRef.current(data.snapshot as HarnessSnapshotView);
          }
        } catch {
          // ignore malformed payloads
        }
      });

      es.addEventListener("reconnect_hint", () => {
        setLastError("server requested reconnect");
        scheduleReconnect();
      });

      es.addEventListener("error", () => {
        setConnected(false);
        setLastError("connection error");
        scheduleReconnect();
      });
    };

    connect();

    return () => {
      cancelled = true;
      if (retryHandle != null) window.clearTimeout(retryHandle);
      try {
        es?.close();
      } catch {
        // ignore
      }
      setConnected(false);
    };
  }, [slug, sid]);

  return { connected, lastError };
}

function Row({ label, value }: { label: string; value: string | null | undefined }) {
  return (
    <div className="flex justify-between items-baseline gap-3">
      <span className="text-xs uppercase tracking-wide text-text-muted">
        {label}
      </span>
      <span className="font-mono text-sm text-text-primary">
        {value && value.length > 0 ? value : "—"}
      </span>
    </div>
  );
}

export function HarnessPanel({ slug, sid, snapshot }: Props) {
  const [live, setLive] = useState<HarnessSnapshotView | null>(snapshot);

  // If the REST-provided initial snapshot changes (e.g. SessionDetail
  // refetch on slug/sid change), reset local state so we don't show
  // stale data from the previous session.
  useEffect(() => {
    let cancelled = false;
    queueMicrotask(() => {
      if (!cancelled) setLive(snapshot);
    });
    return () => {
      cancelled = true;
    };
  }, [snapshot, slug, sid]);

  const { connected, lastError } = useHarnessSnapshotStream(slug, sid, (snap) =>
    // Merge so unspecified fields from a partial snapshot don't blank
    // out previously-seen values. Backend currently sends the full
    // object every time, but defensive merge costs nothing.
    setLive((prev) => ({ ...(prev ?? {}), ...snap })),
  );

  if (!live) {
    return (
      <section className="border border-surface-700/40 rounded-md bg-surface-850 p-3">
        <header className="flex items-center justify-between mb-2">
          <h3 className="text-xs uppercase tracking-wide text-text-secondary">
            Harness snapshot
          </h3>
          <span
            className={
              "text-[10px] font-mono " +
              (connected ? "text-status-running" : "text-text-dim")
            }
            title={lastError ?? undefined}
          >
            {connected ? "live" : "off"}
          </span>
        </header>
        <p className="text-xs text-text-muted">no harness snapshot yet</p>
      </section>
    );
  }

  return (
    <section className="border border-surface-700/40 rounded-md bg-surface-850 p-3 space-y-1.5">
      <header className="flex items-center justify-between mb-1">
        <h3 className="text-xs uppercase tracking-wide text-text-secondary">
          Harness snapshot
        </h3>
        <span
          className={
            "text-[10px] font-mono " +
            (connected ? "text-status-running" : "text-text-dim")
          }
          title={lastError ?? undefined}
        >
          {connected ? "live" : "off"}
        </span>
      </header>
      <Row label="model" value={live.model} />
      <Row label="ctx %" value={live.context_used_pct} />
      <Row label="cost $" value={live.cost_usd_total} />
      <Row label="rate-limit %" value={live.rate_limit_pct} />
      <Row label="captured" value={live.captured_at} />
    </section>
  );
}
