// ─── Home data shapes, and the two fixtures ────────────────────
//
// `HOME` is the demo house, ported verbatim from home-v2-components.jsx. It is
// a DEVELOPMENT fixture and nothing else: it must never be the value a running
// store hands to a screen, because a household cannot tell it from their own
// house. The store seeds `EMPTY_HOME` instead — see hubDataStore's `state`.
//
// Keep `HOME` for the surfaces that are genuinely drawing a demo (hubStore's
// device-state seed) and for tests that need a populated shape, and reach for
// `EMPTY_HOME` everywhere the answer is "the pond has not said yet".

export interface WeatherForecastDay {
  d: string;
  i: "sun" | "cloudSun" | "cloud" | "rain";
  t: number;
}

export interface WeatherData {
  temp: number;
  cond: string;
  icon: string;
  hi: number;
  lo: number;
  hum: number;
  wind: number;
  /** "HH:MM" local time, used to derive the day/night background phase. */
  sunrise: string;
  sunset: string;
  forecast: WeatherForecastDay[];
}

export interface RoomData {
  id: string;
  name: string;
  icon: string;
}

export type DeviceKind = "light" | "lock" | "thermo" | "plug" | "other";

export interface DeviceData {
  id: string;
  name: string;
  kind: DeviceKind;
  /** Raw backend device_type (host, sensor, gotg, smart_speaker, pond, edge, …), used to pick an icon when kind is "other". */
  subtype?: string;
  on?: boolean;
  locked?: boolean;
  value?: number;
  target?: number;
  room: string;
  /**
   * What the backend says this device can do, verbatim from `GET /api/v1/devices`.
   * A contact sensor's list is empty, and that is the only thing that stops Home
   * offering it a power switch it cannot perform.
   */
  capabilities?: string[];
}

export interface CameraData {
  id: string;
  name: string;
  time: string;
  hue: number;
}

export interface CategoryData {
  id: string;
  label: string;
  status: string;
  icon: string;
  color: string;
  bg: string;
}

export interface SceneData {
  id: string;
  name: string;
  icon: string;
  active: boolean;
}

export interface NowPlayingData {
  track: string;
  artist: string;
  elapsed: number;
  hue: number;
  /** Spotify's cover-art URL for the current track, when available. Falls back to the hue gradient when null/absent. */
  albumArt?: string | null;
  /** Whether Spotify is connected — gates real playback controls vs. the cosmetic demo toggle. */
  connected: boolean;
  playing: boolean;
  /**
   * Set when Spotify answered but refused the request. Distinct from "nothing
   * playing": the account is linked, so playback controls would fail too.
   */
  error?: string;
  /** Human-readable explanation for `error`. */
  message?: string;
  /**
   * Position and length in milliseconds, exactly as Spotify sent them.
   *
   * `elapsed` is the fraction derived from the same two numbers and is what the
   * rail binds to; these are what mm:ss labels need, and they are null rather
   * than 0 whenever nobody reported them — a zero here would be read as the
   * start of a track.
   */
  progressMs: number | null;
  durationMs: number | null;
}

/**
 * What the pond last said about weather, which is not the same question as
 * whether weather is switched on.
 *
 * "off" is a household decision; "unreachable" is a 502 from the provider, an
 * egress refusal, a timeout or a dead socket; "unknown" is the state before
 * anything has been asked. Collapsing the last two into "off" is how a panel
 * ends up telling a household to set a location that is already set.
 */
export type WeatherStatus = "on" | "off" | "unreachable" | "unknown";

export interface HomeData {
  user: string;
  weather: WeatherData;
  rooms: RoomData[];
  devices: DeviceData[];
  cameras: CameraData[];
  categories: CategoryData[];
  scenes: SceneData[];
  nowPlaying: NowPlayingData;
  /**
   * The pond answered with a location and weather turned on. False also covers
   * "has not answered yet", which is why nothing keyed on it may render a
   * temperature: the slice is zeroed in that state, not merely stale.
   *
   * It is a narrower question than it looks — read `weatherStatus` before
   * writing copy about it. A card that says "set your location" off this
   * boolean alone says it to a household whose location is set and whose
   * provider answered 502.
   */
  weatherEnabled: boolean;
  /** Why `weatherEnabled` reads the way it does. */
  weatherStatus: WeatherStatus;
  /** The devices in this snapshot came from the pond rather than from this file. */
  devicesAreReal: boolean;
}

/**
 * The slice that is rendered when there is no weather to render.
 *
 * Nothing may paint from this: it is zeroes, and a zero here is a temperature.
 * It exists so `WeatherData` stays a required field instead of becoming a null
 * that every caller has to re-check, and it is shared so the store and the
 * pre-load seed cannot drift into two different ideas of "no weather".
 */
export const NO_WEATHER: WeatherData = {
  temp: 0,
  cond: "",
  icon: "",
  hi: 0,
  lo: 0,
  hum: 0,
  wind: 0,
  sunrise: "",
  sunset: "",
  forecast: [],
};

/** Nothing playing, and nothing claimed about why. */
const SILENT_PLAYER: NowPlayingData = {
  track: "",
  artist: "",
  elapsed: 0,
  hue: 265,
  connected: false,
  playing: false,
  progressMs: null,
  durationMs: null,
};

/**
 * What a household sees before their pond has answered.
 *
 * Empty, not plausible. The store used to seed the demo house below, so the
 * first screen of a fresh install listed six rooms and ten devices nobody
 * owned, and every consumer had to remember to check `devicesAreReal` to avoid
 * repeating them. One consumer forgot, which is the whole reason this exists:
 * an empty fixture is the only one that cannot be mistaken for a house.
 *
 * `user` survives because it claims nothing about this home — it is a name to
 * greet.
 *
 * `gooseSuggestions` used to survive beside it on the same argument, and it is
 * gone: four hardcoded "Goose, ..." strings with ZERO readers anywhere in the
 * app. It read exactly like the field a suggestion engine should populate, and
 * populating it would have rendered nowhere. The real thing is
 * `GET /api/v1/suggestions`, which derives its offers from what this pond can
 * actually do. One of the four was not even honest: "is the front door locked?"
 * implies a lock the pond can read, and device state is the one thing the
 * registry does not know.
 */
export const EMPTY_HOME: HomeData = {
  user: "Jerry",
  weather: NO_WEATHER,
  rooms: [],
  devices: [],
  cameras: [],
  categories: [],
  scenes: [],
  nowPlaying: SILENT_PLAYER,
  weatherEnabled: false,
  weatherStatus: "unknown",
  devicesAreReal: false,
};

export const HOME: HomeData = {
  user: "Jerry",
  weather: {
    temp: 64,
    cond: "Partly cloudy",
    icon: "cloudSun",
    hi: 68,
    lo: 54,
    hum: 62,
    wind: 12,
    sunrise: "06:30",
    sunset: "20:15",
    forecast: [
      { d: "Tue", i: "sun", t: 66 },
      { d: "Wed", i: "cloud", t: 62 },
      { d: "Thu", i: "rain", t: 58 },
    ],
  },
  rooms: [
    { id: "home",    name: "Home",        icon: "home" },
    { id: "living",  name: "Living Room", icon: "sofa" },
    { id: "kitchen", name: "Kitchen",     icon: "utensils" },
    { id: "bedroom", name: "Bedroom",     icon: "bed" },
    { id: "office",  name: "Office",      icon: "briefcase" },
    { id: "outdoor", name: "Outdoor",     icon: "tree" },
  ],
  devices: [
    { id: "driveway",  name: "Driveway Light",  kind: "light", on: false, room: "Outdoor" },
    { id: "thermo",    name: "Thermostat",       kind: "thermo", value: 68, target: 70, room: "Living Room" },
    { id: "frontdoor", name: "Front Door",       kind: "lock",  locked: true, room: "Outdoor" },
    { id: "patio",     name: "Patio Lights",     kind: "light", on: false, room: "Outdoor" },
    { id: "lrlights",  name: "Living Room",      kind: "light", on: true, room: "Living Room" },
    { id: "plug",      name: "Coffee Plug",      kind: "plug",  on: true, room: "Kitchen" },
    { id: "kitchenlt", name: "Kitchen Lights",   kind: "light", on: true, room: "Kitchen" },
    { id: "bedlamp",   name: "Bedside Lamp",     kind: "light", on: false, room: "Bedroom" },
    { id: "bedfan",    name: "Bedroom Fan",      kind: "plug",  on: false, room: "Bedroom" },
    { id: "desklamp",  name: "Desk Lamp",        kind: "light", on: true, room: "Office" },
  ],
  cameras: [
    { id: "front", name: "Front Door", time: "8:48 AM", hue: 150 },
    { id: "drive", name: "Driveway",   time: "8:49 AM", hue: 35 },
    { id: "back",  name: "Backyard",   time: "8:47 AM", hue: 205 },
  ],
  categories: [
    { id: "security", label: "Security", status: "Disarmed",    icon: "shieldCheck", color: "#16A34A", bg: "#DCFCE7" },
    { id: "locks",    label: "Locks",    status: "All locked",  icon: "lock",        color: "#2563EB", bg: "#DBEAFE" },
    { id: "climate",  label: "Climate",  status: "Heat to 70°", icon: "thermo",      color: "#EA580C", bg: "#FFEDD5" },
    { id: "lights",   label: "Lights",   status: "2 on",        icon: "bulb",        color: "#D97706", bg: "#FEF3C7" },
    { id: "cameras",  label: "Cameras",  status: "3 live",      icon: "cctv",        color: "#7C3AED", bg: "#EDE9FE" },
    { id: "plugs",    label: "Plugs",    status: "9 on",        icon: "plug",        color: "#0D9488", bg: "#CCFBF1" },
  ],
  scenes: [
    { id: "morning", name: "Good Morning", icon: "sun",   active: true },
    { id: "night",   name: "Good Night",   icon: "moon",  active: false },
    { id: "movie",   name: "Movie Time",   icon: "film",  active: false },
    { id: "away",    name: "Away",         icon: "away",  active: false },
    { id: "focus",   name: "Focus",        icon: "focus", active: false },
  ],
  nowPlaying: {
    track: "Weightless",
    artist: "Marconi Union",
    elapsed: 0.42,
    hue: 265,
    connected: false,
    playing: true,
    progressMs: null,
    durationMs: null,
  },
  weatherEnabled: false,
  weatherStatus: "unknown",
  devicesAreReal: false,
};
