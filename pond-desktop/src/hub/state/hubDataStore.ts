import { useSyncExternalStore } from "react";
import { api } from "../../api/PondApiClient";
import { isAgentYaml } from "../../agents/agentModel";
import type {
  AgentRecipe,
  Device,
  MusicControlAction,
  NowPlayingApiResponse,
  Schedule,
  Settings,
  WeatherApiResponse,
} from "../../api/types";
import {
  EMPTY_HOME,
  NO_WEATHER,
  type CameraData,
  type CategoryData,
  type DeviceData,
  type DeviceKind,
  type HomeData,
  type NowPlayingData,
  type RoomData,
  type SceneData,
  type WeatherData,
  type WeatherStatus,
} from "../data/mockHome";
import { type RoutineDetail } from "../data/routines";
import { HP_PATHS, sunEl, filmEl, focusEl } from "../primitives/icons";

// Reactive store of real PondApiClient data in the HomeData shape; mock data while the API is offline.

type Subscriber = () => void;

interface InternalState {
  data: HomeData;
  routines: RoutineDetail[];
  loaded: boolean;
  loading: boolean;
  subs: Set<Subscriber>;
}

const state: InternalState = {
  data: EMPTY_HOME,
  routines: [],
  loaded: false,
  loading: false,
  subs: new Set(),
};

function emit() {
  state.subs.forEach((f) => f());
}

function subscribe(f: Subscriber): () => void {
  state.subs.add(f);
  return () => state.subs.delete(f);
}

function getSnapshot(): HomeData {
  return state.data;
}

// ─── Mappers ──────────────────────────────────────────────────

const KIND_FROM_TYPE: Record<string, DeviceKind> = {
  light: "light",
  smart_light: "light",
  bulb: "light",
  lamp: "light",
  lock: "lock",
  smart_lock: "lock",
  thermo: "thermo",
  thermostat: "thermo",
  hvac: "thermo",
  plug: "plug",
  outlet: "plug",
  smart_plug: "plug",
};

function inferKind(d: Device): DeviceKind | "camera" {
  const t = (d.device_type ?? "").toLowerCase();
  if (t === "camera") return "camera";
  if (KIND_FROM_TYPE[t]) return KIND_FROM_TYPE[t];
  const meta = d.metadata ?? {};
  const mk = typeof meta.kind === "string" ? meta.kind.toLowerCase() : "";
  if (mk === "camera") return "camera";
  if (KIND_FROM_TYPE[mk]) return KIND_FROM_TYPE[mk];
  // Unrecognised types (host, sensor, gotg, …) still get a room tile, as a generic kind.
  return "other";
}

/**
 * A device with only the state something reported. `GET /api/v1/devices` sends none, so fields
 * stay undefined ("not reported", never off/locked); live state is `HomeControlsCard`'s MCP read.
 */
function deviceFromApi(d: Device, kind: DeviceKind): DeviceData {
  const meta = d.metadata ?? {};
  const on = typeof meta.on === "boolean" ? meta.on : undefined;
  const locked = typeof meta.locked === "boolean" ? meta.locked : undefined;
  const target = typeof meta.target === "number" ? meta.target : undefined;
  const value = typeof meta.value === "number" ? meta.value : undefined;
  return {
    id: d.id,
    name: d.name,
    kind,
    subtype: d.device_type,
    on,
    locked,
    target,
    value,
    room: d.room ?? "Home",
    // Home offers a switch only to devices whose capabilities allow one.
    capabilities: d.capabilities,
  };
}

function hueForId(id: string): number {
  let h = 0;
  for (let i = 0; i < id.length; i++) h = (h * 31 + id.charCodeAt(i)) & 0xffff;
  return h % 360;
}

function cameraFromApi(d: Device): CameraData {
  const seen = d.last_seen ? new Date(d.last_seen) : new Date();
  const time = seen.toLocaleTimeString([], { hour: "numeric", minute: "2-digit" });
  return { id: d.id, name: d.name, time, hue: hueForId(d.id) };
}

function deriveRooms(devs: DeviceData[]): RoomData[] {
  // Always include "Home" first; then unique device.room values in their natural order.
  const seen = new Map<string, RoomData>();
  seen.set("home", { id: "home", name: "Home", icon: "home" });
  const ICON_FOR: Record<string, string> = {
    "Living Room": "sofa",
    "Kitchen":     "utensils",
    "Bedroom":     "bed",
    "Office":      "briefcase",
    "Outdoor":     "tree",
    "Garage":      "tree",
    "Bathroom":    "tree",
  };
  for (const d of devs) {
    const name = d.room;
    if (!name || name === "Home") continue;
    const id = name.toLowerCase().replace(/\s+/g, "");
    if (seen.has(id)) continue;
    seen.set(id, { id, name, icon: ICON_FOR[name] ?? "home" });
  }
  return Array.from(seen.values());
}

const CATEGORY_TEMPLATE: Record<Exclude<DeviceKind, "other">, Omit<CategoryData, "status">> = {
  light:  { id: "lights",  label: "Lights",   icon: "bulb",   color: "#D97706", bg: "#FEF3C7" },
  lock:   { id: "locks",   label: "Locks",    icon: "lock",   color: "#2563EB", bg: "#DBEAFE" },
  thermo: { id: "climate", label: "Climate",  icon: "thermo", color: "#EA580C", bg: "#FFEDD5" },
  plug:   { id: "plugs",   label: "Plugs",    icon: "plug",   color: "#0D9488", bg: "#CCFBF1" },
};

/** Chip text when nothing under it reported state; "0 on" or "All locked" would be a guess. */
const NOT_REPORTED = "Not reported";

/** The status line for "how many of these are on", said only about the ones that said. */
function powerStatus(devs: DeviceData[]): string {
  const known = devs.filter((d) => typeof d.on === "boolean");
  const on = known.filter((d) => d.on).length;
  if (known.length === 0) return NOT_REPORTED;
  if (known.length === devs.length) return `${on} on`;
  return `${on} on, ${devs.length - known.length} unknown`;
}

function deriveCategories(devs: DeviceData[], cams: CameraData[]): CategoryData[] {
  const out: CategoryData[] = [];
  // No Security chip until something reports alarm state: never show that from a placeholder.

  const byKind: Record<DeviceKind, DeviceData[]> = { light: [], lock: [], thermo: [], plug: [], other: [] };
  for (const d of devs) byKind[d.kind].push(d);

  if (byKind.lock.length) {
    const known = byKind.lock.filter((d) => typeof d.locked === "boolean");
    const locked = known.filter((d) => d.locked).length;
    // "All locked" needs every lock to have reported; one silent lock is a door nobody read.
    const status =
      known.length === 0                                             ? NOT_REPORTED
      : known.length === byKind.lock.length && locked === known.length ? "All locked"
      : `${locked}/${known.length} locked`;
    out.push({ ...CATEGORY_TEMPLATE.lock, status });
  }
  if (byKind.thermo.length) {
    const t = byKind.thermo[0];
    out.push({
      ...CATEGORY_TEMPLATE.thermo,
      status: typeof t.target === "number" ? `Heat to ${t.target}°` : NOT_REPORTED,
    });
  }
  if (byKind.light.length) {
    out.push({ ...CATEGORY_TEMPLATE.light, status: powerStatus(byKind.light) });
  }
  if (cams.length) {
    // "paired", not "live": the device list knows registrations, not streams.
    out.push({
      id: "cameras", label: "Cameras", status: `${cams.length} paired`,
      icon: "cctv", color: "#7C3AED", bg: "#EDE9FE",
    });
  }
  if (byKind.plug.length) {
    out.push({ ...CATEGORY_TEMPLATE.plug, status: powerStatus(byKind.plug) });
  }
  return out;
}

const SCENE_ICONS = ["sun", "moon", "film", "away", "focus"];

function scenesFromSchedules(schedules: Schedule[]): SceneData[] {
  return schedules.slice(0, 5).map((s, i) => ({
    id: s.id,
    name: s.label ?? s.name,
    icon: SCENE_ICONS[i] ?? "sun",
    active: i === 0,
  }));
}

// ─── Recipe → RoutineDetail mapping ───────────────────────────

const ROUTINE_TEMPLATES: Array<Omit<RoutineDetail, "id" | "name" | "does" | "time">> = [
  { iconPath: sunEl,         color: "#F59E0B", bg: "linear-gradient(150deg,#FCD34D,#F59E0B)", prompt: "" },
  { iconPath: HP_PATHS.moon, color: "#6366F1", bg: "linear-gradient(150deg,#818CF8,#4F46E5)", prompt: "" },
  { iconPath: filmEl,        color: "#7C3AED", bg: "linear-gradient(150deg,#A78BFA,#7C3AED)", prompt: "" },
  { iconPath: HP_PATHS.away, color: "#0D9488", bg: "linear-gradient(150deg,#2DD4BF,#0D9488)", prompt: "" },
  { iconPath: focusEl,       color: "#EC4899", bg: "linear-gradient(150deg,#F472B6,#DB2777)", prompt: "" },
];

function recipeIdHash(name: string): number {
  let h = 0;
  for (let i = 0; i < name.length; i++) h = (h * 31 + name.charCodeAt(i)) & 0xffff;
  return h;
}

/** Recipes as routine cards. Icon and colours are hash-picked, so stable across launches. */
function routinesFromRecipes(recipes: AgentRecipe[]): RoutineDetail[] {
  // An agent starts from a schedule, an event or a hand-off, so it is no one-tap routine.
  return recipes.filter((r) => !isAgentYaml(r.yaml)).map((r) => {
    const template = ROUTINE_TEMPLATES[recipeIdHash(r.name) % ROUTINE_TEMPLATES.length];
    // The chips are the recipe's own description, split the way it was written.
    const desc = (r.description ?? "").trim();
    const does = desc
      ? desc.split(/[,;]/).map((s) => s.trim()).filter(Boolean).slice(0, 4)
      : [];
    return {
      id:       r.name as RoutineDetail["id"],
      name:     r.name,
      iconPath: template.iconPath,
      color:    template.color,
      bg:       template.bg,
      does:     does.length ? does : ["On demand"],
      // Recipes have no schedule.
      time:     "On demand",
      prompt:   `Run routine: ${r.name}`,
    };
  });
}

/** Branch copy on this, not `weatherEnabled`: "off" and "unreachable" need different words. */
function weatherStatusFor(answered: boolean, w: WeatherApiResponse | null): WeatherStatus {
  if (!answered) return "unreachable";
  return w?.enabled ? "on" : "off";
}

/** Missing fields zero out and a missing forecast draws no strip; never borrow demo values. */
function weatherFromApi(w: WeatherApiResponse | null): WeatherData {
  if (!w || !w.enabled) return NO_WEATHER;
  return {
    temp: w.temp ?? 0,
    cond: w.cond ?? "",
    icon: w.icon ?? "",
    hi: w.hi ?? 0,
    lo: w.lo ?? 0,
    hum: w.hum ?? 0,
    wind: w.wind ?? 0,
    sunrise: w.sunrise ?? "",
    sunset: w.sunset ?? "",
    forecast: (w.forecast as WeatherData["forecast"] | undefined) ?? [],
  };
}

/** Why the now-playing poll backed off, or null. Module state: in `state.data` it would re-render the dashboard. */
let nowPlayingBackoff: string | null = null;

/** Ticks elapsed since the last attempt while backed off. */
let backoffTicks = 0;

/** How often the widget asks when everything is healthy. */
const NOW_PLAYING_TICK_MS = 10_000;

/** Ticks to skip between attempts while backed off: five minutes at the tick above. */
const BACKOFF_TICKS = 30;

/** Consecutive 4XX answers before the poll stops: a Spotify refusal waits on a person, not a timer. */
const STOP_AFTER_4XX = 5;

/** Consecutive 4XX answers seen so far. */
let fourXxRun = 0;

/** True once the run hit the limit; only an interaction clears it. */
let nowPlayingStopped = false;

/** A real Spotify 4XX; `upstream_status` is set only when Spotify answered, so outages never count. */
function isClientRefusal(np: NowPlayingApiResponse | null): boolean {
  const status = np?.upstream_status;
  return typeof status === "number" && status >= 400 && status < 500;
}

/** Clears a stop, which can't clear on its own; call it when a person engages the widget. */
export function resumeNowPlayingPolling(): void {
  fourXxRun = 0;
  nowPlayingStopped = false;
  nowPlayingBackoff = null;
  backoffTicks = 0;
}

function dueForNowPlayingPoll(): boolean {
  // A stop lets no tick through; only `resumeNowPlayingPolling` clears it.
  if (nowPlayingStopped) return false;
  if (!nowPlayingBackoff) return true;
  backoffTicks += 1;
  if (backoffTicks < BACKOFF_TICKS) return false;
  backoffTicks = 0;
  return true;
}

/** Counts consecutive 4XX toward a stop (logged once); any other answer resets the run. */
function setNowPlayingBackoff(np: NowPlayingApiResponse | null): void {
  if (isClientRefusal(np)) {
    fourXxRun += 1;
    if (fourXxRun >= STOP_AFTER_4XX && !nowPlayingStopped) {
      nowPlayingStopped = true;
      nowPlayingBackoff = np?.error ?? "client_refusal";
      console.info(
        `Now-playing polling stopped after ${STOP_AFTER_4XX} consecutive ` +
          `${np?.upstream_status} answers: ${np?.error}. It resumes when you use the ` +
          `widget, or when the pond next talks to the music service.`,
      );
    }
    return;
  }
  fourXxRun = 0;
}

function nowPlayingFromApi(np: NowPlayingApiResponse | null): NowPlayingData {
  // Apple Music chosen: the assistant and its page play it, so the card says where, not what.
  if (np?.service === "apple") {
    return {
      track: "",
      artist: "",
      elapsed: 0,
      hue: EMPTY_HOME.nowPlaying.hue,
      connected: false,
      playing: false,
      progressMs: null,
      durationMs: null,
      service: "apple",
      player: np.player ?? "page",
    };
  }
  if (!np || !np.connected) {
    // Blank, never a mock track: that would pass for working playback.
    return {
      track: "",
      artist: "",
      elapsed: 0,
      hue: EMPTY_HOME.nowPlaying.hue,
      connected: false,
      playing: false,
      progressMs: null,
      durationMs: null,
    };
  }
  // Spotify refused: show that, not an idle player, since the user has to act on it.
  if (np.error) {
    return {
      track: np.error === "forbidden" ? "Spotify not authorised" : "Spotify unavailable",
      artist: np.message || "",
      elapsed: 0,
      hue: EMPTY_HOME.nowPlaying.hue,
      connected: true,
      playing: false,
      error: np.error,
      message: np.message,
      progressMs: null,
      durationMs: null,
    };
  }
  // Connected but nothing playing (Spotify's 204): an honest idle state, never the demo track.
  const progress = np.progress_ms ?? 0;
  const duration = np.duration_ms ?? 0;
  return {
    track: np.track || "Nothing playing",
    artist: np.artist || "",
    elapsed: duration > 0 ? progress / duration : 0,
    hue: EMPTY_HOME.nowPlaying.hue,
    albumArt: np.album_art,
    connected: true,
    playing: np.playing ?? false,
    // For mm:ss (the fraction above can't give it); null, not 0, when Spotify omits them.
    progressMs: typeof np.progress_ms === "number" ? np.progress_ms : null,
    durationMs: typeof np.duration_ms === "number" ? np.duration_ms : null,
    link: np.link ?? null,
    ...(np.can ? { can: np.can } : {}),
  };
}

// ─── Loader ───────────────────────────────────────────────────

async function load() {
  if (state.loading) return;
  state.loading = true;
  try {
    const [settings, devices, schedules, recipes, weather, nowPlaying] = await Promise.allSettled([
      api.getSettings(),
      api.listDevices(),
      api.listSchedules(),
      api.listRecipes(),
      api.getWeather(),
      api.getNowPlaying(),
    ]);

    const sOK = settings.status === "fulfilled" ? (settings.value as Settings) : null;
    const dOK = devices.status === "fulfilled" ? devices.value : [];
    const schOK = schedules.status === "fulfilled" ? schedules.value : [];
    const rcOK = recipes.status === "fulfilled" ? recipes.value : [];
    // A rejected fetch is "unreachable", not `{enabled:false}`, and keeps the last reading.
    const weatherAnswered = weather.status === "fulfilled";
    const wOK = weatherAnswered ? weather.value : null;
    const npOK = nowPlaying.status === "fulfilled" ? nowPlaying.value : null;
    // The load's answer counts toward the 4XX run like any poll's.
    setNowPlayingBackoff(npOK);

    const ctlDevices: DeviceData[] = [];
    const cams: CameraData[] = [];
    for (const d of dOK) {
      const k = inferKind(d);
      if (k === "camera") cams.push(cameraFromApi(d));
      else ctlDevices.push(deviceFromApi(d, k));
    }

    const userName = (sOK?.user_name && sOK.user_name.trim()) || EMPTY_HOME.user;

    // Field by field, never spread over an older object: a spread keeps slices nobody asked for.
    state.data = {
      user: userName,
      devices: ctlDevices,
      cameras: cams,
      rooms: deriveRooms(ctlDevices),
      categories: deriveCategories(ctlDevices, cams),
      scenes: scenesFromSchedules(schOK),
      weather: weatherAnswered ? weatherFromApi(wOK) : state.data.weather,
      weatherEnabled: weatherAnswered ? Boolean(wOK?.enabled) : state.data.weatherEnabled,
      weatherStatus: weatherStatusFor(weatherAnswered, wOK),
      devicesAreReal: true,
      nowPlaying: nowPlayingFromApi(npOK),
    };
    state.routines = routinesFromRecipes(rcOK);
    state.loaded = true;
    emit();
  } catch {
    // Keep what the last successful load left (the empty home before the first).
  } finally {
    state.loading = false;
  }
}

/** The server caches upstream weather for 15 minutes, so most of these polls are answered locally. */
const WEATHER_POLL_MS = 10 * 60_000;

// Kick off load once on first import in a browser; safe to call again.
if (typeof window !== "undefined") {
  // Fire-and-forget; the UI renders the empty home until load resolves.
  void load();
  // Playback changes outside the app, so poll it.
  setInterval(() => {
    if (dueForNowPlayingPoll()) void refreshNowPlaying();
  }, NOW_PLAYING_TICK_MS);
  // A GIAP dashboard stays open for days, so the weather must refresh itself.
  setInterval(() => {
    void refreshWeather();
  }, WEATHER_POLL_MS);
  // Timers don't fire while asleep or hidden; catch up when the dashboard is seen again.
  document.addEventListener("visibilitychange", () => {
    if (document.visibilityState !== "visible") return;
    void refreshWeather();
    void refreshNowPlaying();
  });
}

// ─── Public API ───────────────────────────────────────────────

export function useHomeData(): HomeData {
  return useSyncExternalStore(subscribe, getSnapshot, getSnapshot);
}

export function getHomeData(): HomeData {
  return state.data;
}

function getRoutinesSnapshot(): RoutineDetail[] {
  return state.routines;
}

export function useRoutines(): RoutineDetail[] {
  return useSyncExternalStore(subscribe, getRoutinesSnapshot, getRoutinesSnapshot);
}

export function refreshHomeData(): Promise<void> {
  return load();
}

/** Re-fetches just the weather slice, without the full dashboard reload. */
export async function refreshWeather(): Promise<void> {
  try {
    const w = await api.getWeather();
    state.data = {
      ...state.data,
      weather: weatherFromApi(w),
      weatherEnabled: Boolean(w?.enabled),
      weatherStatus: weatherStatusFor(true, w),
    };
    emit();
  } catch {
    // Keep the last reading but mark it "unreachable", so it isn't presented as current.
    state.data = { ...state.data, weatherStatus: "unreachable" };
    emit();
  }
}

/**
 * Fetches now-playing even while the poll is stopped. Only a person's Try again may pass
 * `userInitiated`: it resumes the poll, and the timer calls this too.
 */
export async function refreshNowPlaying(userInitiated = false): Promise<void> {
  if (userInitiated) resumeNowPlayingPolling();
  try {
    const np = await api.getNowPlaying();
    state.data = { ...state.data, nowPlaying: nowPlayingFromApi(np) };
    setNowPlayingBackoff(np);
    emit();
  } catch {
    // Server unreachable, not Spotify refusing: keep the last state and keep polling.
  }
}

/** Sends a playback control action, then re-syncs from Spotify's actual state. */
export async function controlNowPlaying(action: MusicControlAction): Promise<void> {
  // Someone is using the widget, so a stopped poll resumes.
  resumeNowPlayingPolling();
  try {
    await api.controlMusic(action);
  } catch {
    // ignore — Spotify may report no active device etc; nothing more to do here
  }
  await refreshNowPlaying();
}

/** Test hook: reset to mock data and clear the poll state. */
export function __resetHubDataForTests(): void {
  state.data = EMPTY_HOME;
  state.routines = [];
  state.loaded = false;
  state.loading = false;
  // Poll state is module state and would otherwise leak into the next test.
  nowPlayingBackoff = null;
  backoffTicks = 0;
  resumeNowPlayingPolling();
}

/** Test hook: why the now-playing poll is slowed, or null at full rate. */
export function __nowPlayingBackoffForTests(): string | null {
  return nowPlayingBackoff;
}

/** Test hook: run one poll tick's decision, counter and all. */
export function __tickNowPlayingPollForTests(): boolean {
  return dueForNowPlayingPoll();
}

/** Test hook: ticks skipped between attempts while backed off. */
export const __BACKOFF_TICKS_FOR_TESTS = BACKOFF_TICKS;

export function __getRoutinesForTests(): RoutineDetail[] {
  return state.routines;
}
