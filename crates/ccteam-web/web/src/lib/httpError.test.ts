import { describe, expect, it } from "vitest";
import { errorDetail, httpError } from "./httpError";

function res(status: number, body: string, ok = false): Response {
  return {
    status,
    ok,
    text: () => Promise.resolve(body),
  } as unknown as Response;
}

describe("httpError (the server already wrote the reason — deliver it)", () => {
  it("lifts a ccteam `{error}` body into the message", async () => {
    const e = await httpError(res(404, JSON.stringify({ error: "unknown tenant: u1" })));
    expect(e.message).toBe("HTTP 404: unknown tenant: u1");
  });

  it("uses a plain-text body verbatim (axum extractor rejections)", async () => {
    const e = await httpError(res(422, "Failed to deserialize the JSON body"));
    expect(e.message).toBe("HTTP 422: Failed to deserialize the JSON body");
  });

  it("falls back to the bare status when the body carries nothing", async () => {
    expect((await httpError(res(500, ""))).message).toBe("HTTP 500");
    expect((await httpError(res(404, JSON.stringify({}))))!.message).toBe("HTTP 404: {}");
  });

  it("never throws on an unreadable body", async () => {
    const broken = {
      status: 503,
      ok: false,
      text: () => Promise.reject(new Error("stream consumed")),
    } as unknown as Response;
    expect(await errorDetail(broken)).toBeNull();
    expect((await httpError(broken)).message).toBe("HTTP 503");
  });

  it("caps a runaway body so a toast stays a toast", async () => {
    const e = await httpError(res(500, "x".repeat(1000)));
    expect(e.message.length).toBeLessThan(330);
  });
});
