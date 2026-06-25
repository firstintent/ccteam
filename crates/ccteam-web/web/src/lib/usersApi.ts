// v0.8.18 档1 — REST client for web-first user (tenant) management
// (`GET/POST /api/v1/users` + `DELETE /api/v1/users/{id}`).
//
// Backend SoT: `crates/ccteam-web/src/routes/users.rs`. Admin-gated: every
// endpoint 403s unless the caller holds the owner (bootstrap) token. Auth +
// error mapping mirror `hostsApi`:
//   401 → throw Error("UNAUTHENTICATED")  (global TokenEntryGate kicks in)
//   403 → throw Error("FORBIDDEN")        (caller is a tenant, not the admin)
//   other non-2xx → throw Error("HTTP <status>")

/** One tenant as `GET /api/v1/users` returns it — never carries the token. */
export interface TenantView {
  id: string;
  handle: string;
  /** Linked IM chat (`"channel:chat_id"`), if any. */
  linked_chat: string | null;
  created_at: string;
}

/** `POST /api/v1/users` response — the new tenant + its one-time personal link. */
export interface CreateUserResponse {
  tenant: TenantView;
  /** Relative `/?token=ccteam:<hex>` — shown ONCE; the token is never re-listed. */
  personal_link: string;
}

/** `GET /api/v1/users` — list tenants (admin only). */
export function listUsers(): Promise<TenantView[]> {
  return getJson<TenantView[]>("/api/v1/users");
}

/** `POST /api/v1/users` — mint a tenant (admin only). Returns the one-time link. */
export function createUser(handle: string): Promise<CreateUserResponse> {
  return sendJson<CreateUserResponse>("/api/v1/users", "POST", { handle });
}

/** `DELETE /api/v1/users/{id}` — remove a tenant (admin only). */
export function deleteUser(id: string): Promise<{ removed: boolean }> {
  return sendJson<{ removed: boolean }>(
    `/api/v1/users/${encodeURIComponent(id)}`,
    "DELETE",
  );
}

/** `GET /api/v1/users/{id}/link` response — a tenant's personal login link. */
export interface UserLinkResponse {
  id: string;
  handle: string;
  /** Relative `/?token=ccteam:<hex>` the tenant opens to sign in. */
  personal_link: string;
}

/** `GET /api/v1/users/{id}/link` — re-reveal a tenant's personal login link
 *  (admin only). v0.8.20 F3: unlike the list (which never carries the token),
 *  this lets the admin re-copy any tenant's link at any time. */
export function getUserLink(id: string): Promise<UserLinkResponse> {
  return getJson<UserLinkResponse>(`/api/v1/users/${encodeURIComponent(id)}/link`);
}

/** `PUT /api/v1/me/im` body — v0.8.20 F2. REPLACE semantics: the full desired
 *  per-user IM config; a platform omitted/empty is cleared. */
export interface PutMyImForm {
  /** The tenant's own Telegram bot token. Omit/empty → no Telegram bot. */
  telegram_bot_token?: string;
  /** The tenant's own Lark/Feishu app. Omit → no Lark bot. */
  lark?: { app_id: string; app_secret: string; use_feishu?: boolean };
}

/** `PUT /api/v1/me/im` — the caller sets its OWN per-user IM bot (self-serve).
 *  The Telegram token is `getMe`-validated server-side before it is stored. */
export function putMyIm(form: PutMyImForm): Promise<{ ok: boolean }> {
  return sendJson<{ ok: boolean }>("/api/v1/me/im", "PUT", form);
}

async function getJson<T>(url: string): Promise<T> {
  let res: Response;
  try {
    res = await fetch(url, {
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
  } catch (e) {
    throw new Error(`network: ${e instanceof Error ? e.message : "connection failed"}`);
  }
  return handleResponse<T>(res);
}

async function sendJson<T>(
  url: string,
  method: "POST" | "PUT" | "DELETE",
  body?: unknown,
): Promise<T> {
  let res: Response;
  try {
    res = await fetch(url, {
      method,
      headers: body
        ? { Accept: "application/json", "Content-Type": "application/json" }
        : { Accept: "application/json" },
      credentials: "same-origin",
      body: body ? JSON.stringify(body) : undefined,
    });
  } catch (e) {
    throw new Error(`network: ${e instanceof Error ? e.message : "connection failed"}`);
  }
  return handleResponse<T>(res);
}

async function handleResponse<T>(res: Response): Promise<T> {
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (res.status === 403) throw new Error("FORBIDDEN");
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return (await res.json()) as T;
}
