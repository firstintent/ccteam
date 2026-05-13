// V0.3.2 F57 — WS PTY URL construction for the F56 relay.
//
// Centralised so all xterm callers share the same wss/ws selection rule
// and the same path templates. Both routes expect the
// `ccteam-pty.v1` subprotocol (declared in
// `crates/ccteam-web/src/routes/pty_ws.rs::SUBPROTOCOL`).
//
// Auth: the WS upgrade carries the same `ccteam_token` cookie as the
// rest of the SPA. We do NOT pass the token via the subprotocol list
// (F56 accepted only `["ccteam-pty.v1"]`) — the cookie shim installed
// by `auth_layer` runs before the upgrade extractor, so an unauth'd
// client gets 401 before the socket opens.

/**
 * Build the WebSocket URL for a workflow project (no session id) or a
 * flex per-session (with sid).
 *
 * @param slug — project slug. Encoded; safe for slugs containing
 *               unusual characters.
 * @param sid — flex session id. Optional. If omitted, the workflow
 *              route `/ws/<slug>/pty` is used.
 */
export function ptyUrlFor(slug: string, sid?: string): string {
  const proto = window.location.protocol === "https:" ? "wss:" : "ws:";
  const host = window.location.host;
  const path = sid
    ? `/ws/${encodeURIComponent(slug)}/${encodeURIComponent(sid)}/pty`
    : `/ws/${encodeURIComponent(slug)}/pty`;
  return `${proto}//${host}${path}`;
}

/**
 * Subprotocol the F56 backend echoes back on accept. Browsers fail the
 * upgrade if the server doesn't acknowledge a requested subprotocol,
 * so this string must match `routes::pty_ws::SUBPROTOCOL` on the
 * server side.
 */
export const PTY_SUBPROTOCOL = "ccteam-pty.v1";
