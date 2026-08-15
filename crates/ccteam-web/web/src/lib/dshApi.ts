// v0.9.15 — DSH web first-class page. `GET /api/v1/dsh/status` reports this
// identity's DSH web instance (operator = the real ~/.dsh; tenant = an
// isolated per-tenant DSH_HOME), and `start`/`stop` drive its lifecycle. The
// page itself embeds the instance through the daemon's companion port
// (`companion_port`), which reuses the same ccteam auth — the SPA never talks
// to `dsh web` directly. Backend SoT: routes/dsh.rs.

import { httpError } from "./httpError";

export type DshState = "disabled" | "stopped" | "starting" | "running" | "attached";

export interface DshStatus {
  /** Lifecycle of THIS identity's instance. `attached` = proxying a DSH web
   *  the operator already started by hand (never a second writer on one home);
   *  `disabled` = the daemon was launched with `--dsh-web-bind off`. */
  state: DshState;
  /** The `dsh web` listen port (ephemeral) once running; absent otherwise. */
  port?: number | null;
  /** The daemon's DSH companion listener port — the iframe's real origin.
   *  Absent when the companion listener is off (`state:"disabled"`). */
  companion_port?: number | null;
  /** `own` = the operator's real ~/.dsh; `managed` = a ccteam-owned tenant home. */
  home_kind?: "own" | "managed" | null;
  dsh_version?: string | null;
  /** Tail of the child's stderr when it crashed / failed to boot. */
  error_tail?: string | null;
  /** Loopback URL of the native DSH web (operator + running/attached only) —
   *  the "open native window" affordance. Absent for tenants. */
  native_url?: string | null;
  /** Some backends signal the `off` mode with an explicit flag instead of
   *  (or alongside) `state:"disabled"` — treat either as disabled. */
  disabled?: boolean;
}

async function dshFetch(path: string, method: "GET" | "POST"): Promise<DshStatus> {
  let res: Response;
  try {
    res = await fetch(path, {
      method,
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
  } catch (e) {
    throw new Error(`network: ${e instanceof Error ? e.message : "connection failed"}`);
  }
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (!res.ok) throw await httpError(res);
  return (await res.json()) as DshStatus;
}

export const getDshStatus = (): Promise<DshStatus> => dshFetch("/api/v1/dsh/status", "GET");
export const startDsh = (): Promise<DshStatus> => dshFetch("/api/v1/dsh/start", "POST");
export const stopDsh = (): Promise<DshStatus> => dshFetch("/api/v1/dsh/stop", "POST");

/** Normalize the two possible "off" encodings the backend may use. */
export function isDisabled(status: DshStatus | null): boolean {
  return !!status && (status.state === "disabled" || status.disabled === true);
}

/** The embed origin: same scheme + hostname as the SPA, on the daemon's DSH
 *  companion port. Empty until the instance is serving. */
export function embedSrc(status: DshStatus | null): string | null {
  if (!status || (status.state !== "running" && status.state !== "attached")) return null;
  if (status.companion_port == null) return null;
  if (typeof window === "undefined") return null;
  const { protocol, hostname } = window.location;
  return `${protocol}//${hostname}:${status.companion_port}/`;
}

/** Whether a hostname is loopback — the only place a DSH-loopback-bound URL is
 *  reachable from a browser. Pure, for unit tests. */
export function isLoopbackHost(hostname: string): boolean {
  return (
    hostname === "127.0.0.1" ||
    hostname === "localhost" ||
    hostname === "::1" ||
    hostname === "[::1]"
  );
}

/** DSH binds loopback only, so the "open native window" link can only work
 *  when the browser is ON the daemon host. Hide it otherwise (never a dead
 *  button). */
export function nativeHref(status: DshStatus | null): string | null {
  if (!status?.native_url) return null;
  if (typeof window === "undefined") return null;
  return isLoopbackHost(window.location.hostname) ? status.native_url : null;
}
