// Composer attachments — the SPA face of the two-step attach flow:
//
//   1. `uploadAttachment(slug, file)`  → POST /api/v1/projects/{slug}/uploads
//      (raw body; the server stores it under `<project>/.ccteam/uploads/` and
//      returns the absolute path).
//   2. the send names that path in the turn's `attachments[]`
//      (`sessionsApi.submitTurn`), and the server weaves the same
//      `[attachment …]` turn-text lines the IM path emits — every vendor
//      session already knows to `Read` them.
//
// `listProjectSkills(slug)` backs the composer's attach-skill picker
// (`GET /api/v1/projects/{slug}/skills` — the project's own skill set).
// `listLibrarySkills()` backs the picker's Global-library section
// (`GET /api/v1/skills` — the user-level `~/.ccteam/skills` library; ADMIN-only,
// the SPA only calls it for admins). Attaching a library skill never installs
// anything — the turn attachment just carries `scope:"global"` + the id.

/** Server reply for one stored upload. */
export interface UploadedAttachment {
  /** Absolute daemon-side path — echo it back in the turn's `attachments[]`. */
  path: string;
  kind: "image" | "file";
  /** Sanitized stored name. */
  name: string;
  size: number;
}

/** One installed project skill (`GET .../projects/{slug}/skills`). */
export interface SkillSummary {
  skill: string;
  description: string;
}

/** One skill in the user-level global library (`GET /api/v1/skills`,
 *  admin-only). `id` may be nested (`baoyu-skills/baoyu-comic`); `path` is the
 *  absolute SKILL.md the server weaves into the turn. */
export interface LibrarySkillSummary {
  id: string;
  description: string;
  path: string;
  source?: string;
}

/** One attachment named in a turn POST (mirror of the server `TurnAttachment`).
 *  `scope` marks a skill as coming from the global library; project skills
 *  omit it (byte-compatible with older clients). */
export interface TurnAttachment {
  kind: "image" | "file" | "skill";
  path?: string;
  name?: string;
  scope?: "project" | "global";
}

async function errorMessage(res: Response, fallback: string): Promise<string> {
  try {
    const text = await res.text();
    try {
      const msg = (JSON.parse(text) as { error?: unknown }).error;
      if (typeof msg === "string" && msg.trim()) return msg;
      return fallback;
    } catch {
      return text ? `HTTP ${res.status}: ${text.slice(0, 200)}` : fallback;
    }
  } catch {
    return fallback;
  }
}

/** Upload one picked/pasted/dropped file. Raw-body POST (no multipart);
 *  the original name rides the query string and is sanitized server-side. */
export async function uploadAttachment(
  slug: string,
  file: File,
): Promise<UploadedAttachment> {
  const url = `/api/v1/projects/${encodeURIComponent(slug)}/uploads?name=${encodeURIComponent(file.name || "upload.bin")}`;
  let res: Response;
  try {
    res = await fetch(url, {
      method: "POST",
      credentials: "same-origin",
      headers: {
        "Content-Type": file.type || "application/octet-stream",
        Accept: "application/json",
      },
      body: file,
    });
  } catch (e) {
    throw new Error(
      `network: ${e instanceof Error ? e.message : "connection failed"}`,
    );
  }
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (!res.ok) throw new Error(await errorMessage(res, `upload failed (${res.status})`));
  return (await res.json()) as UploadedAttachment;
}

/** List the project's installed skills for the composer picker. */
export async function listProjectSkills(slug: string): Promise<SkillSummary[]> {
  const url = `/api/v1/projects/${encodeURIComponent(slug)}/skills`;
  let res: Response;
  try {
    res = await fetch(url, {
      credentials: "same-origin",
      headers: { Accept: "application/json" },
    });
  } catch (e) {
    throw new Error(
      `network: ${e instanceof Error ? e.message : "connection failed"}`,
    );
  }
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (!res.ok) throw new Error(await errorMessage(res, `skills failed (${res.status})`));
  return (await res.json()) as SkillSummary[];
}

/** List the user-level global skill library (`GET /api/v1/skills`).
 *  ADMIN-only server-side — the SPA calls this only for admins (a tenant
 *  gets 403, lifted to its `{error}` message). */
export async function listLibrarySkills(): Promise<LibrarySkillSummary[]> {
  let res: Response;
  try {
    res = await fetch("/api/v1/skills", {
      credentials: "same-origin",
      headers: { Accept: "application/json" },
    });
  } catch (e) {
    throw new Error(
      `network: ${e instanceof Error ? e.message : "connection failed"}`,
    );
  }
  if (res.status === 401) throw new Error("UNAUTHENTICATED");
  if (!res.ok) throw new Error(await errorMessage(res, `skills failed (${res.status})`));
  const data = (await res.json()) as { skills?: LibrarySkillSummary[] };
  return data.skills ?? [];
}
