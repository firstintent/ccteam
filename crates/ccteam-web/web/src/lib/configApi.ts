// v0.8.8 F4 — REST client for the IM credential config surface
// (`/api/v1/config/im/...`), driving the Settings page (Telegram + Lark).
//
// Backend SoT: `crates/ccteam-web/src/routes/im_config.rs`. Every endpoint
// sits behind the same web-token gate as the rest of the resource API.
//
// 红线(red line): the read shape NEVER carries a plaintext secret — the
// server `ImConfigStatus` has no `bot_token` / `app_secret` field at all,
// only last-4 fingerprints + counts. {@link ImConfigStatus} mirrors that
// exactly: omitting the secret fields here is a second (type-level) guard
// on top of the server's.
//
// Auth: plain same-origin `fetch`; the global `fetchInterceptor`
// monkey-patch attaches `Authorization: Bearer <token>` and we keep
// `credentials: "same-origin"` so the cookie rides along. Error mapping
// mirrors `sessionsApi`:
//   401 → throw Error("UNAUTHENTICATED")  (global TokenEntryGate kicks in)
//   non-2xx → throw Error(<server {error} text> || "HTTP <status>")
// Unlike `sessionsApi`, the PUT validators return a human `{error}` on 400
// (e.g. "Telegram token rejected: ..."), so we surface that text rather
// than a bare "HTTP 400".

/** Masked Telegram status (`im_config::TelegramStatus`). No token field. */
export interface TelegramStatus {
  /** Always `true` when present (a block exists on disk). */
  configured: boolean;
  /** Last-4 fingerprint of the bot token (`…wxyz`), never the token. */
  bot_token_last4: string;
  /** How many `chat_id`s are bound (the allowlist length). */
  chat_id_count: number;
}

/** Masked Lark/Feishu status (`im_config::LarkStatus`). No app_secret field. */
export interface LarkStatus {
  /** Always `true` when present. */
  configured: boolean;
  /** Last-4 fingerprint of the app id (`…wxyz`). */
  app_id_last4: string;
  /** `true` = Feishu (CN), `false` = Lark international. */
  use_feishu: boolean;
  /** How many `open_id`s are allowlisted. */
  allowed_user_id_count: number;
}

/** `GET /api/v1/config/im` response — masked, secret-free
 *  (`im_config::ImConfigStatus`). `telegram`/`lark` are `null` when the
 *  corresponding block is absent on disk. */
export interface ImConfigStatus {
  telegram: TelegramStatus | null;
  lark: LarkStatus | null;
  /** Cleartext-on-LAN caveat (no TLS) — surfaced as a warning banner. */
  transport_warning: string;
}

/** `PUT /config/im/telegram` success body. */
export interface TelegramSaveResult {
  ok: boolean;
  restart_required: boolean;
  /** The bot's `@username` from `getMe` (validation echo, not a secret). */
  bot_username: string;
  note: string;
}

/** `POST /config/im/telegram/chat-id/start` success body. */
export interface ChatIdStartResult {
  started: boolean;
  poll_seconds: number;
}

/** Async `chat_id` capture states (`im_config` GET poll). `idle` before a
 *  start, `pending` while long-polling, then a terminal state. `captured`
 *  may carry a `warning` when the token was cleared mid-capture. */
export type ChatIdPollStatus =
  | "idle"
  | "pending"
  | "captured"
  | "timeout"
  | "error";

/** `GET /config/im/telegram/chat-id` poll body. */
export interface ChatIdPollResult {
  status: ChatIdPollStatus;
  /** Present on `captured` — last-4 fingerprint (`…wxyz`), never the id. */
  chat_id_last4?: string;
  /** Present on `captured` (set the token first, restart to apply). */
  restart_required?: boolean;
  note?: string;
  /** Present on `error`. */
  error?: string;
  /** Present on `captured` when no token was on disk to persist into. */
  warning?: string;
}

/** `PUT /config/im/lark` request body. */
export interface LarkSaveInput {
  app_id: string;
  app_secret: string;
  /** `open_id` (`ou_...`) allowlist — empty = fail-closed (no one). */
  allowed_user_ids: string[];
  /** `true` = Feishu (CN), `false` = Lark international. */
  use_feishu: boolean;
}

/** `PUT /config/im/lark` success body. */
export interface LarkSaveResult {
  ok: boolean;
  restart_required: boolean;
  note: string;
}

/** Read a server `{error: "..."}` body, falling back to `HTTP <status>`
 *  when the response has no parseable error string. Used by the mutating
 *  helpers so a 400 surfaces the validator's human reason. */
async function errorTextOf(res: Response): Promise<string> {
  try {
    const body = (await res.json()) as { error?: unknown };
    if (body && typeof body.error === "string" && body.error.length > 0) {
      return body.error;
    }
  } catch {
    // Non-JSON / empty body — fall through to the status code.
  }
  return `HTTP ${res.status}`;
}

async function getJson<T>(url: string): Promise<T> {
  let res: Response;
  try {
    res = await fetch(url, {
      headers: { Accept: "application/json" },
      credentials: "same-origin",
    });
  } catch (e) {
    throw new Error(
      `network: ${e instanceof Error ? e.message : "connection failed"}`,
    );
  }
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (!res.ok) throw new Error(await errorTextOf(res));
  return (await res.json()) as T;
}

async function postJson<T>(url: string, body: unknown): Promise<T> {
  let res: Response;
  try {
    res = await fetch(url, {
      method: "POST",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: JSON.stringify(body),
    });
  } catch (e) {
    throw new Error(
      `network: ${e instanceof Error ? e.message : "connection failed"}`,
    );
  }
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (!res.ok) throw new Error(await errorTextOf(res));
  return (await res.json()) as T;
}

async function putJson<T>(url: string, body: unknown): Promise<T> {
  let res: Response;
  try {
    res = await fetch(url, {
      method: "PUT",
      credentials: "same-origin",
      headers: { "Content-Type": "application/json", Accept: "application/json" },
      body: JSON.stringify(body),
    });
  } catch (e) {
    throw new Error(
      `network: ${e instanceof Error ? e.message : "connection failed"}`,
    );
  }
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (!res.ok) throw new Error(await errorTextOf(res));
  return (await res.json()) as T;
}

/** `GET /api/v1/config/im` — masked IM credential status. */
export function getImConfig(): Promise<ImConfigStatus> {
  return getJson<ImConfigStatus>("/api/v1/config/im");
}

/** `PUT /api/v1/config/im/telegram` — validate (`getMe`) + persist the bot
 *  token. 400 surfaces the validator's reason (e.g. token rejected). */
export function saveTelegramToken(bot_token: string): Promise<TelegramSaveResult> {
  return putJson<TelegramSaveResult>("/api/v1/config/im/telegram", { bot_token });
}

/** `POST /api/v1/config/im/telegram/chat-id/start` — begin the background
 *  long-poll for the owner's `chat_id` (requires a token already on disk). */
export function startTelegramChatId(): Promise<ChatIdStartResult> {
  return postJson<ChatIdStartResult>(
    "/api/v1/config/im/telegram/chat-id/start",
    {},
  );
}

/** `GET /api/v1/config/im/telegram/chat-id` — poll the async capture state. */
export function pollTelegramChatId(): Promise<ChatIdPollResult> {
  return getJson<ChatIdPollResult>("/api/v1/config/im/telegram/chat-id");
}

/** `PUT /api/v1/config/im/lark` — validate (`tenant_access_token`) + persist
 *  the Lark/Feishu app credentials. 400 surfaces the validator's reason. */
export function saveLark(input: LarkSaveInput): Promise<LarkSaveResult> {
  return putJson<LarkSaveResult>("/api/v1/config/im/lark", input);
}
