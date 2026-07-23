// Composer attachments API unit tests — mirrors the sessionsApi pattern:
// spy on `fetch`, assert URL + method + headers + body + error mapping.

import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import {
  listLibrarySkills,
  listProjectSkills,
  uploadAttachment,
  type LibrarySkillSummary,
  type SkillSummary,
  type UploadedAttachment,
} from "./attachmentsApi";

const realFetch = globalThis.fetch;

function jsonResponse(status: number, body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "content-type": "application/json" },
  });
}

beforeEach(() => {
  globalThis.fetch = vi.fn();
});

afterEach(() => {
  globalThis.fetch = realFetch;
});

describe("uploadAttachment", () => {
  it("POSTs the raw file with its name on the query + type as content-type", async () => {
    const stored: UploadedAttachment = {
      path: "/proj/.ccteam/uploads/1-shot.png",
      kind: "image",
      name: "shot.png",
      size: 3,
    };
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(201, stored));
    const file = new File(["png"], "shot.png", { type: "image/png" });
    const got = await uploadAttachment("dex ui", file);
    expect(got).toEqual(stored);
    const [url, init] = fetchMock.mock.calls[0]!;
    expect(url).toBe("/api/v1/projects/dex%20ui/uploads?name=shot.png");
    expect(init?.method).toBe("POST");
    expect((init?.headers as Record<string, string>)["Content-Type"]).toBe("image/png");
    expect(init?.body).toBe(file);
  });

  it("falls back to octet-stream + upload.bin for a typeless nameless file", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(
      jsonResponse(201, { path: "/p", kind: "file", name: "upload.bin", size: 1 }),
    );
    await uploadAttachment("demo", new File(["x"], "", { type: "" }));
    const [url, init] = fetchMock.mock.calls[0]!;
    expect(url).toBe("/api/v1/projects/demo/uploads?name=upload.bin");
    expect((init?.headers as Record<string, string>)["Content-Type"]).toBe(
      "application/octet-stream",
    );
  });

  it("maps 401 to UNAUTHENTICATED and lifts the server error body", async () => {
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(401, {}));
    await expect(uploadAttachment("demo", new File(["x"], "a"))).rejects.toThrow(
      "UNAUTHENTICATED",
    );
    fetchMock.mockResolvedValueOnce(
      jsonResponse(400, { error: "project `demo` runs on remote host `sat-a` — attachments are not yet supported for remote projects" }),
    );
    await expect(uploadAttachment("demo", new File(["x"], "a"))).rejects.toThrow(
      "remote host",
    );
  });
});

describe("listProjectSkills", () => {
  it("GETs the project skills list", async () => {
    const skills: SkillSummary[] = [
      { skill: "deep-research", description: "fan-out research harness" },
    ];
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, skills));
    const got = await listProjectSkills("demo");
    expect(got).toEqual(skills);
    const [url] = fetchMock.mock.calls[0]!;
    expect(url).toBe("/api/v1/projects/demo/skills");
  });

  it("maps 401 to UNAUTHENTICATED", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(401, {}));
    await expect(listProjectSkills("demo")).rejects.toThrow("UNAUTHENTICATED");
  });
});

// v0.9.9 — the user-level global skill library (admin-only endpoint).
describe("listLibrarySkills", () => {
  it("GETs /api/v1/skills and unwraps {skills} (nested ids; source optional)", async () => {
    const skills: LibrarySkillSummary[] = [
      {
        id: "grill-me",
        description: "decision-tree griller",
        path: "/home/u/.ccteam/skills/grill-me/SKILL.md",
        source: "hub",
      },
      {
        id: "baoyu-skills/baoyu-comic",
        description: "comic renderer",
        path: "/home/u/.ccteam/skills/baoyu-skills/baoyu-comic/SKILL.md",
      },
    ];
    const fetchMock = vi.mocked(globalThis.fetch);
    fetchMock.mockResolvedValueOnce(jsonResponse(200, { skills }));
    const got = await listLibrarySkills();
    expect(got).toEqual(skills);
    const [url, init] = fetchMock.mock.calls[0]!;
    expect(url).toBe("/api/v1/skills");
    expect(init?.method ?? "GET").toBe("GET");
  });

  it("lifts the 403 admin-only error body", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(
      jsonResponse(403, { error: "admin only" }),
    );
    await expect(listLibrarySkills()).rejects.toThrow("admin only");
  });

  it("maps 401 to UNAUTHENTICATED", async () => {
    vi.mocked(globalThis.fetch).mockResolvedValueOnce(jsonResponse(401, {}));
    await expect(listLibrarySkills()).rejects.toThrow("UNAUTHENTICATED");
  });
});
