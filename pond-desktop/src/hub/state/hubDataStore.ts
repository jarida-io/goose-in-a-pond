import { useSyncExternalStore } from "react";
import { api } from "../../api/PondApiClient";
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
  HOME as MOCK_HOME,
  type CameraData,
  type CategoryData,
  type DeviceData,
  type DeviceKind,
  type HomeData,
  type NowPlayingData,
  type RoomData,
  type SceneData,
  type WeatherData,
} from "../data/mockHome";
import { ROUTINES as MOCK_ROUTINES, type RoutineDetail } from "../data/routines";
import { sunEl, filmEl, focusEl } from "../primitives/HubIco";
import { HP_PATHS } from "../primitives/icons";

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
  data: MOCK_HOME,
  routines: MOCK_ROUTINES,
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

function deviceFromApi(d: Device, kind: DeviceKind): DeviceData {
  const meta = d.metadata ?? {};
  const on = typeof meta.on === "boolean" ? meta.on : kind === "light" ? false : true;
  const locked = typeof meta.locked === "boolean" ? meta.locked : true;
  const target = typeof meta.target === "number" ? meta.target : 70;
  const value = typeof meta.value === "number" ? meta.value : kind === "thermo" ? 68 : undefined;
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

function deriveCategories(devs: DeviceData[], cams: CameraData[]): CategoryData[] {
  const out: CategoryData[] = [];
  // Security pinned first (no device backing yet)
  out.push({
    id: "security", label: "Security", status: "Disarmed",
    icon: "shieldCheck", color: "#16A34A", bg: "#DCFCE7",
  });

  const byKind: Record<DeviceKind, DeviceData[]> = { light: [], lock: [], thermo: [], plug: [], other: [] };
  for (const d of devs) byKind[d.kind].push(d);

  if (byKind.lock.length) {
    const locked = byKind.lock.filter((d) => d.locked).length;
    out.push({
      ...CATEGORY_TEMPLATE.lock,
      status: locked === byKind.lock.length ? "All locked" : `${locked}/${byKind.lock.length} locked`,
    });
  }
  if (byKind.thermo.length) {
    const t = byKind.thermo[0];
    out.push({ ...CATEGORY_TEMPLATE.thermo, status: `Heat to ${t.target ?? 70}°` });
  }
  if (byKind.light.length) {
    const on = byKind.light.filter((d) => d.on).length;
    out.push({ ...CATEGORY_TEMPLATE.light, status: `${on} on` });
  }
  if (cams.length) {
    out.push({
      id: "cameras", label: "Cameras", status: `${cams.length} live`,
      icon: "cctv", color: "#7C3AED", bg: "#EDE9FE",
    });
  }
  if (byKind.plug.length) {
    const on = byKind.plug.filter((d) => d.on).length;
    out.push({ ...CATEGORY_TEMPLATE.plug, status: `${on} on` });
  }
  return out;
}

const SCENE_ICONS = ["sun", "moon", "film", "away", "focus"];

function scenesFromSchedules(schedules: Schedule[]): SceneData[] {
  if (!schedules.length) return MOCK_HOME.scenes;
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

function routinesFromRecipes(recipes: AgentRecipe[]): RoutineDetail[] {
  if (!recipes.length) return MOCK_ROUTINES;
  return recipes.map((r) => {
    const known = MOCK_ROUTINES.find((m) => m.name.toLowerCase() === r.name.toLowerCase());
    const template = known ?? {
      iconPath: ROUTINE_TEMPLATES[recipeIdHash(r.name) % ROUTINE_TEMPLATES.length].iconPath,
      color:    ROUTINE_TEMPLATES[recipeIdHash(r.name) % ROUTINE_TEMPLATES.length].color,
      bg:       ROUTINE_TEMPLATES[recipeIdHash(r.name) % ROUTINE_TEMPLATES.length].bg,
    };
    const desc = (r.description ?? "").trim();
    const does = known
      ? known.does
      : desc
        ? desc.split(/[,;]/).map((s) => s.trim()).filter(Boolean).slice(0, 4)
        : ["On demand"];
    return {
      id:       r.name as RoutineDetail["id"],
      name:     known?.name ?? r.name,
      iconPath: template.iconPath,
      color:    template.color,
      bg:       template.bg,
      does:     does.length ? does : ["On demand"],
      time:     known?.time ?? "On demand",
      prompt:   known?.prompt ?? `Run routine: ${r.name}`,
    };
  });
}

function weatherFromApi(w: WeatherApiResponse | null): WeatherData {
  if (!w || !w.enabled) return MOCK_HOME.weather;
  return {
    temp: w.temp ?? MOCK_HOME.weather.temp,
    cond: w.cond ?? MOCK_HOME.weather.cond,
    icon: w.icon ?? MOCK_HOME.weather.icon,
    hi: w.hi ?? MOCK_HOME.weather.hi,
    lo: w.lo ?? MOCK_HOME.weather.lo,
    hum: w.hum ?? MOCK_HOME.weather.hum,
    wind: w.wind ?? MOCK_HOME.weather.wind,
    sunrise: w.sunrise ?? MOCK_HOME.weather.sunrise,
    sunset: w.sunset ?? MOCK_HOME.weather.sunset,
    forecast: w.forecast?.length
      ? (w.forecast as WeatherData["forecast"])
      : MOCK_HOME.weather.forecast,
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
  if (!np || !np.connected) return { ...MOCK_HOME.nowPlaying, connected: false };
  // Spotify refused: show that, not an idle player, since the user has to act on it.
  if (np.error) {
    return {
      track: np.error === "forbidden" ? "Spotify not authorised" : "Spotify unavailable",
      artist: np.message || "",
      elapsed: 0,
      hue: MOCK_HOME.nowPlaying.hue,
      connected: true,
      playing: false,
      error: np.error,
      message: np.message,
    };
  }
  // Connected but nothing playing (Spotify's 204): an honest idle state, never the demo track.
  const progress = np.progress_ms ?? 0;
  const duration = np.duration_ms ?? 0;
  return {
    track: np.track || "Nothing playing",
    artist: np.artist || "",
    elapsed: duration > 0 ? progress / duration : 0,
    hue: MOCK_HOME.nowPlaying.hue,
    albumArt: np.album_art,
    connected: true,
    playing: np.playing ?? false,
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
    const wOK = weather.status === "fulfilled" ? weather.value : null;
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

    // If backend has no devices at all, keep mock devices/cameras as a friendly demo.
    const useMockDevices = ctlDevices.length === 0 && cams.length === 0;
    const finalDevices = useMockDevices ? MOCK_HOME.devices : ctlDevices;
    const finalCameras = useMockDevices ? MOCK_HOME.cameras : cams;

    const rooms = useMockDevices ? MOCK_HOME.rooms : deriveRooms(finalDevices);
    const categories = useMockDevices
      ? MOCK_HOME.categories
      : deriveCategories(finalDevices, finalCameras);

    const userName = (sOK?.user_name && sOK.user_name.trim()) || MOCK_HOME.user;
    const scenes = scenesFromSchedules(schOK);

    state.data = {
      ...MOCK_HOME,
      user: userName,
      devices: finalDevices,
      cameras: finalCameras,
      rooms,
      categories,
      scenes,
      weather: weatherFromApi(wOK),
      nowPlaying: nowPlayingFromApi(npOK),
    };
    state.routines = routinesFromRecipes(rcOK);
    state.loaded = true;
    emit();
  } catch {
    // keep mock fallback
  } finally {
    state.loading = false;
  }
}

/** The server caches upstream weather for 15 minutes, so most of these polls are answered locally. */
const WEATHER_POLL_MS = 10 * 60_000;

// Kick off load once on first import in a browser; safe to call again.
if (typeof window !== "undefined") {
  // Fire-and-forget; UI renders mock until load resolves.
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
    state.data = { ...state.data, weather: weatherFromApi(w) };
    emit();
  } catch {
    // keep the last known reading rather than blanking the card
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
  state.data = MOCK_HOME;
  state.routines = MOCK_ROUTINES;
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
