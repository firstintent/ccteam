// v0.8.18 档1 — GET /api/v1/me: the caller's identity, so the SPA shows
// admin-only surfaces (Status, 主机, IM credentials, user management) only to
// the owner. Backend SoT: crates/ccteam-web/src/routes/api_v1.rs (handle_me).

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
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return (await res.json()) as Me;
}
