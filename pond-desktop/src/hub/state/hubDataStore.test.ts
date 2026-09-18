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

  /**
   * The demo house is gone, and this is the test that used to require it.
   *
   * A pond with nothing paired was handed ten invented devices, three cameras
   * and six rooms — which made the screen a new household meets the one screen
   * guaranteed to be false. Zero devices is a state, and every surface that
   * shows them has an empty state for it.
   */
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

  /**
   * And nothing is on the screen before the first answer lands, either.
   *
   * The seed used to be the demo house, which every consumer then had to
   * remember to suppress by checking `devicesAreReal`. The drawer did not, so
   * it listed Living Room, Kitchen, Bedroom, Office and Outdoor to households
   * that owned none of them, for however long the six requests took — 30s per
   * abort, 190s if a re-pair runs, and for the whole session if `load()` threw.
   * An empty seed cannot be mistaken for a house by anybody, guard or no guard.
   */
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

  /**
   * The device list carries identity and capabilities and no state whatsoever
   * (`routes.rs` list_devices), so these four fields have nothing behind them.
   * They used to default — and `locked: true` in particular made "All locked"
   * unfalsifiable: no real-world door could change what the panel said.
   */
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

  /** The chips read off those same fields, so they say so rather than guess. */
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
    // The Security chip is gone with it: it was pinned first and hardcoded to
    // "Disarmed", which is an alarm state read off a placeholder.
    expect(byId.security).toBeUndefined();
  });

  /** One silent lock is enough to stop the chip saying "All". */
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

  /**
   * Scenes are the household's schedules. An empty schedule list used to return
   * the demo file's five, so a pond that had never been given one showed five
   * tappable scenes.
   */
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

  /**
   * Weather off used to render a mock 64° / Partly cloudy / H68 L54 and a
   * Tue-Wed-Thu strip, which is a forecast for a place the pond does not know.
   * Now the slice is zeroed and `weatherEnabled` is false, and Home draws a
   * "set your location" panel where the card would be.
   */
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

  /**
   * The per-field fallbacks were the same fabrication at smaller scale: a real
   * answer missing a high and low reported the demo numbers beside a real
   * temperature, which is harder to spot and no more true.
   */
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

  /**
   * A 502 is not a household decision.
   *
   * `GET /api/v1/weather` answers 200 `{enabled:false}` only when the pond has
   * no provider configured at all; once a location is set, an upstream failure
   * or a PAI-2 egress refusal is a 502 with a message saying which. The client
   * throws on that, `Promise.allSettled` flattened it to null, and null became
   * `weatherEnabled: false` — so the card told a household whose location was
   * already set to go and set their location, and tapping through to Settings
   * and saving fixed nothing, because nothing was unset.
   */
  it("does not report a failed weather fetch as weather being off", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);
    apiMock.getWeather.mockRejectedValue(new Error("502 Failed to fetch weather"));

    await refreshHomeData();
    expect(getHomeData().weatherStatus).toBe("unreachable");
  });

  /** The household's own "off" still reads as off, and is still distinguishable. */
  it("reports weather being switched off as off", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);
    apiMock.getWeather.mockResolvedValue({ enabled: false });

    await refreshHomeData();
    expect(getHomeData().weatherStatus).toBe("off");
  });

  /**
   * And a failed reload does not wipe a reading that was right all day. `load()`
   * rebuilds every field, so one 502 during a reconnect used to replace a live
   * 71 degrees with the "set your location" panel — which the ten-minute poll
   * cannot undo, because it only ever keeps the last slice.
   */
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
    // Kept, and said to be kept: a card may go on showing the last reading, but
    // nothing may present it as current.
    expect(getHomeData().weatherStatus).toBe("unreachable");
  });

  /**
   * A pond with no recipes has no routines.
   *
   * It used to have five — Good Morning, Good Night, Movie Time, Away, Focus —
   * on every fresh install and every unreachable server, listed in the drawer
   * and on Routines with a control that ran them. Tapping one wrote the fixture
   * into the household's real `agent_recipes` and sent its prompt to the agent,
   * in a house that may have had nothing paired.
   */
  it("shows no routines when the pond has no recipes", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);
    apiMock.listRecipes.mockResolvedValue([]);

    await refreshHomeData();
    expect(__getRoutinesForTests()).toEqual([]);
  });

  /** Offline is the same answer: no recipes came back, so there are no routines. */
  it("shows no routines when the recipe call fails", async () => {
    apiMock.getSettings.mockResolvedValue({ user_name: "Ada", assistant_name: "Goose", prompt_style: "balanced" });
    apiMock.listDevices.mockResolvedValue([]);
    apiMock.listSchedules.mockResolvedValue([]);
    apiMock.listRecipes.mockRejectedValue(new Error("server offline"));

    await refreshHomeData();
    expect(__getRoutinesForTests()).toEqual([]);
  });

  /** And nothing is seeded before the first load, either. */
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
    // A recipe whose name happens to match a fixture keeps its OWN description
    // and gets no schedule. It used to be relabelled "7:00 AM · weekdays" and
    // given four actions off a file, with "wake up macro" thrown away.
    const morning = snapshot.find((r) => r.name === "Good Morning");
    expect(morning?.does).toEqual(["wake up macro"]);
    expect(morning?.time).toBe("On demand");
    // Every other recipe derives its chips from its description, as before.
    const sunset = snapshot.find((r) => r.name === "Sunset Bath");
    expect(sunset?.does).toEqual(["Run tub", "dim lights", "play jazz"]);
    expect(sunset?.time).toBe("On demand");
  });

  // ── Now Playing ──────────────────────────────────────────────
  // Spotify refusing a call must never look like a paused player: a
  // development-mode app serves only allowlisted accounts, and everyone else
  // completes the whole OAuth flow before every API call 403s.

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
    // The raw milliseconds too: the fraction cannot be turned back into mm:ss,
    // so a card that wants to say 1:00 of 3:20 needs both of these.
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
    // And no borrowed track. A fresh install showed "Weightless / Marconi
    // Union" here, which is exactly the state most likely to be mistaken for
    // working playback.
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

  /// The widget polls every ten seconds and the dashboard is left open for
  /// days, so an answer that cannot change without somebody doing something
  /// costs ~8,600 requests a day — each one a round trip the server makes to
  /// Spotify on our behalf. After five consecutive 4XX answers it stops
  /// asking entirely, and only an interaction brings it back.

  const REFUSAL = { connected: true, error: "forbidden", upstream_status: 403 };

  async function answer(np: unknown, times = 1) {
    apiMock.getNowPlaying.mockResolvedValue(np);
    for (let i = 0; i < times; i += 1) await refreshNowPlaying();
  }

  it("keeps polling through the first four refusals", async () => {
    // Four is not five. Stopping early would give up on a service that was
    // about to answer.
    await answer(REFUSAL, 4);
    expect(__tickNowPlayingPollForTests()).toBe(true);
  });

  it("stops asking after five consecutive 4XX answers", async () => {
    await answer(REFUSAL, 5);
    expect(__tickNowPlayingPollForTests()).toBe(false);
    // Still stopped on later ticks: unlike the old slow-retry, no tick leaks
    // through, because the condition cannot clear on its own.
    expect(__tickNowPlayingPollForTests()).toBe(false);
    expect(__tickNowPlayingPollForTests()).toBe(false);
  });

  it("only counts real 4XX answers", async () => {
    // `unavailable` covers 5xx, and a transport failure has no status at all.
    // Counting either would let a Spotify outage — or this pond restarting
    // mid-poll — permanently silence a widget whose recovery needs a person.
    await answer({ connected: true, error: "unavailable", upstream_status: 502 }, 5);
    expect(__tickNowPlayingPollForTests()).toBe(true);

    __resetHubDataForTests();
    await answer(null, 5);
    expect(__tickNowPlayingPollForTests()).toBe(true);
  });

  it("needs the five to be consecutive", async () => {
    // Five refusals spread across a week of healthy polling are not a reason
    // to stop; one good answer means the service is reachable.
    await answer(REFUSAL, 4);
    await answer({ connected: true, playing: true, track: "x" });
    await answer(REFUSAL, 4);
    expect(__tickNowPlayingPollForTests()).toBe(true);
  });

  it("resumes when the widget is used", async () => {
    // The stop rule is only safe because this exists. `refreshNowPlaying` is
    // the widget's own Try again, and pressing it means somebody is watching.
    await answer(REFUSAL, 5);
    expect(__tickNowPlayingPollForTests()).toBe(false);

    // The widget's own Try again — the `userInitiated` path. The automatic
    // tick calls the same function and must NOT resume, or the breaker could
    // never trip.
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
