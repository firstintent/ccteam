// v0.8.18 档1 — GET /api/v1/me: the caller's identity, so the SPA keeps user
// management + global IM credentials owner-only while opening shared and
// project-owned surfaces. Backend SoT: routes/api_v1.rs (handle_me).

import { httpError } from "./httpError";

export interface Me {
  /** `"admin"` for the owner (bootstrap token), else the tenant id. */
  id: string;
  /** `"owner"` for the admin, else the tenant's handle. */
  handle: string;
  is_admin: boolean;
}

export async function getMe(): Promise<Me> {
  let res: Response;
  try {
    res = await fetch("/api/v1/me", {
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
  } catch (e) {
    throw new Error(`network: ${e instanceof Error ? e.message : "connection failed"}`);
  }
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (!res.ok) throw await httpError(res);
  return (await res.json()) as Me;
}

/** v0.8.24 — `POST /api/v1/me/reset-token`: rotate the ADMIN web token
 *  (atomic file write + live in-memory rotate server-side). Returns the NEW
 *  wire token (`ccteam:<hex>`) — the only reveal; the caller must store it
 *  locally at once (the old token is already dead). Admin-only (403 for a
 *  tenant); 400 when auth is disabled. */
export async function resetToken(): Promise<{ wire_token: string }> {
  let res: Response;
  try {
    res = await fetch("/api/v1/me/reset-token", {
      method: "POST",
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
  } catch (e) {
    throw new Error(`network: ${e instanceof Error ? e.message : "connection failed"}`);
  }
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (!res.ok) throw await httpError(res);
  return (await res.json()) as { wire_token: string };
}
