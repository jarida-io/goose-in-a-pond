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
         __resetHubDataForTests, __tickNowPlayingPollForTests,
         __getRoutinesForTests } from "./hubDataStore";

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

  it("reports an empty house as empty rather than borrowing a demo one", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);

    await refreshHomeData();
    const home = getHomeData();
    expect(home.devices).toEqual([]);
    expect(home.cameras).toEqual([]);
    // The flag says the emptiness was answered for, not merely not-yet-loaded.
    expect(home.devicesAreReal).toBe(true);
    expect(home.user).toBe("Jerry");
  });

  it("seeds nothing at all before the first load", () => {
    const home = getHomeData();
    expect(home.devices).toEqual([]);
    expect(home.rooms).toEqual([]);
    expect(home.cameras).toEqual([]);
    expect(home.categories).toEqual([]);
    expect(home.scenes).toEqual([]);
    expect(home.devicesAreReal).toBe(false);
    expect(home.nowPlaying.track).toBe("");
    expect(home.weather.cond).toBe("");
  });

  /** `list_devices` sends identity and capabilities only, never state. */
  it("invents no on, locked or setpoint for a device that reported none", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([
      { id: "lr1", name: "Lamp", device_type: "light",      is_online: true, room: "Living Room" },
      { id: "fd",  name: "Door", device_type: "lock",       is_online: true, room: "Outdoor" },
      { id: "th",  name: "Nest", device_type: "thermostat", is_online: true, room: "Living Room" },
    ]);
    apiMock.listSchedules.mockResolvedValue([]);

    await refreshHomeData();
    const byId = Object.fromEntries(getHomeData().devices.map((d) => [d.id, d]));
    expect(byId.lr1.on).toBeUndefined();
    expect(byId.fd.locked).toBeUndefined();
    expect(byId.th.target).toBeUndefined();
    expect(byId.th.value).toBeUndefined();
  });

  it("does not let a category chip claim a state nothing reported", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([
      { id: "fd",  name: "Door", device_type: "lock",       is_online: true, room: "Outdoor" },
      { id: "lr1", name: "Lamp", device_type: "light",      is_online: true, room: "Living Room" },
      { id: "th",  name: "Nest", device_type: "thermostat", is_online: true, room: "Living Room" },
    ]);
    apiMock.listSchedules.mockResolvedValue([]);

    await refreshHomeData();
    const byId = Object.fromEntries(getHomeData().categories.map((c) => [c.id, c]));
    expect(byId.locks.status).toBe("Not reported");
    expect(byId.lights.status).toBe("Not reported");
    expect(byId.climate.status).toBe("Not reported");
    // No Security chip: nothing reports alarm state.
    expect(byId.security).toBeUndefined();
  });

  it("will not say all locked when one lock stayed silent", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([
      { id: "fd", name: "Front", device_type: "lock", is_online: true, room: "Outdoor", metadata: { locked: true } },
      { id: "bd", name: "Back",  device_type: "lock", is_online: true, room: "Outdoor" },
    ]);
    apiMock.listSchedules.mockResolvedValue([]);

    await refreshHomeData();
    const locks = getHomeData().categories.find((c) => c.id === "locks");
    expect(locks?.status).toBe("1/1 locked");
  });

  it("shows no scenes when the pond has no schedules", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);

    await refreshHomeData();
    expect(getHomeData().scenes).toEqual([]);
  });

  /** Home gates a device's power control on this, so a sensor is never offered one. */
  it("carries a device's capabilities through untouched", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([
      { id: "lr1", name: "Lamp", device_type: "light", is_online: true, room: "Living Room",
        capabilities: ["power", "brightness"] },
      { id: "cs1", name: "Contact", device_type: "sensor", is_online: true, room: "Hall", capabilities: [] },
    ]);
    apiMock.listSchedules.mockResolvedValue([]);

    await refreshHomeData();
    const home = getHomeData();
    expect(home.devices.find((d) => d.id === "lr1")?.capabilities).toEqual(["power", "brightness"]);
    expect(home.devices.find((d) => d.id === "cs1")?.capabilities).toEqual([]);
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

  it("reports no weather at all when the API says it is disabled", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);
    apiMock.getWeather.mockResolvedValue({ enabled: false });

    await refreshHomeData();
    const home = getHomeData();
    expect(home.weatherEnabled).toBe(false);
    expect(home.weather.cond).toBe("");
    expect(home.weather.forecast).toEqual([]);
  });

  it("does not fill a real answer's gaps from the mock record", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);
    apiMock.getWeather.mockResolvedValue({ enabled: true, temp: 12, cond: "Overcast", icon: "cloud" });

    await refreshHomeData();
    const home = getHomeData();
    expect(home.weatherEnabled).toBe(true);
    expect(home.weather.temp).toBe(12);
    expect(home.weather.hi).toBe(0);
    expect(home.weather.lo).toBe(0);
    expect(home.weather.forecast).toEqual([]);
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

  /** `{enabled:false}` means no provider configured; with one set, a failure is a 502. */
  it("does not report a failed weather fetch as weather being off", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);
    apiMock.getWeather.mockRejectedValue(new Error("502 Failed to fetch weather"));

    await refreshHomeData();
    expect(getHomeData().weatherStatus).toBe("unreachable");
  });

  it("reports weather being switched off as off", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);
    apiMock.getWeather.mockResolvedValue({ enabled: false });

    await refreshHomeData();
    expect(getHomeData().weatherStatus).toBe("off");
  });

  it("keeps the last good reading when a full reload cannot reach weather", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);
    apiMock.getWeather.mockResolvedValue({ enabled: true, temp: 71, cond: "Clear sky", icon: "sun" });

    await refreshHomeData();
    expect(getHomeData().weatherStatus).toBe("on");

    // The server restarts mid-session and AppContext re-runs the whole load.
    apiMock.getWeather.mockRejectedValue(new Error("502 Failed to fetch weather"));
    await refreshHomeData();

    const home = getHomeData();
    expect(home.weather.temp).toBe(71);
    expect(home.weather.cond).toBe("Clear sky");
    expect(home.weatherEnabled).toBe(true);
    expect(home.weatherStatus).toBe("unreachable");
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
    // Kept, but marked so nothing presents it as current.
    expect(getHomeData().weatherStatus).toBe("unreachable");
  });

  it("shows no routines when the pond has no recipes", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);
    apiMock.listRecipes.mockResolvedValue([]);

    await refreshHomeData();
    expect(__getRoutinesForTests()).toEqual([]);
  });

  it("shows no routines when the recipe call fails", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);
    apiMock.listRecipes.mockRejectedValue(new Error("server offline"));

    await refreshHomeData();
    expect(__getRoutinesForTests()).toEqual([]);
  });

  it("starts with no routines at all", () => {
    expect(__getRoutinesForTests()).toEqual([]);
  });

  it("maps recipes to routines from the recipe itself", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);
    apiMock.listRecipes.mockResolvedValue([
      { name: "Good Morning", description: "wake up macro", yaml: "" },
      { name: "Sunset Bath",  description: "Run tub, dim lights, play jazz", yaml: "" },
    ]);

    await refreshHomeData();
    const snapshot = __getRoutinesForTests();
    expect(snapshot.length).toBe(2);
    // A name matching a fixture routine still keeps its own description and no schedule.
    const morning = snapshot.find((r) => r.name === "Good Morning");
    expect(morning?.does).toEqual(["wake up macro"]);
    expect(morning?.time).toBe("On demand");
    // Every other recipe derives its chips from its description.
    const sunset = snapshot.find((r) => r.name === "Sunset Bath");
    expect(sunset?.does).toEqual(["Run tub", "dim lights", "play jazz"]);
    expect(sunset?.time).toBe("On demand");
  });

  it("leaves agents out of the routines: they start from a schedule, an event or a hand-off", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);
    apiMock.listRecipes.mockResolvedValue([
      { name: "Sunset Bath", description: "Run tub", yaml: "prompt: run the tub\n" },
      { name: "morning-brief", description: "Briefs the house", yaml: "giap_role:\n  tool_groups: []\n  instructions: Brief the house.\n" },
    ]);

    await refreshHomeData();
    expect(__getRoutinesForTests().map((r) => r.name)).toEqual(["Sunset Bath"]);
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

  it("follows the household's choice: with Apple Music chosen, the card is Apple Music's", async () => {
    const np = await loadWithNowPlaying({ connected: false, service: "apple", player: "app" });
    expect(np).toMatchObject({ service: "apple", player: "app", connected: false, playing: false });
  });

  it("carries the Spotify item's link and what Spotify allows now into the card", async () => {
    const np = await loadWithNowPlaying({
      connected: true,
      service: "spotify",
      playing: true,
      track: "So What",
      artist: "Miles Davis",
      link: "https://open.spotify.com/track/abc",
      can: { pause: false, resume: true, next: true, previous: true },
    });
    expect(np.link).toBe("https://open.spotify.com/track/abc");
    expect(np.can).toEqual({ pause: false, resume: true, next: true, previous: true });
  });

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
    // Raw milliseconds too, for mm:ss display.
    expect(np.progressMs).toBe(60_000);
    expect(np.durationMs).toBe(200_000);
  });

  /** Null, never 0 — a zero here would be read as the start of a track. */
  it("leaves the milliseconds null when Spotify did not send them", async () => {
    const idle = await loadWithNowPlaying({ connected: true, playing: false });
    expect(idle.progressMs).toBeNull();
    expect(idle.durationMs).toBeNull();

    const off = await loadWithNowPlaying({ connected: false });
    expect(off.progressMs).toBeNull();
    expect(off.durationMs).toBeNull();
    // No borrowed track: a fake title would pass for working playback.
    expect(off.track).toBe("");
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
