// REST client for enrollment credentials — the "copy an MCP config an external
// agent can paste" surface.
//
// Backend SoT: `crates/ccteam-web/src/routes/enroll.rs`
//   GET    /api/v1/enroll                     → credentials the caller owns, NO secrets
//   POST   /api/v1/enroll                     → mint a MACHINE-USER credential
//   POST   /api/v1/projects/{slug}/enroll     → mint a PROJECT-scoped one
//   DELETE /api/v1/enroll/{id}                → revoke
//
// The project-scoped mint is addressed by PATH, not by a `project` field in the
// body: `/api/v1/projects/{slug}/*` is the shape the daemon's single REST ACL
// choke point (`auth::project_acl_layer`) gates, so the caller cannot reach a
// workspace it can't see, and a future project-scoped enroll route inherits the
// gate for free. Both mints return the same shape; only the bearer's scope
// differs.
//
// Auth + error mapping mirror `hostsApi` / `statusApi`:
//   401 → throw Error("UNAUTHENTICATED")  (global TokenEntryGate kicks in)
//   other non-2xx → the server's own `{error}` text via httpError()
//
// The snippets are NOT built here on purpose: their shapes are owned by the
// daemon (`ccteam_core::mcp_register`, one writer per vendor). A second copy in
// the SPA is how you ship a config that looks right and does not work — this
// file used to be exactly that bug (`transport: "http"`, a key Claude does not
// read, carrying the operator's own web login token).

import { httpError } from "./httpError";

/** One credential as listed — redacted by construction (no secret field). */
export interface EnrollCredentialView {
  id: string;
  /** `user` = this machine's user (names no project) · `project` = pinned. */
  scope: "user" | "project";
  /** The pinned workspace; absent for a user-scoped credential. */
  project?: string;
  /** ccteam identity every session this credential creates belongs to. */
  owner: string;
  label?: string;
  created_at: string;
  /** `ccteam-enroll:<id>:` — grep-able head, zero secret bytes. */
  bearer_prefix: string;
}

/** One vendor's paste-ready config. */
export interface EnrollSnippet {
  /** `claude` | `codex` | `grok` | `opencode` | `kimi`. */
  vendor: string;
  /** `json` | `toml` — how to merge it. */
  format: string;
  /** Where that vendor reads it, on the PASTING machine. */
  path: string;
  body: string;
}

/** `POST /api/v1/enroll` — the bearer is shown once and never again. */
export interface MintedEnrollment {
  credential: EnrollCredentialView;
  bearer: string;
  url: string;
  snippets: EnrollSnippet[];
  /** Credential would travel in clear text (plain HTTP, non-loopback host). */
  insecure_transport: boolean;
}

/** Body for either mint route: no scope discriminator and no project — the
 *  ROUTE says which scope, and the PATH says which project. */
export interface MintEnrollRequest {
  label?: string;
}

/** Display order: the JSON `mcpServers` family, then Codex, then the ACP pair.
 *  Vendors the daemon stops emitting simply disappear from the UI. */
export const VENDOR_ORDER = ["claude", "kimi", "codex", "grok", "opencode"];

/** Sort snippets into a stable, family-grouped order for the copy buttons. */
export function orderSnippets(snippets: EnrollSnippet[]): EnrollSnippet[] {
  const rank = (v: string) => {
    const i = VENDOR_ORDER.indexOf(v);
    return i < 0 ? VENDOR_ORDER.length : i;
  };
  return [...snippets].sort((a, b) => rank(a.vendor) - rank(b.vendor));
}

export async function listEnrollments(): Promise<EnrollCredentialView[]> {
  const res = await request("/api/v1/enroll");
  const body = (await res.json()) as { credentials?: EnrollCredentialView[] };
  return body.credentials ?? [];
}

/** `POST /api/v1/enroll` — a credential for this machine's user (no project).
 *  The label is REQUIRED here: the unlabelled machine-user slot is the daemon's
 *  own credential, so the route refuses a request that does not name a slot. */
export function mintUserEnrollment(
  req: MintEnrollRequest & { label: string },
): Promise<MintedEnrollment> {
  return postMint("/api/v1/enroll", req);
}

/** `POST /api/v1/projects/{slug}/enroll` — pinned to one workspace. */
export function mintProjectEnrollment(
  slug: string,
  req: MintEnrollRequest = {},
): Promise<MintedEnrollment> {
  return postMint(`/api/v1/projects/${encodeURIComponent(slug)}/enroll`, req);
}

async function postMint(url: string, req: MintEnrollRequest): Promise<MintedEnrollment> {
  const res = await request(url, {
    method: "POST",
    headers: { Accept: "application/json", "Content-Type": "application/json" },
    body: JSON.stringify(req),
  });
  return (await res.json()) as MintedEnrollment;
}

export async function revokeEnrollment(id: string): Promise<void> {
  await request(`/api/v1/enroll/${encodeURIComponent(id)}`, { method: "DELETE" });
}

async function request(url: string, init?: RequestInit): Promise<Response> {
  let res: Response;
  try {
    res = await fetch(url, {
      credentials: "same-origin",
      headers: { Accept: "application/json" },
      ...init,
    });
  } catch (e) {
    throw new Error(`network: ${e instanceof Error ? e.message : "connection failed"}`);
  }
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (!res.ok) throw await httpError(res);
  return res;
}
