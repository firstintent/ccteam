// v0.9.11 TEAM-6 — `lib/charterRoster.ts` unit suite (node env, no DOM/React
// needed at all — these are plain functions). Mirrors how `charterState.ts`'s
// reducer is tested apart from the views that dispatch into it;
// `pages/CharterPanel.test.tsx` covers VendorRosterCards' render/wiring of
// these same pieces.

import { afterEach, describe, expect, it, vi } from "vitest";

vi.mock("./hostsApi", async (importOriginal) => {
  const actual = await importOriginal<typeof import("./hostsApi")>();
  return { ...actual, deleteHost: vi.fn() };
});

import { deleteHost } from "./hostsApi";
import { handleRosterRemoveClick, sortRosterHosts, type RosterHost } from "./charterRoster";

function fixtureHost(over: Partial<RosterHost> = {}): RosterHost {
  return {
    host: "local",
    hostname: "box",
    status: "online",
    agents: [],
    ...over,
  };
}

describe("sortRosterHosts", () => {
  it("puts local first, then online hosts before offline, keeping each bucket's own order", () => {
    const scrambled: RosterHost[] = [
      fixtureHost({ host: "b-offline", status: "offline" }),
      fixtureHost({ host: "a-online", status: "online" }),
      fixtureHost({ host: "local", status: "online" }),
      fixtureHost({ host: "c-offline", status: "offline" }),
    ];
    expect(sortRosterHosts(scrambled).map((h) => h.host)).toEqual([
      "local",
      "a-online",
      "b-offline",
      "c-offline",
    ]);
  });

  it("is a no-op copy on an already-sorted list and doesn't mutate the input", () => {
    const hosts: RosterHost[] = [fixtureHost({ host: "local" })];
    const sorted = sortRosterHosts(hosts);
    expect(sorted).toEqual(hosts);
    expect(sorted).not.toBe(hosts);
  });
});

describe("handleRosterRemoveClick (remove-button orchestration, no hooks — mocks deleteHost)", () => {
  afterEach(() => {
    vi.mocked(deleteHost).mockReset();
  });

  it("(d) an OFFLINE host removes immediately: one deleteHost call, no confirm arming", async () => {
    vi.mocked(deleteHost).mockResolvedValueOnce({ host: "smoke-self" });
    const setConfirmingHost = vi.fn();
    const setRoster = vi.fn();
    const onError = vi.fn();
    handleRosterRemoveClick({
      host: "smoke-self",
      online: false,
      confirmingHost: null,
      setConfirmingHost,
      setRoster,
      onError,
    });
    expect(deleteHost).toHaveBeenCalledWith("smoke-self", undefined);
    expect(setConfirmingHost).not.toHaveBeenCalled(); // no arming step for offline
    await Promise.resolve();
    await Promise.resolve();
    expect(setRoster).toHaveBeenCalledTimes(1);
    const updater = setRoster.mock.calls[0]![0] as (prev: RosterHost[]) => RosterHost[];
    const remaining = updater([fixtureHost({ host: "smoke-self" }), fixtureHost({ host: "local" })]);
    expect(remaining.map((h) => h.host)).toEqual(["local"]);
  });

  it("(e) an ONLINE host requires two calls: first arms (no API call), second fires force=true", async () => {
    vi.mocked(deleteHost).mockResolvedValueOnce({ host: "dxa347" });
    const setConfirmingHost = vi.fn();
    const setRoster = vi.fn();
    const onError = vi.fn();

    handleRosterRemoveClick({
      host: "dxa347",
      online: true,
      confirmingHost: null,
      setConfirmingHost,
      setRoster,
      onError,
    });
    expect(deleteHost).not.toHaveBeenCalled();
    expect(setConfirmingHost).toHaveBeenCalledWith("dxa347");

    handleRosterRemoveClick({
      host: "dxa347",
      online: true,
      confirmingHost: "dxa347", // now armed, as CharterPanel's state would be after the first click
      setConfirmingHost,
      setRoster,
      onError,
    });
    expect(deleteHost).toHaveBeenCalledTimes(1);
    expect(deleteHost).toHaveBeenCalledWith("dxa347", { force: true });
    await Promise.resolve();
    await Promise.resolve();
    expect(setRoster).toHaveBeenCalledTimes(1);
  });

  it("a second call for a DIFFERENT host while one is armed re-arms instead of firing", () => {
    const setConfirmingHost = vi.fn();
    const setRoster = vi.fn();
    const onError = vi.fn();
    handleRosterRemoveClick({
      host: "other-host",
      online: true,
      confirmingHost: "dxa347", // a DIFFERENT host was armed
      setConfirmingHost,
      setRoster,
      onError,
    });
    expect(deleteHost).not.toHaveBeenCalled();
    expect(setConfirmingHost).toHaveBeenCalledWith("other-host");
  });

  it("(f) on failure, the roster is untouched and onError reports the message", async () => {
    vi.mocked(deleteHost).mockRejectedValueOnce(
      new Error("host dxa347 is online; pass ?force=true to remove a live satellite"),
    );
    const setConfirmingHost = vi.fn();
    const setRoster = vi.fn();
    const onError = vi.fn();
    handleRosterRemoveClick({
      host: "dxa347",
      online: false,
      confirmingHost: null,
      setConfirmingHost,
      setRoster,
      onError,
    });
    await Promise.resolve();
    await Promise.resolve();
    expect(setRoster).not.toHaveBeenCalled();
    expect(onError).toHaveBeenCalledWith(
      "host dxa347 is online; pass ?force=true to remove a live satellite",
    );
  });
});
