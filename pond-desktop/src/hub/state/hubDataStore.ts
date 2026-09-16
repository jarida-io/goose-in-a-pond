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
import { sunEl, filmEl, focusEl } from "../primitives/HubIco";
import { HP_PATHS } from "../primitives/icons";

// Reactive store that loads real data from PondApiClient and exposes it in the
// HomeData shape used by Hub primitives.
//
// It does NOT fall back to fixtures, in any state. It used to — the seed was
// the demo house, an empty recipe list became five invented routines, and an
// empty schedule list became five invented scenes — and the effect was that
// the screen a household could least afford to have wrong was the one screen
// guaranteed to be wrong. Every slice here is either something the pond said
// or an empty one, and the surfaces above have an empty state for each.

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
  // Try metadata.kind
  const meta = d.metadata ?? {};
  const mk = typeof meta.kind === "string" ? meta.kind.toLowerCase() : "";
  if (mk === "camera") return "camera";
  if (KIND_FROM_TYPE[mk]) return KIND_FROM_TYPE[mk];
  // Unrecognized types (host, sensor, gotg, smart_speaker, pond, edge, …) still
  // need a room tile — fall back to a generic kind instead of dropping the device.
  return "other";
}

/**
 * A device, carrying only the state something actually reported.
 *
 * These four fields used to default — `on` to false, `locked` to TRUE, the
 * thermostat to 70/68 — and `GET /api/v1/devices` sends identity and
 * capabilities only, never a `metadata` block. So on a real pond the defaults
 * always fired, and the panel's quiet line read "All locked, and everything is
 * off." having read no lock at all. Worse, it could not be falsified: with
 * `locked` hardcoded true, "1 of 2 doors are still unlocked" was unreachable
 * on real data, so unlocking the front door changed nothing on screen.
 *
 * Undefined is the honest value, and every reader downstream treats it as "not
 * reported" rather than as off or locked. Live state comes from the MCP read in
 * `HomeControlsCard`, which is the only thing in this app that knows it.
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
    // Carried through untouched. Home reads this to decide whether a device is
    // offered a switch at all; without it every tile would offer a power toggle
    // to a contact sensor.
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

/**
 * What a chip says when no device under it has reported its state.
 *
 * Said rather than guessed. "0 on" and "All locked" are both claims, and on
 * today's device API — identity and capabilities, no state — they would be
 * claims nothing behind them supports.
 */
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
  // No Security chip. It was pinned first and hardcoded to "Disarmed", with
  // nothing behind it — an alarm state is exactly the kind of thing a household
  // must not read off a placeholder. It comes back when something can answer it.

  const byKind: Record<DeviceKind, DeviceData[]> = { light: [], lock: [], thermo: [], plug: [], other: [] };
  for (const d of devs) byKind[d.kind].push(d);

  if (byKind.lock.length) {
    const known = byKind.lock.filter((d) => typeof d.locked === "boolean");
    const locked = known.filter((d) => d.locked).length;
    // "All locked" needs every lock to have said so — one silent lock and the
    // chip is speaking for a door nobody read.
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
    // "N live" is a claim about a stream. This counts registrations, which is
    // all the device list knows.
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

/**
 * Scenes are the household's first five schedules, and nothing when they have
 * none. The empty case used to return the demo file's five — Good Morning,
 * Good Night, Movie Time, Away, Focus — which put five tappable scenes on a
 * pond that had never been given one.
 */
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

/**
 * The household's recipes, as routine cards. No recipes means no cards.
 *
 * Two fabrications used to live here and both are gone. An empty recipe list
 * returned the five in `data/routines.ts`, so a fresh pond — and any pond whose
 * server was unreachable — showed Good Morning, Good Night, Movie Time, Away
 * and Focus in the drawer and on Routines, each with a Run control; tapping one
 * wrote the fixture into the household's real `agent_recipes` and ran its
 * prompt, in a house that may have had nothing paired.
 *
 * And a recipe whose name happened to match one of the five had its own
 * description thrown away for the fixture's chips and was labelled "7:00 AM
 * weekdays" — a schedule recipes do not have. Everything below is now derived
 * from the recipe in hand; only the icon and the two colours are decoration,
 * picked by a hash so the same recipe looks the same each launch.
 */
function routinesFromRecipes(recipes: AgentRecipe[]): RoutineDetail[] {
  return recipes.map((r) => {
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
      // A recipe with no description says nothing about itself rather than
      // borrowing four actions from a file.
      does:     does.length ? does : ["On demand"],
      // Recipes have no schedule. "7:00 AM weekdays" was never true of one.
      time:     "On demand",
      prompt:   `Run routine: ${r.name}`,
    };
  });
}

/**
 * Turn the weather answer into a slice, and never into a borrowed one.
 *
 * Every `?? MOCK_HOME.weather.x` that used to sit on these lines was the same
 * fabrication as the whole-record fallback, one field at a time: a real answer
 * missing a high and low would silently report yesterday's demo numbers beside
 * a real temperature, which is harder to notice and no more true. An absent
 * field now zeroes, and the forecast strip is simply not drawn rather than
 * showing three invented days.
 */
/**
 * Which of the four weather states this answer puts us in.
 *
 * Copy above this store must branch on the result and not on `weatherEnabled`
 * alone: "off" earns "set your location", "unreachable" earns a sentence about
 * the pond not being able to reach the weather, and they are not the same
 * message to a household that has already set one.
 */
function weatherStatusFor(answered: boolean, w: WeatherApiResponse | null): WeatherStatus {
  if (!answered) return "unreachable";
  return w?.enabled ? "on" : "off";
}

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

/**
 * Why the now-playing poll has backed off, or `null` while it runs normally.
 *
 * Module state rather than store state: nothing renders it, and putting it in
 * `state.data` would make every change an extra re-render of the whole
 * dashboard.
 */
let nowPlayingBackoff: string | null = null;

/** Ticks elapsed since the last attempt while backed off. */
let backoffTicks = 0;

/** How often the widget asks when everything is healthy. */
const NOW_PLAYING_TICK_MS = 10_000;

/**
 * Ticks to skip while backed off — five minutes at the tick above.
 *
 * A flat slow retry rather than a hard stop, because a stop is not recoverable
 * without somebody pressing something: a full reload only happens on app start
 * or server reconnect, `visibilitychange` is unreliable in a desktop webview,
 * and the transport controls are disabled in exactly the state that would need
 * them. Backing off keeps the ~97% saving (8,640 requests a day down to 288)
 * while a Spotify that gets fixed is noticed on its own within five minutes.
 */
const BACKOFF_TICKS = 30;

/**
 * Consecutive 4XX answers before the poll stops entirely.
 *
 * A 4XX from Spotify is a refusal that waits on a person: sign in again, add
 * the account to the app, set a client id. Retrying it on a timer cannot fix
 * it, and a dashboard left open for days spends ~8,600 requests a day finding
 * that out.
 */
const STOP_AFTER_4XX = 5;

/** Consecutive 4XX answers seen so far. */
let fourXxRun = 0;

/** True once the run hit the limit; only an interaction clears it. */
let nowPlayingStopped = false;

/**
 * Is this answer a real 4XX?
 *
 * Deliberately narrower than "did it fail". `upstream_status` is only present
 * when Spotify actually answered, so a transport failure (`np === null`), a
 * refused egress call, or a 5xx outage all return false and leave the counter
 * where it is. Stopping on those would mean a pond restarting mid-poll
 * silences its own music widget until somebody notices and taps it.
 */
function isClientRefusal(np: NowPlayingApiResponse | null): boolean {
  const status = np?.upstream_status;
  return typeof status === "number" && status >= 400 && status < 500;
}

/**
 * Resume polling, and forget the run that stopped it.
 *
 * Two callers, and they are the two the stop rule depends on existing: the
 * widget's own controls (somebody touched it, so they are watching and can see
 * the result) and a Music MCP tool call (the pond just engaged the service, so
 * whatever was refusing may not be any more). Without both of these a stop is
 * unrecoverable — the transport controls are disabled in exactly the state
 * that would need them, and a webview reload only happens on app start.
 */
export function resumeNowPlayingPolling(): void {
  fourXxRun = 0;
  nowPlayingStopped = false;
  nowPlayingBackoff = null;
  backoffTicks = 0;
}

function dueForNowPlayingPoll(): boolean {
  // Stopped is stopped. Unlike the backoff below this never lets a tick
  // through, because the condition cannot clear on its own — see
  // `resumeNowPlayingPolling`.
  if (nowPlayingStopped) return false;
  if (!nowPlayingBackoff) return true;
  backoffTicks += 1;
  if (backoffTicks < BACKOFF_TICKS) return false;
  backoffTicks = 0;
  return true;
}

/**
 * Record what this answer did to the poll, and say so once when it changes.
 *
 * A run of 4XX stops it; anything else resets the run, so five refusals spread
 * across a week of healthy polling never accumulate into a stop.
 */
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
  // Any non-4XX answer — healthy, transport failure, 5xx — breaks the run.
  fourXxRun = 0;
}


function nowPlayingFromApi(np: NowPlayingApiResponse | null): NowPlayingData {
  if (!np || !np.connected) {
    // Not the mock track with `connected` flipped: that put "Weightless /
    // Marconi Union" on the screen of every fresh install, which is the exact
    // state most likely to be mistaken for working playback.
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
  // Spotify answered but refused the request. This is NOT "nothing playing" —
  // the account is linked, so silently showing an idle player hides a problem
  // the user has to act on (and the transport controls would fail too).
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
  // Connected but nothing actively playing (Spotify's 204 case) — show an
  // honest idle state instead of the mock/demo track, so a real connection
  // never gets mistaken for the decorative filler.
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
    // Carried rather than discarded. The fraction above cannot be turned back
    // into mm:ss, so a card that wants to say 3:26 of 8:08 needs the milliseconds
    // the snapshot has always sent. Null, not 0, when Spotify did not send them.
    progressMs: typeof np.progress_ms === "number" ? np.progress_ms : null,
    durationMs: typeof np.duration_ms === "number" ? np.duration_ms : null,
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
    // Answered at all, which is a different question from what the answer was.
    // A 502 from the weather provider, an egress refusal, a 408 from the 30s
    // abort and a dead socket all arrive here as a rejection, and flattening
    // them to `null` made every one of them indistinguishable from an honest
    // `{enabled:false}` — so the card told a household with a location set and
    // weather switched on to go and set their location. It is also why the
    // reading below is kept rather than rebuilt: one failed reload used to wipe
    // a temperature that had been right all day, and the ten-minute poll cannot
    // put it back until upstream recovers.
    const weatherAnswered = weather.status === "fulfilled";
    const wOK = weatherAnswered ? weather.value : null;
    const npOK = nowPlaying.status === "fulfilled" ? nowPlaying.value : null;
    // A full dashboard load is a fresh verdict on whether the poll should run —
    // it is the other route by which a fixed Spotify gets noticed.
    setNowPlayingBackoff(npOK);

    // Partition devices into controllable + cameras
    const ctlDevices: DeviceData[] = [];
    const cams: CameraData[] = [];
    for (const d of dOK) {
      const k = inferKind(d);
      if (k === "camera") cams.push(cameraFromApi(d));
      else ctlDevices.push(deviceFromApi(d, k));
    }

    // No demo house. A pond with nothing paired used to be handed ten invented
    // devices, three cameras and six rooms, which meant the screen a new
    // household meets is the one screen guaranteed to be false. Zero devices is
    // a state, and every surface that shows them has an empty state for it.
    const userName = (sOK?.user_name && sOK.user_name.trim()) || EMPTY_HOME.user;

    // Built field by field rather than spread over MOCK_HOME. The spread kept
    // whatever it was not asked about — which is how a mock slice survives a
    // real load without anybody choosing to keep it.
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
    // Keep whatever the last successful load left behind. There is no fixture
    // to fall back to any more: before the first load that is the empty home,
    // and the screens above it draw their empty states.
  } finally {
    state.loading = false;
  }
}

/** How often the weather slice is re-fetched. The server caches upstream
 *  responses for 15 minutes, so most of these polls are answered locally. */
const WEATHER_POLL_MS = 10 * 60_000;

// Kick off load once on first import in a browser; safe to call again.
if (typeof window !== "undefined") {
  // Fire-and-forget; the UI renders the empty home until load resolves.
  void load();
  // Now-playing changes on its own (user starts/stops playback elsewhere),
  // unlike the rest of the dashboard — poll it so the widget catches up
  // without requiring a manual refresh action.
  setInterval(() => {
    if (dueForNowPlayingPoll()) void refreshNowPlaying();
  }, NOW_PLAYING_TICK_MS);
  // Weather changes on its own too, and a GIAP dashboard is typically left
  // open for days — without this the card keeps showing whatever the sky was
  // doing when the app started.
  setInterval(() => {
    void refreshWeather();
  }, WEATHER_POLL_MS);
  // Timers do not fire while the machine sleeps or the window is hidden, so
  // catch up as soon as the dashboard is looked at again.
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
    // Keep the last known reading rather than blanking the card — but say that
    // it is the last known one. Leaving `weatherStatus` on "on" would let a
    // card go on presenting an hour-old temperature as current.
    state.data = { ...state.data, weatherStatus: "unreachable" };
    emit();
  }
}

/**
 * Re-fetches just the now-playing snapshot, without the full dashboard reload.
 *
 * Always runs when called directly — the halt only gates the timer. That is
 * what makes coming back to the dashboard, refreshing it, or pressing a
 * transport control the way to resume: each of them routes through here and
 * re-evaluates.
 */
/**
 * Fetch the playback snapshot.
 *
 * `userInitiated` must be true ONLY when a person asked — the widget's Try
 * again. It resumes a stopped poll, and the automatic tick calls this same
 * function: resuming unconditionally here reset the 4XX counter on every tick,
 * so the breaker could never trip at all. The tests caught exactly that.
 */
export async function refreshNowPlaying(userInitiated = false): Promise<void> {
  if (userInitiated) resumeNowPlayingPolling();
  try {
    const np = await api.getNowPlaying();
    state.data = { ...state.data, nowPlaying: nowPlayingFromApi(np) };
    setNowPlayingBackoff(np);
    emit();
  } catch {
    // Keep whatever was last known. A throw here is the server being
    // unreachable, not Spotify refusing, so the poll deliberately continues.
  }
}

/** Sends a playback control action, then re-syncs from Spotify's actual state. */
export async function controlNowPlaying(action: MusicControlAction): Promise<void> {
  // Pressing play/next is the clearest "I am here and I want this working"
  // there is — one of the two signals the stop rule depends on.
  resumeNowPlayingPolling();
  try {
    await api.controlMusic(action);
  } catch {
    // ignore — Spotify may report no active device etc; nothing more to do here
  }
  await refreshNowPlaying();
}

// Test hook: reset to the pre-load state — used by vitest tests. Empty, not the
// demo house: a reset that seeded fixtures would let a test pass on data no
// running pond can produce.
export function __resetHubDataForTests(): void {
  state.data = EMPTY_HOME;
  state.routines = [];
  state.loaded = false;
  state.loading = false;
  // Module state, so it outlives a test without this and the next test starts
  // with the poll already halted.
  nowPlayingBackoff = null;
  backoffTicks = 0;
  // The 4XX counter and the stopped flag are module state too; a test that
  // left them set would leak a stopped poll into the next one.
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
