import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { api } from "../api/PondApiClient";
import { allZones, deviceZone, detectPlace, placeFromZone, __resetZoneCache } from "./place";

vi.mock("../api/PondApiClient", () => ({
  api: { listTimeZones: vi.fn(), detectLocation: vi.fn() },
}));

const mockApi = api as unknown as {
  listTimeZones: ReturnType<typeof vi.fn>;
  detectLocation: ReturnType<typeof vi.fn>;
};

beforeEach(() => {
  vi.clearAllMocks();
  __resetZoneCache();
});
afterEach(() => vi.unstubAllGlobals());

describe("zones", () => {
  it("uses the server's IANA catalogue when it answers", async () => {
    mockApi.listTimeZones.mockResolvedValue({
      zones: [{ zone: "Africa/Kampala", offset: "+03:00", place: "Kampala" }],
    });
    const zones = await allZones();
    expect(zones.map((z) => z.zone)).toContain("Africa/Kampala");
  });

  it("falls back to this device's own catalogue when the server is unreachable", async () => {
    mockApi.listTimeZones.mockRejectedValue(new Error("offline"));
    const zones = await allZones();
    expect(zones.length).toBeGreaterThan(0);
    expect(zones.every((z) => typeof z.offset === "string")).toBe(true);
  });

  it("always offers the device's own zone", async () => {
    mockApi.listTimeZones.mockRejectedValue(new Error("offline"));
    vi.stubGlobal("Intl", {
      DateTimeFormat: Object.assign(
        () => ({
          resolvedOptions: () => ({ timeZone: "Pacific/Chatham" }),
          formatToParts: () => [{ type: "timeZoneName", value: "GMT+12:45" }],
        }),
        { supportedValuesOf: undefined },
      ),
    });
    const zones = await allZones();
    expect(zones.map((z) => z.zone)).toContain("Pacific/Chatham");
  });

  it("caches, so a picker does not refetch per render", async () => {
    mockApi.listTimeZones.mockResolvedValue({
      zones: [{ zone: "UTC", offset: "+00:00", place: "" }],
    });
    await allZones();
    await allZones();
    expect(mockApi.listTimeZones).toHaveBeenCalledTimes(1);
  });
});

describe("placeFromZone", () => {
  it("reads the place out of a zone name", () => {
    expect(placeFromZone("America/New_York")).toBe("New York");
    expect(placeFromZone("Africa/Nairobi")).toBe("Nairobi");
  });

  it("does not invent a place for a zone that is not one", () => {
    expect(placeFromZone("UTC")).toBe("");
  });
});

describe("detectPlace", () => {
  /** The normal case inside Tauri. */
  it("still detects when the webview has no geolocation", async () => {
    vi.stubGlobal("navigator", { geolocation: undefined });
    mockApi.detectLocation.mockResolvedValue({ name: "Nairobi, Kenya", has_coordinates: true });
    await detectPlace();
    expect(mockApi.detectLocation).toHaveBeenCalledTimes(1);
    const sent = mockApi.detectLocation.mock.calls[0][0];
    expect(sent.system_zone).toBe(deviceZone());
    expect(sent.latitude).toBeUndefined();
  });

  it("offers the device's coordinates when a browser provides them", async () => {
    vi.stubGlobal("navigator", {
      geolocation: {
        getCurrentPosition: (ok: (p: unknown) => void) =>
          ok({ coords: { latitude: -1.2864123, longitude: 36.8172223 } }),
      },
    });
    mockApi.detectLocation.mockResolvedValue({ name: "Nairobi", has_coordinates: true });
    await detectPlace();
    const sent = mockApi.detectLocation.mock.calls[0][0];
    expect(sent.latitude).toBe(-1.2864);
    expect(sent.longitude).toBe(36.8172);
  });

  it("gives up on a geolocation that never answers", async () => {
    vi.useFakeTimers();
    vi.stubGlobal("navigator", { geolocation: { getCurrentPosition: () => {} } });
    mockApi.detectLocation.mockResolvedValue({ name: "Nairobi", has_coordinates: false });

    const pending = detectPlace();
    await vi.advanceTimersByTimeAsync(7000);
    await pending;

    expect(mockApi.detectLocation).toHaveBeenCalledTimes(1);
    expect(mockApi.detectLocation.mock.calls[0][0].latitude).toBeUndefined();
    vi.useRealTimers();
  });

  it("passes a typed name through, so it beats the zone's guess", async () => {
    vi.stubGlobal("navigator", { geolocation: undefined });
    mockApi.detectLocation.mockResolvedValue({ name: "Kisumu, Kenya", has_coordinates: true });
    await detectPlace("  Kisumu  ");
    expect(mockApi.detectLocation.mock.calls[0][0].typed_name).toBe("Kisumu");
  });
});
