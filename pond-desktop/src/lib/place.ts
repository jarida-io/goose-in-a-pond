// ─── Where and when this pond is ─────────────────────────────────────────────
// Zones come from the server's IANA database; detection is one POST shared by every screen.

import { api } from "../api/PondApiClient";
import type { DetectedPlace, PlaceSource, ZoneChoice } from "../api/types";

// Re-exported so a caller needs one import for the whole subject.
export type { DetectedPlace, PlaceSource, ZoneChoice };

/** Only for when the server is unreachable and `Intl.supportedValuesOf` is missing; not a curated list. */
const LAST_RESORT = ["UTC", "Africa/Nairobi", "Europe/London", "America/New_York"];

let cache: ZoneChoice[] | null = null;

/** Offset for a zone today, computed locally. Used to fill in local fallbacks. */
function localOffset(zone: string): string {
  try {
    const parts = new Intl.DateTimeFormat("en", {
      timeZone: zone,
      timeZoneName: "longOffset",
    }).formatToParts(new Date());
    const name = parts.find((p) => p.type === "timeZoneName")?.value ?? "";
    // "GMT+03:00" → "+03:00"; plain "GMT" means UTC.
    const m = name.match(/([+-]\d{2}:\d{2})$/);
    return m ? m[1] : "+00:00";
  } catch {
    return "+00:00";
  }
}

/** The place a zone name implies: `Africa/Nairobi` → `Nairobi`. */
export function placeFromZone(zone: string): string {
  if (!zone.includes("/")) return "";
  return zone.split("/").pop()?.replace(/_/g, " ") ?? "";
}

/** This device's own zone. Local, instant, no network. */
export function deviceZone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
  } catch {
    return "UTC";
  }
}

/** Build choices from bare zone names, resolving each offset on this device. */
function decorate(zones: string[]): ZoneChoice[] {
  return zones.map((zone) => ({
    zone,
    offset: localOffset(zone),
    place: placeFromZone(zone),
  }));
}

/** Server zones (the list saving validates against), then this webview's `Intl` list, then LAST_RESORT; cached. */
export async function allZones(): Promise<ZoneChoice[]> {
  if (cache) return cache;
  try {
    const res = await api.listTimeZones();
    if (res?.zones?.length) {
      cache = res.zones;
      return cache;
    }
  } catch {
    // Fall through — an offline pond still deserves a working picker.
  }
  try {
    const supported = (Intl as unknown as {
      supportedValuesOf?: (k: string) => string[];
    }).supportedValuesOf?.("timeZone");
    if (supported?.length) {
      cache = decorate(supported);
      return cache;
    }
  } catch {
    /* older webview */
  }
  // Always include the device's own zone, so it stays selectable.
  const zone = deviceZone();
  const zones = LAST_RESORT.includes(zone) ? LAST_RESORT : [zone, ...LAST_RESORT];
  cache = decorate(zones);
  return cache;
}

/** Best-effort geolocation that never gates detection: in a Tauri webview it often never resolves. */
function deviceCoords(timeoutMs = 6000): Promise<{ latitude: number; longitude: number } | null> {
  return new Promise((resolve) => {
    if (typeof navigator === "undefined" || !navigator.geolocation) {
      resolve(null);
      return;
    }
    let settled = false;
    const done = (v: { latitude: number; longitude: number } | null) => {
      if (!settled) {
        settled = true;
        resolve(v);
      }
    };
    // Own timer as well as the option: a webview may never call either callback.
    const timer = setTimeout(() => done(null), timeoutMs);
    navigator.geolocation.getCurrentPosition(
      (p) => {
        clearTimeout(timer);
        done({
          // Four decimals (~11 m): enough for weather, too coarse to read as tracking.
          latitude: Number(p.coords.latitude.toFixed(4)),
          longitude: Number(p.coords.longitude.toFixed(4)),
        });
      },
      () => {
        clearTimeout(timer);
        done(null);
      },
      { timeout: timeoutMs, maximumAge: 600_000, enableHighAccuracy: false },
    );
  });
}

/** The one place detection for every screen; `typedName` beats anything derived. */
export async function detectPlace(typedName?: string): Promise<DetectedPlace> {
  const coords = await deviceCoords();
  return api.detectLocation({
    system_zone: deviceZone(),
    typed_name: typedName?.trim() || undefined,
    latitude: coords?.latitude,
    longitude: coords?.longitude,
  });
}

/** Reset the cached catalogue. Tests only. */
export function __resetZoneCache() {
  cache = null;
}
