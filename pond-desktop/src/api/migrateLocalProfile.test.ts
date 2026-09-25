// The server is the household's record; a migration that won ties could revert a corrected name.

import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

const getSettings = vi.fn();
const listProfiles = vi.fn();
const getProfilePrefs = vi.fn();
const updateProfilePrefs = vi.fn();

vi.mock("./PondApiClient", () => ({
  api: {
    getSettings: (...a: unknown[]) => getSettings(...a),
    listProfiles: (...a: unknown[]) => listProfiles(...a),
    getProfilePrefs: (...a: unknown[]) => getProfilePrefs(...a),
    updateProfilePrefs: (...a: unknown[]) => updateProfilePrefs(...a),
  },
}));

import { migrateLocalProfileToServer } from "./migrateLocalProfile";

const LOCAL_KEY = "giap-user-profile";
const DONE_KEY = "giap-user-profile-migrated";

beforeEach(() => {
  localStorage.clear();
  vi.clearAllMocks();
  getSettings.mockResolvedValue({ primary_profile_id: "p1" });
  listProfiles.mockResolvedValue({ profiles: [{ id: "p1", display_name: "Jerry" }] });
  getProfilePrefs.mockResolvedValue({});
  updateProfilePrefs.mockResolvedValue({});
});
afterEach(() => localStorage.clear());

describe("migrateLocalProfileToServer", () => {
  it("lifts stranded values and maps them to the server's spelling", async () => {
    localStorage.setItem(
      LOCAL_KEY,
      JSON.stringify({ preferredName: "Cap", birthday: "1990-04-02", atypicalSpeech: true }),
    );

    await migrateLocalProfileToServer();

    expect(updateProfilePrefs).toHaveBeenCalledTimes(1);
    const [id, prefs] = updateProfilePrefs.mock.calls[0];
    expect(id).toBe("p1");
    expect(prefs).toMatchObject({
      preferred_name: "Cap",
      birthday: "1990-04-02",
      // Boolean -> the literal string the server compares against.
      accessibility_atypical_speech: "true",
    });
    expect(prefs).not.toHaveProperty("preferredName");
  });

  it("never overwrites a value the pond already has", async () => {
    getProfilePrefs.mockResolvedValue({ preferred_name: "Jerry" });
    localStorage.setItem(LOCAL_KEY, JSON.stringify({ preferredName: "Stale", birthday: "1990-04-02" }));

    await migrateLocalProfileToServer();

    const [, prefs] = updateProfilePrefs.mock.calls[0];
    expect(prefs.preferred_name).toBe(
      "Jerry",
    );
    // ...but the value the server did NOT have still gets rescued.
    expect(prefs.birthday).toBe("1990-04-02");
  });

  it("does not write at all when there is nothing to rescue", async () => {
    getProfilePrefs.mockResolvedValue({ preferred_name: "Jerry" });
    localStorage.setItem(LOCAL_KEY, JSON.stringify({ preferredName: "Stale" }));

    await migrateLocalProfileToServer();

    expect(updateProfilePrefs).not.toHaveBeenCalled();
    expect(localStorage.getItem(DONE_KEY)).toBe("1");
  });

  it("runs once, not on every boot", async () => {
    localStorage.setItem(LOCAL_KEY, JSON.stringify({ preferredName: "Cap" }));

    await migrateLocalProfileToServer();
    await migrateLocalProfileToServer();

    expect(updateProfilePrefs).toHaveBeenCalledTimes(1);
  });

  it("retries next boot when there is no member to attach to yet", async () => {
    // Marking done here would strand the data once a member is created.
    getSettings.mockResolvedValue({ primary_profile_id: null });
    localStorage.setItem(LOCAL_KEY, JSON.stringify({ preferredName: "Cap" }));

    await migrateLocalProfileToServer();

    expect(updateProfilePrefs).not.toHaveBeenCalled();
    expect(localStorage.getItem(DONE_KEY)).toBeNull();
  });

  it("retries next boot when the API is unreachable", async () => {
    getSettings.mockRejectedValue(new Error("connection refused"));
    localStorage.setItem(LOCAL_KEY, JSON.stringify({ preferredName: "Cap" }));

    // Must not throw: this runs at startup and cannot be what stops the app.
    await expect(migrateLocalProfileToServer()).resolves.toBeUndefined();
    expect(localStorage.getItem(DONE_KEY)).toBeNull();
  });

  it("keeps the browser copy rather than deleting it", async () => {
    localStorage.setItem(LOCAL_KEY, JSON.stringify({ preferredName: "Cap" }));
    await migrateLocalProfileToServer();
    expect(localStorage.getItem(LOCAL_KEY)).not.toBeNull();
  });
});
