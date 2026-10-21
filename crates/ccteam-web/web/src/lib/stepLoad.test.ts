import { describe, expect, it } from "vitest";
import { failedLoad, initialLoad, shouldRetry } from "./stepLoad";

describe("step inspector load state", () => {
  it("asks the server for any step with a record id, running or not", () => {
    expect(initialLoad({ step_id: "n-1-t1" })).toBeNull();
  });

  it("says a step from a daemon that sent no id has no record", () => {
    expect(initialLoad({ step_id: "" })).toEqual({ state: "unavailable" });
  });

  it("reads a 404 by the local done flag and keeps any other failure", () => {
    expect(failedLoad(new Error("NOT_FOUND"), false)).toEqual({ state: "running" });
    expect(failedLoad(new Error("NOT_FOUND"), true)).toEqual({ state: "unavailable" });
    expect(failedLoad(new Error("network: down"), true)).toEqual({
      state: "error",
      message: "network: down",
    });
  });

  it("retries only while the record may still appear", () => {
    expect(shouldRetry({ state: "running" })).toBe(true);
    expect(shouldRetry({ state: "unavailable" })).toBe(true);
    expect(shouldRetry({ state: "error", message: "x" })).toBe(false);
    expect(shouldRetry({ state: "loading" })).toBe(false);
  });
});
