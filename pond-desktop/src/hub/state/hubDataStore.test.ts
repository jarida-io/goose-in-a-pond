import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("../../api/PondApiClient", () => ({
  api: {
    getSettings: vi.fn(),
    listDevices: vi.fn(),
    listSchedules: vi.fn(),
    listRecipes: vi.fn().mockResolvedValue([]),
    getWeather: vi.fn().mockResolvedValue({ enabled: false }),
    getNowPlaying: vi.fn().mockResolvedValue({ connected: false }),
  },
}));

import { api } from "../../api/PondApiClient";
import { getHomeData, refreshHomeData, refreshWeather, refreshNowPlaying,
         resumeNowPlayingPolling,
         __resetHubDataForTests, __tickNowPlayingPollForTests } from "./hubDataStore";
import { ROUTINES as MOCK_ROUTINES } from "../data/routines";

const apiMock = api as unknown as {
  getSettings: ReturnType<typeof vi.fn>;
  listDevices: ReturnType<typeof vi.fn>;
  listSchedules: ReturnType<typeof vi.fn>;
  listRecipes: ReturnType<typeof vi.fn>;
  getWeather: ReturnType<typeof vi.fn>;
  getNowPlaying: ReturnType<typeof vi.fn>;
};

describe("hubDataStore", () => {
  beforeEach(() => {
    __resetHubDataForTests();
    apiMock.getSettings.mockReset();
    apiMock.listDevices.mockReset();
    apiMock.listSchedules.mockReset();
    apiMock.listRecipes.mockReset();
    apiMock.listRecipes.mockResolvedValue([]);
    apiMock.getWeather.mockReset();
    apiMock.getWeather.mockResolvedValue({ enabled: false });
    apiMock.getNowPlaying.mockReset();
    apiMock.getNowPlaying.mockResolvedValue({ connected: false });
  });

  it("falls back to mock data when API returns empty devices", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);

    await refreshHomeData();
    const home = getHomeData();
    expect(home.devices.length).toBeGreaterThan(0);
    expect(home.cameras.length).toBeGreaterThan(0);
    expect(home.user).toBe("Jerry");
  });

  it("uses settings.user_name when present", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);

    await refreshHomeData();
    expect(getHomeData().user).toBe("Ada");
  });

  it("derives categories and rooms from real devices", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([
      { id: "lr1", name: "Lamp", device_type: "light",      is_online: true, room: "Living Room", metadata: { on: true } },
      { id: "fd",  name: "Door", device_type: "lock",       is_online: true, room: "Outdoor",     metadata: { locked: true } },
      { id: "th",  name: "Nest", device_type: "thermostat", is_online: true, room: "Living Room", metadata: { target: 72 } },
      { id: "cm",  name: "Cam",  device_type: "camera",     is_online: true, last_seen: "2026-06-01T13:48:00Z" },
    ]);
    apiMock.listSchedules.mockResolvedValue([]);

    await refreshHomeData();
    const home = getHomeData();
    expect(home.devices.map((d) => d.id).sort()).toEqual(["fd", "lr1", "th"]);
    expect(home.cameras.map((c) => c.id)).toEqual(["cm"]);
    expect(home.rooms.map((r) => r.name)).toContain("Home");
    expect(home.rooms.map((r) => r.name)).toContain("Living Room");
    expect(home.rooms.map((r) => r.name)).toContain("Outdoor");
    const lights = home.categories.find((c) => c.id === "lights");
    expect(lights?.status).toBe("1 on");
    const climate = home.categories.find((c) => c.id === "climate");
    expect(climate?.status).toBe("Heat to 72°");
  });

  it("uses real weather when the API reports enabled", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);
    apiMock.getWeather.mockResolvedValue({
      enabled: true,
      temp: 71,
      cond: "Clear sky",
      icon: "sun",
      hi: 75,
      lo: 60,
      hum: 40,
      wind: 8,
      forecast: [{ d: "Wed", i: "rain", t: 55 }],
    });

    await refreshHomeData();
    const home = getHomeData();
    expect(home.weather.temp).toBe(71);
    expect(home.weather.icon).toBe("sun");
    expect(home.weather.forecast).toEqual([{ d: "Wed", i: "rain", t: 55 }]);
  });

  it("falls back to mock weather when the API reports disabled", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);
    apiMock.getWeather.mockResolvedValue({ enabled: false });

    await refreshHomeData();
    const home = getHomeData();
    expect(home.weather.temp).toBe(64);
    expect(home.weather.cond).toBe("Partly cloudy");
  });

  it("refreshWeather updates the weather slice without a full reload", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);
    apiMock.getWeather.mockResolvedValue({ enabled: true, temp: 16, cond: "Overcast", icon: "cloud" });

    await refreshHomeData();
    expect(getHomeData().weather.temp).toBe(16);

    // The sky changed while the dashboard sat open; only the poll re-runs.
    apiMock.getWeather.mockResolvedValue({ enabled: true, temp: 21, cond: "Clear sky", icon: "sun" });
    apiMock.listDevices.mockClear();

    await refreshWeather();
    const home = getHomeData();
    expect(home.weather.temp).toBe(21);
    expect(home.weather.cond).toBe("Clear sky");
    expect(apiMock.listDevices).not.toHaveBeenCalled();
  });

  it("refreshWeather keeps the last reading when the fetch fails", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);
    apiMock.getWeather.mockResolvedValue({ enabled: true, temp: 16, cond: "Overcast", icon: "cloud" });

    await refreshHomeData();
    apiMock.getWeather.mockRejectedValue(new Error("server offline"));

    await refreshWeather();
    expect(getHomeData().weather.temp).toBe(16);
  });

  it("falls back to mock routines when no recipes returned", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);
    apiMock.listRecipes.mockResolvedValue([]);

    await refreshHomeData();
    const { useRoutines: _u, getHomeData: _g } = await import("./hubDataStore");
    const { __resetHubDataForTests: _r } = await import("./hubDataStore");
    void _u; void _g; void _r;
    const mod = await import("./hubDataStore");
    // routines aren't on HomeData — read via the snapshot used by useRoutines
    const snapshot = (mod as unknown as { __getRoutinesForTests?: () => unknown[] }).__getRoutinesForTests?.()
      ?? MOCK_ROUTINES;
    expect(Array.isArray(snapshot)).toBe(true);
    expect((snapshot as { name: string }[]).map((r) => r.name)).toContain("Good Morning");
  });

  it("maps recipes to routines (known names reuse mock visual templates)", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);
    apiMock.listRecipes.mockResolvedValue([
      { name: "Good Morning", description: "wake up macro", yaml: "" },
      { name: "Sunset Bath",  description: "Run tub, dim lights, play jazz", yaml: "" },
    ]);

    await refreshHomeData();
    const mod = await import("./hubDataStore");
    const snapshot = (mod as unknown as { __getRoutinesForTests?: () => unknown[] }).__getRoutinesForTests?.() ?? [];
    expect(snapshot.length).toBe(2);
    const morning = (snapshot as { name: string; does: string[] }[]).find((r) => r.name === "Good Morning");
    expect(morning?.does.length).toBeGreaterThan(1);
    const sunset = (snapshot as { name: string; does: string[] }[]).find((r) => r.name === "Sunset Bath");
    expect(sunset?.does).toEqual(["Run tub", "dim lights", "play jazz"]);
  });

  // ── Now Playing ──────────────────────────────────────────────
  // Spotify dev-mode apps 403 every call for non-allowlisted accounts, even after OAuth;
  // that must never look like a paused player.

  async function loadWithNowPlaying(np: unknown) {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);
    apiMock.getNowPlaying.mockResolvedValue(np);
    await refreshHomeData();
    return getHomeData().nowPlaying;
  }

  it("surfaces a Spotify authorisation failure instead of an idle player", async () => {
    const np = await loadWithNowPlaying({
      connected: true,
      playing: false,
      error: "forbidden",
      message: "This Spotify account is not authorised for the app GIAP signs in with.",
    });

    expect(np.error).toBe("forbidden");
    expect(np.track).toBe("Spotify not authorised");
    expect(np.artist).toContain("not authorised");
    expect(np.track).not.toBe("Nothing playing");
    expect(np.playing).toBe(false);
  });

  it("labels non-403 Spotify failures without claiming an authorisation problem", async () => {
    const np = await loadWithNowPlaying({
      connected: true,
      playing: false,
      error: "rate_limited",
      message: "Spotify is rate-limiting requests.",
    });

    expect(np.error).toBe("rate_limited");
    expect(np.track).toBe("Spotify unavailable");
  });

  it("still shows an honest idle state when nothing is playing", async () => {
    const np = await loadWithNowPlaying({ connected: true, playing: false });

    expect(np.error).toBeUndefined();
    expect(np.track).toBe("Nothing playing");
    expect(np.connected).toBe(true);
  });

  it("keeps real playback untouched", async () => {
    const np = await loadWithNowPlaying({
      connected: true,
      playing: true,
      track: "Blinding Lights",
      artist: "The Weeknd",
      progress_ms: 60_000,
      duration_ms: 200_000,
    });

    expect(np.error).toBeUndefined();
    expect(np.track).toBe("Blinding Lights");
    expect(np.artist).toBe("The Weeknd");
    expect(np.playing).toBe(true);
    expect(np.elapsed).toBeCloseTo(0.3);
  });

  it("derives scenes from schedules", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([
      { id: "lr1", name: "Lamp", device_type: "light", is_online: true, room: "Living Room" },
    ]);
    apiMock.listSchedules.mockResolvedValue([
      { id: "s1", name: "morning_routine", label: "Wake Up", cron: "0 7 * * *", prompt: "", enabled: true },
      { id: "s2", name: "bedtime",         label: "Bedtime", cron: "0 22 * * *", prompt: "", enabled: true },
    ]);

    await refreshHomeData();
    const home = getHomeData();
    expect(home.scenes.length).toBe(2);
    expect(home.scenes[0].name).toBe("Wake Up");
    expect(home.scenes[1].name).toBe("Bedtime");
  });
});

describe("now-playing polling", () => {
  beforeEach(() => {
    __resetHubDataForTests();
    apiMock.getNowPlaying.mockReset();
  });

  // Five consecutive 4XX stop the poll; only an interaction brings it back.

  const REFUSAL = { connected: true, error: "forbidden", upstream_status: 403 };

  async function answer(np: unknown, times = 1) {
    apiMock.getNowPlaying.mockResolvedValue(np);
    for (let i = 0; i < times; i += 1) await refreshNowPlaying();
  }

  it("keeps polling through the first four refusals", async () => {
    await answer(REFUSAL, 4);
    expect(__tickNowPlayingPollForTests()).toBe(true);
  });

  it("stops asking after five consecutive 4XX answers", async () => {
    await answer(REFUSAL, 5);
    expect(__tickNowPlayingPollForTests()).toBe(false);
    // Still stopped on later ticks.
    expect(__tickNowPlayingPollForTests()).toBe(false);
    expect(__tickNowPlayingPollForTests()).toBe(false);
  });

  it("only counts real 4XX answers", async () => {
    // `unavailable` is a 5xx and a transport failure has no status; neither may count.
    await answer({ connected: true, error: "unavailable", upstream_status: 502 }, 5);
    expect(__tickNowPlayingPollForTests()).toBe(true);

    __resetHubDataForTests();
    await answer(null, 5);
    expect(__tickNowPlayingPollForTests()).toBe(true);
  });

  it("needs the five to be consecutive", async () => {
    await answer(REFUSAL, 4);
    await answer({ connected: true, playing: true, track: "x" });
    await answer(REFUSAL, 4);
    expect(__tickNowPlayingPollForTests()).toBe(true);
  });

  it("resumes when the widget is used", async () => {
    await answer(REFUSAL, 5);
    expect(__tickNowPlayingPollForTests()).toBe(false);

    // The Try again (`userInitiated`) path; the tick calls the same function and must not resume.
    apiMock.getNowPlaying.mockResolvedValue({ connected: true, playing: true, track: "x" });
    await refreshNowPlaying(true);
    expect(__tickNowPlayingPollForTests()).toBe(true);
  });

  it("resumes when the music service is engaged directly", async () => {
    await answer(REFUSAL, 5);
    expect(__tickNowPlayingPollForTests()).toBe(false);

    resumeNowPlayingPolling();
    expect(__tickNowPlayingPollForTests()).toBe(true);
  });
});
