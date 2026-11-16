// Turns an activity event into a sentence a householder can read.
//
// Pure: the Activity screen renders these, and the tests pin the wording for every action the
// server emits. The raw action and attributes survive in `technical`, shown on hover, so the
// plain wording never costs an engineer the original record.

import { friendlyToolName } from "../../components/toolNames";
import type { ActivityEvent, AttributeValue } from "../../api/types";

/** What one event row says. */
export interface EventDescription {
  /** The sentence the row leads with, e.g. "Goose reached Spotify for music". */
  title: string;
  /** A quieter second line, when the event carries something worth adding. */
  detail?: string;
  /** The raw action and attributes, for the hover text a developer reads. */
  technical: string;
}

/** Hosts a householder knows by a name rather than an address; matched exactly or as a subdomain. */
const HOST_NAMES: ReadonlyArray<[string, string]> = [
  ["spotify.com", "Spotify"],
  ["music.apple.com", "Apple Music"],
  ["api.search.brave.com", "the web search service"],
  ["open-meteo.com", "the weather service"],
  ["uber.com", "Uber"],
  ["github.com", "GitHub"],
  ["coingecko.com", "CoinGecko"],
  ["huggingface.co", "Hugging Face"],
  ["wikipedia.org", "Wikipedia"],
  ["fcm.googleapis.com", "the phone notification service"],
  ["ngrok.com", "the remote access tunnel"],
  ["ngrok-free.app", "the remote access tunnel"],
];

/**
 * What an outbound call was for, keyed by the `tool` attribute: either the pond's own purpose
 * (`EgressPurpose::as_str` in crates/pond-core/src/shared/services/egress.rs) or a chat tool.
 */
const PURPOSES: Record<string, string> = {
  "giap-assistant": "to think through a reply",
  "giap-sign-in": "to keep a sign-in current",
  "giap-music": "for music",
  "giap-credentials": "for a managed key",
  "giap-rides": "for a ride",
  web_search_key_check: "to check the web search key",
  "giap-models": "for a model download",
  "giap-voice": "to recognise speech",
  "giap-diagnostics": "for a health check",
  "giap-schedules": "for a schedule",
  "giap-extensions": "to check an extension",
  "giap-push": "to send a phone notification",
  "giap-web-forms": "to send a form you confirmed",
  "giap-news": "for your news",
  "giap-remote-tunnel": "for remote access",
  "giap-weather": "for the weather",
  "giap-knowledge": "to look something up",
  search_web: "for a web search",
  read_web_page: "to read a web page",
  show_pictures: "for pictures",
  get_current_weather: "for the weather",
  get_weather_forecast: "for the forecast",
  get_wikipedia_article: "to look something up on Wikipedia",
  compute_answer: "to work something out",
  play: "for music",
  status: "for music",
  control: "for music",
};

/** What Goose did when it called a tool, keyed by the bare tool name. */
const TOOL_DEEDS: Record<string, string> = {
  search_web: "searched the web",
  read_web_page: "read a web page",
  show_pictures: "found some pictures",
  get_current_weather: "checked the weather",
  get_weather_forecast: "checked the forecast",
  get_wikipedia_article: "looked something up on Wikipedia",
  compute_answer: "worked something out",
  list_schedules: "looked at your schedules",
  create_schedule: "created a schedule",
  delete_schedule: "deleted a schedule",
  pause_schedule: "paused a schedule",
  resume_schedule: "resumed a schedule",
  run_schedule_now: "ran a schedule",
  save_memory: "saved a memory",
  recall_memories: "recalled some memories",
  forget_memory: "forgot a memory",
  get_system_info: "checked how the Pond is doing",
  get_current_time: "checked the time",
  send_notification: "sent a notification",
  list_registered_devices: "looked at your devices",
  get_current_profile: "checked who it is talking to",
  play: "played music",
  status: "checked what is playing",
  control: "controlled the music",
};

/** Who a `security.audit` principal is (`principal_label` in sqlite_security_policy.rs). */
function principalWords(principal: string | undefined): string {
  if (!principal) return "Someone";
  if (principal === "loopback") return "This computer";
  if (principal === "internal") return "Goose itself";
  if (principal === "agamemnon") return "The household admin";
  if (principal.startsWith("token:")) return "A paired device";
  return "Someone";
}

function text(attrs: Record<string, AttributeValue>, key: string): string | undefined {
  const v = attrs[key];
  if (!v) return undefined;
  if ("text" in v) return v.text;
  if ("int" in v) return String(v.int);
  if ("float" in v) return String(v.float);
  if ("bool" in v) return String(v.bool);
  return undefined;
}

function num(attrs: Record<string, AttributeValue>, key: string): number | undefined {
  const v = attrs[key];
  if (!v) return undefined;
  if ("int" in v) return v.int;
  if ("float" in v) return v.float;
  return undefined;
}

function flag(attrs: Record<string, AttributeValue>, key: string): boolean | undefined {
  const v = attrs[key];
  return v && "bool" in v ? v.bool : undefined;
}

/** "living_room-lamp" -> "living room lamp": ids read as words, never as code. */
function words(raw: string): string {
  return raw.replace(/[._:-]+/g, " ").trim();
}

function capitalise(s: string): string {
  return s.charAt(0).toUpperCase() + s.slice(1);
}

function bareTool(raw: string): string {
  return raw.includes("__") ? raw.split("__").pop() ?? raw : raw;
}

/** A host as its owner's name when known, else the host itself. */
export function placeName(host: string): string {
  const h = host.toLowerCase();
  for (const [suffix, name] of HOST_NAMES) {
    if (h === suffix || h.endsWith(`.${suffix}`)) return name;
  }
  return host;
}

/** " for music": why a call left the house, from its `tool` attribute. */
function purpose(tool: string | undefined): string {
  if (!tool) return "";
  const phrase = PURPOSES[tool] ?? PURPOSES[bareTool(tool)];
  return ` ${phrase ?? `for ${friendlyToolName(tool).toLowerCase()}`}`;
}

function toolDeed(tool: string | undefined): string {
  if (!tool) return "used a tool";
  return TOOL_DEEDS[bareTool(tool)] ?? `used ${friendlyToolName(tool).toLowerCase()}`;
}

/** 1536 -> "1.5 KB"; for the space an admin clean-up freed. */
export function formatBytes(n: number): string {
  if (n < 1024) return `${n} bytes`;
  const units = ["KB", "MB", "GB", "TB"];
  let value = n / 1024;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value >= 10 ? Math.round(value) : value.toFixed(1)} ${units[unit]}`;
}

function freed(attrs: Record<string, AttributeValue>): string {
  const n = num(attrs, "freed_bytes");
  return n !== undefined && n > 0 ? `, freeing ${formatBytes(n)}` : "";
}

function count(n: number, one: string, many: string): string {
  return `${n} ${n === 1 ? one : many}`;
}

/** The fallback for an action this module does not know: "foo.bar_baz" -> "Foo: bar baz". */
function unknownAction(action: string): string {
  const [head, ...rest] = action.split(".");
  const subject = capitalise(words(head ?? action));
  return rest.length ? `${subject}: ${words(rest.join(" "))}` : subject;
}

function technical(event: ActivityEvent): string {
  const attrs = Object.keys(event.attributes)
    .sort()
    .map((k) => `${k}=${text(event.attributes, k) ?? ""}`);
  return [event.action, ...attrs].join(" ");
}

function describeTitle(event: ActivityEvent): Omit<EventDescription, "technical"> {
  const a = event.attributes;
  switch (event.action) {
    case "egress.http": {
      const host = text(a, "host") ?? "the internet";
      const status = num(a, "status");
      const failed = status !== undefined && status >= 400;
      const place = placeName(host);
      return {
        title: failed
          ? `Goose tried to reach ${place}${purpose(text(a, "tool"))}, but the request failed`
          : `Goose reached ${place}${purpose(text(a, "tool"))}`,
        detail: place === host ? undefined : host,
      };
    }
    case "egress.denied": {
      const host = text(a, "host") ?? "the internet";
      const reason = text(a, "reason");
      return {
        title: `Goose was kept from reaching ${placeName(host)}${purpose(text(a, "tool"))}`,
        detail: reason ? `${host}: ${reason}` : host,
      };
    }
    case "agent.turn": {
      const tools = num(a, "tool_count") ?? 0;
      return {
        title: tools > 0
          ? `Goose answered a message, using ${count(tools, "tool", "tools")}`
          : "Goose answered a message",
      };
    }
    case "inference.completion": {
      const model = text(a, "model");
      return { title: "Goose thought through a reply", detail: model };
    }
    case "tool.call":
      return { title: `Goose ${toolDeed(text(a, "tool"))}` };
    case "sensor.reading": {
      const device = words(text(a, "device_id") ?? "a sensor");
      const kind = text(a, "sensor_type");
      const value = text(a, "value");
      const unit = text(a, "unit");
      const reading = value !== undefined ? `${value}${unit ? ` ${unit}` : ""}` : undefined;
      return {
        title: kind
          ? `${capitalise(words(kind))} reading from ${device}${reading ? `: ${reading}` : ""}`
          : `New reading from ${device}${reading ? `: ${reading}` : ""}`,
      };
    }
    case "camera.event": {
      const camera = words(text(a, "camera_id") ?? "a");
      const kind = text(a, "event_type");
      return { title: `The ${camera} camera noticed ${kind ? words(kind) : "something"}` };
    }
    case "device.state_changed": {
      const device = words(text(a, "device_id") ?? "A device");
      const key = text(a, "key");
      return { title: key ? `${capitalise(device)} changed its ${words(key)}` : `${capitalise(device)} changed` };
    }
    case "time.tick":
      return { title: "The Pond's clock reached a new hour" };
    case "presence.profile": {
      const transition = text(a, "transition");
      const source = text(a, "source");
      return {
        title: transition === "departed" ? "Someone left home" : "Someone arrived home",
        detail: source ? `Noticed by ${words(source)}` : undefined,
      };
    }
    case "session.lifecycle": {
      const phase = text(a, "phase");
      if (phase === "idle") return { title: "A conversation went quiet" };
      if (phase === "resumed") return { title: "A conversation picked up again" };
      return { title: "A conversation started" };
    }
    case "security.audit": {
      const who = principalWords(text(a, "principal"));
      const what = [text(a, "action"), text(a, "scope")]
        .filter((s): s is string => Boolean(s))
        .map(words)
        .join(" ");
      const verdict = text(a, "verdict") ?? (flag(a, "ok") === false ? "deny" : "allow");
      const reason = text(a, "reason");
      if (verdict === "deny") {
        return { title: `${who} was not allowed to ${what || "do that"}`, detail: reason };
      }
      if (verdict === "would_deny") {
        return {
          title: `${who} was allowed to ${what || "do that"}, though stricter rules would refuse it`,
          detail: reason,
        };
      }
      return { title: `${who} was allowed to ${what || "do that"}` };
    }
    case "auth.device_paired": {
      const name = text(a, "device_name");
      return { title: name ? `${name} was paired with the Pond` : "A new device was paired with the Pond" };
    }
    case "auth.device_resumed": {
      const name = text(a, "device_name");
      return { title: name ? `${name} reconnected to the Pond` : "A paired device reconnected to the Pond" };
    }
    case "auth.pairing_verify_failed": {
      const name = text(a, "device_name");
      const reason = text(a, "rejection_reason");
      return {
        title: name ? `${name} could not be paired` : "A device could not be paired",
        detail: reason ? capitalise(words(reason)) : undefined,
      };
    }
    case "auth.device_resume_failed": {
      const name = text(a, "device_name");
      const reason = text(a, "rejection_reason");
      return {
        title: name ? `${name} could not reconnect` : "A paired device could not reconnect",
        detail: reason ? capitalise(words(reason)) : undefined,
      };
    }
    case "auth.spoken_code_issued":
      return { title: "Goose read out a pairing code" };
    case "developer.debug_switched":
      return {
        title: flag(a, "enabled") ? "Developer debugging was turned on" : "Developer debugging was turned off",
      };
    case "admin.unlocked":
      return {
        title: flag(a, "by_key") ? "My Pond was unlocked with a recovery key" : "My Pond was unlocked",
      };
    case "admin.unlock_refused":
      return { title: "An attempt to unlock My Pond was refused" };
    case "admin.key_issued":
      return { title: "A My Pond recovery key was issued" };
    case "admin.key_revoked":
      return { title: "A My Pond recovery key was revoked" };
    case "admin.file_deleted": {
      const area = text(a, "area");
      return { title: `A file was deleted${area ? ` from ${words(area)}` : ""}${freed(a)}` };
    }
    case "admin.cache_cleared": {
      const cache = text(a, "cache");
      return { title: `${cache ? `The ${words(cache)} cache` : "A cache"} was cleared${freed(a)}` };
    }
    case "admin.task_deleted":
      return { title: "A task was deleted" };
    case "admin.artifact_deleted":
      return { title: "A file from a task was deleted" };
    case "admin.power_mode_set": {
      const mode = text(a, "mode");
      return {
        title: mode ? `Power mode was set to ${mode}` : "Power mode was changed",
        detail: flag(a, "reboot_required") ? "Takes effect after a restart" : undefined,
      };
    }
    case "admin.restart_requested":
      return { title: "A restart of the Pond was requested" };
    case "settings.changed": {
      const label = text(a, "label") ?? capitalise(words(text(a, "key") ?? "a setting"));
      const from = text(a, "from");
      const to = text(a, "to");
      if (flag(a, "redacted") || to === undefined) return { title: `${label} was changed` };
      return {
        title: from !== undefined
          ? `Setting changed: ${label} from ${from} to ${to}`
          : `Setting changed: ${label} set to ${to}`,
      };
    }
    case "extension.changed": {
      const name = capitalise(words(text(a, "key") ?? "a tool set"));
      const to = text(a, "to");
      const did = to === "added" ? "were added" : to === "removed" ? "were removed"
        : to === "enabled" ? "were switched on" : to === "disabled" ? "were switched off" : "were changed";
      return { title: `${name} tools ${did}` };
    }
    case "schedule.changed": {
      const to = text(a, "to");
      return { title: to === "paused" ? "A routine was paused" : to === "active" ? "A routine was resumed" : "A routine was changed" };
    }
    case "system.start":
    case "system.boot":
      return { title: "The Pond started" };
    default:
      return { title: unknownAction(event.action) };
  }
}

/** Describe one event in plain words, keeping the raw record in `technical`. */
export function describeEvent(event: ActivityEvent): EventDescription {
  return { ...describeTitle(event), technical: technical(event) };
}
