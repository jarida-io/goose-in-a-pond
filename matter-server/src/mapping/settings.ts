/**
 * Appliance settings and operations. ModeBase clusters are found by shape (`supportedModes` +
 * `currentMode`), not name; Temperature Control and Laundry Washer Controls differ, so are explicit.
 */

import type { EndpointSnapshot, NodeSnapshot } from "./snapshot.js";
import { applicationEndpoints } from "./snapshot.js";

/** How a chosen value reaches the device. */
export type SettingWrite =
  | { kind: "command"; command: string; field: string }
  | { kind: "attribute"; attribute: string };

/** One thing a user can choose on a device; `values` are the labels the device itself published. */
export interface Setting {
  /** What to call it in a sentence: "laundry washer mode", "spin speed". */
  name: string;
  endpoint: number;
  cluster: string;
  values: string[];
  write: SettingWrite;
  /** Where the live choice is, if not `currentMode` or the written attribute (e.g. `currentInput`). */
  current?: string;
  /** The number to send for a label, or undefined if the device never offered it. */
  valueFor: (choice: string) => number | undefined;
}

/** Operational State's commands, which every appliance that runs a cycle shares. */
export interface Operations {
  endpoint: number;
  cluster: string;
  /** Commands the cluster defines. Matter names them exactly these. */
  values: string[];
}

const OPERATIONAL_STATE = "operationalState";
const MEDIA_PLAYBACK = "mediaPlayback";
const MEDIA_INPUT = "mediaInput";
const AUDIO_OUTPUT = "audioOutput";

/** MediaPlayback's PlaybackStateEnum, in the words a person would use. */
const PLAYBACK_STATES: Record<number, string> = {
  0: "playing",
  1: "paused",
  2: "not playing",
  3: "buffering",
};

/** A media device's input or output list as a setting: device-published labels, chosen by index. */
function mediaListSetting(
  endpoint: EndpointSnapshot,
  cluster: string,
  attribute: string,
  command: string,
  name: string,
): Setting | undefined {
  const list = endpoint.clusters[cluster]?.[attribute];
  if (!Array.isArray(list) || list.length === 0) return undefined;

  const entries: { label: string; index: number }[] = [];
  for (const entry of list) {
    if (typeof entry !== "object" || entry === null) return undefined;
    const label = (entry as { name?: unknown }).name;
    const index = (entry as { index?: unknown }).index;
    if (typeof label !== "string" || typeof index !== "number") return undefined;
    entries.push({ label, index });
  }

  return {
    name,
    endpoint: endpoint.number,
    cluster,
    values: entries.map(e => e.label),
    write: { kind: "command", command, field: "index" },
    current: attribute === "inputList" ? "currentInput" : "currentOutput",
    valueFor: choice => entries.find(e => looseEquals(e.label, choice))?.index,
  };
}

/** OperationalStateEnum's own values, for a device that labels a state with nothing. */
const STANDARD_STATES: Record<number, string> = {
  0: "stopped",
  1: "running",
  2: "paused",
  3: "error",
};
const TEMPERATURE_CONTROL = "temperatureControl";
const THERMOSTAT = "thermostat";
const LAUNDRY_WASHER_CONTROLS = "laundryWasherControls";

/**
 * Clusters read BY NAME, for `controller.ts`'s allowlist, which drops unnamed ones (unit fixtures
 * bypass it, so a miss passes tests). ModeBase clusters get in via its `*Mode` suffix rule.
 */
export function settingClusters(): ReadonlySet<string> {
  return new Set([
    OPERATIONAL_STATE,
    TEMPERATURE_CONTROL,
    THERMOSTAT,
    LAUNDRY_WASHER_CONTROLS,
    MEDIA_PLAYBACK,
    MEDIA_INPUT,
    AUDIO_OUTPUT,
  ]);
}

/** `laundryWasherMode` → `laundry washer mode`. The device's own word, made speakable. */
function spokenName(clusterId: string): string {
  return clusterId
    .replace(/([a-z0-9])([A-Z])/g, "$1 $2")
    .replace(/([A-Z]+)([A-Z][a-z])/g, "$1 $2")
    .toLowerCase();
}

function labelsOf(value: unknown): string[] | undefined {
  if (!Array.isArray(value)) return undefined;
  const labels = value.filter((v): v is string => typeof v === "string");
  return labels.length === value.length ? labels : undefined;
}

/** Match a label the way a user says it: case and spacing are not the point. */
function looseEquals(a: string, b: string): boolean {
  return a.trim().toLowerCase() === b.trim().toLowerCase();
}

function indexSetting(
  name: string,
  endpoint: number,
  cluster: string,
  labels: string[],
  write: SettingWrite,
): Setting {
  return {
    name,
    endpoint,
    cluster,
    values: labels,
    write,
    valueFor: choice => {
      const index = labels.findIndex(label => looseEquals(label, choice));
      return index === -1 ? undefined : index;
    },
  };
}

/** A ModeBase cluster, detected by shape (`supportedModes` of `{label, mode}` + `currentMode`), not name. */
function modeSetting(endpoint: EndpointSnapshot, cluster: string): Setting | undefined {
  const state = endpoint.clusters[cluster];
  const supported = state?.["supportedModes"];
  if (!Array.isArray(supported) || supported.length === 0) return undefined;

  const entries: { label: string; mode: number }[] = [];
  for (const entry of supported) {
    if (typeof entry !== "object" || entry === null) return undefined;
    const label = (entry as { label?: unknown }).label;
    const mode = (entry as { mode?: unknown }).mode;
    if (typeof label !== "string" || typeof mode !== "number") return undefined;
    entries.push({ label, mode });
  }

  return {
    name: spokenName(cluster),
    endpoint: endpoint.number,
    cluster,
    values: entries.map(e => e.label),
    // By command, not a currentMode write: the command is how the device refuses a transition.
    write: { kind: "command", command: "changeToMode", field: "newMode" },
    valueFor: choice => entries.find(e => looseEquals(e.label, choice))?.mode,
  };
}

/**
 * Thermostat SystemMode: a spec-fixed enum8, not ModeBase. Emergency heat, precooling and
 * fan-only are omitted: optional and rarely implemented, so likely rejected.
 */
const SYSTEM_MODES: readonly { label: string; code: number }[] = [
  { label: "off", code: 0 },
  { label: "auto", code: 1 },
  { label: "cool", code: 3 },
  { label: "heat", code: 4 },
];

function systemModeSetting(endpoint: EndpointSnapshot): Setting | undefined {
  const state = endpoint.clusters[THERMOSTAT];
  // Present but unpopulated is still a thermostat; absent is not one.
  if (state === undefined || !("systemMode" in state)) return undefined;

  return {
    name: "system mode",
    endpoint: endpoint.number,
    cluster: THERMOSTAT,
    values: SYSTEM_MODES.map(m => m.label),
    write: { kind: "attribute", attribute: "systemMode" },
    valueFor: choice => SYSTEM_MODES.find(m => looseEquals(m.label, choice))?.code,
  };
}

/** Temperature Control's level variant: levels named in a parallel array. */
function temperatureLevelSetting(endpoint: EndpointSnapshot): Setting | undefined {
  const levels = labelsOf(endpoint.clusters[TEMPERATURE_CONTROL]?.["supportedTemperatureLevels"]);
  if (levels === undefined || levels.length === 0) return undefined;

  return indexSetting("temperature level", endpoint.number, TEMPERATURE_CONTROL, levels, {
    kind: "command",
    command: "setTemperature",
    field: "targetTemperatureLevel",
  });
}

/** Laundry Washer Controls: spin speed and rinse count, side by side. */
function washerControlSettings(endpoint: EndpointSnapshot): Setting[] {
  const state = endpoint.clusters[LAUNDRY_WASHER_CONTROLS];
  if (state === undefined) return [];
  const settings: Setting[] = [];

  const speeds = labelsOf(state["spinSpeeds"]);
  if (speeds !== undefined && speeds.length > 0) {
    settings.push(
      indexSetting("spin speed", endpoint.number, LAUNDRY_WASHER_CONTROLS, speeds, {
        kind: "attribute",
        attribute: "spinSpeedCurrent",
      }),
    );
  }

  // `supportedRinses` is an enum list; matter.js may hand over names or numbers.
  const rinses = state["supportedRinses"];
  if (Array.isArray(rinses) && rinses.length > 0) {
    const labels = rinses.map(String);
    settings.push({
      name: "rinses",
      endpoint: endpoint.number,
      cluster: LAUNDRY_WASHER_CONTROLS,
      values: labels,
      write: { kind: "attribute", attribute: "numberOfRinses" },
      valueFor: choice => {
        const index = labels.findIndex(label => looseEquals(label, choice));
        if (index === -1) return undefined;
        // Numeric entries are the value itself; named ones are their position.
        const raw = rinses[index];
        return typeof raw === "number" ? raw : index;
      },
    });
  }

  return settings;
}

/** Everything selectable on this device, in endpoint order. */
export function settingsOf(node: NodeSnapshot): Setting[] {
  const settings: Setting[] = [];

  for (const endpoint of applicationEndpoints(node)) {
    for (const cluster of Object.keys(endpoint.clusters)) {
      const mode = modeSetting(endpoint, cluster);
      if (mode !== undefined) settings.push(mode);
    }
    const input = mediaListSetting(endpoint, MEDIA_INPUT, "inputList", "selectInput", "input");
    if (input !== undefined) settings.push(input);
    const output = mediaListSetting(
      endpoint,
      AUDIO_OUTPUT,
      "outputList",
      "selectOutput",
      "audio output",
    );
    if (output !== undefined) settings.push(output);
    const temperature = temperatureLevelSetting(endpoint);
    if (temperature !== undefined) settings.push(temperature);
    const systemMode = systemModeSetting(endpoint);
    if (systemMode !== undefined) settings.push(systemMode);
    settings.push(...washerControlSettings(endpoint));
  }

  return settings;
}

/** The one setting a spoken name matches (exact, then partial, then shared words); ambiguous = none. */
export function settingNamed(node: NodeSnapshot, name: string): Setting | undefined {
  const settings = settingsOf(node);
  const exact = settings.find(s => looseEquals(s.name, name));
  if (exact !== undefined) return exact;

  // e.g. "washer mode" for "laundry washer mode".
  const wanted = name.trim().toLowerCase();
  const partial = settings.filter(s => s.name.includes(wanted) || wanted.includes(s.name));
  if (partial.length === 1) return partial[0];

  // e.g. "temperature control" for "temperature level": named by its cluster.
  const words = new Set(wanted.split(/\s+/).filter(w => w !== ""));
  const shared = settings.filter(s => s.name.split(" ").some(w => words.has(w)));
  return shared.length === 1 ? shared[0] : undefined;
}

/** The device's `operationalStateList`, id → lowercased label; it may add states of its own. */
function operationalStates(endpoint: EndpointSnapshot): Map<number, string> {
  const list = endpoint.clusters[OPERATIONAL_STATE]?.["operationalStateList"];
  const states = new Map<number, string>();
  if (!Array.isArray(list)) return states;

  for (const entry of list) {
    if (typeof entry !== "object" || entry === null) continue;
    const id = (entry as { operationalStateId?: unknown }).operationalStateId;
    const label = (entry as { operationalStateLabel?: unknown }).operationalStateLabel;
    if (typeof id !== "number") continue;
    states.set(id, typeof label === "string" && label !== "" ? label.toLowerCase() : STANDARD_STATES[id] ?? `state ${id}`);
  }
  return states;
}

/**
 * The operations this device accepts, derived from `operationalStateList`: the spec requires the
 * states matching its supported commands (Running → Start, Paused → Pause/Resume).
 */
export function operationsOf(node: NodeSnapshot): Operations | undefined {
  const endpoint = applicationEndpoints(node).find(e => OPERATIONAL_STATE in e.clusters);
  if (endpoint === undefined) {
    // No Operational State: a video player's MediaPlayback play/pause/stop serves `operation`.
    const media = applicationEndpoints(node).find(e => MEDIA_PLAYBACK in e.clusters);
    if (media === undefined) return undefined;
    return { endpoint: media.number, cluster: MEDIA_PLAYBACK, values: ["play", "pause", "stop"] };
  }

  const states = new Set(operationalStates(endpoint).values());
  const values: string[] = [];
  if (states.has("running")) values.push("start");
  if (states.has("stopped")) values.push("stop");
  if (states.has("paused")) values.push("pause", "resume");

  // No state list says nothing, not "no": offer the standard four and let the device refuse.
  return {
    endpoint: endpoint.number,
    cluster: OPERATIONAL_STATE,
    values: values.length > 0 ? values : ["start", "stop", "pause", "resume"],
  };
}

/** The state the device reports, in its own words: what an operation reports back, not the request. */
export function observedOperation(node: NodeSnapshot): string | undefined {
  const endpoint = applicationEndpoints(node).find(e => OPERATIONAL_STATE in e.clusters);
  if (endpoint === undefined) {
    const media = applicationEndpoints(node).find(e => MEDIA_PLAYBACK in e.clusters);
    if (media === undefined) return undefined;
    const state = media.clusters[MEDIA_PLAYBACK]?.["currentState"];
    if (typeof state === "number") return PLAYBACK_STATES[state];
    if (typeof state === "string") {
      // matter.js may decode the enum to its name, as everywhere else.
      const key = state.toLowerCase().replace(/[\s_-]/g, "");
      return Object.values(PLAYBACK_STATES).find(w => w.replace(/ /g, "") === key);
    }
    return undefined;
  }

  const current = endpoint.clusters[OPERATIONAL_STATE]?.["operationalState"];
  if (typeof current !== "number") return undefined;
  return operationalStates(endpoint).get(current) ?? STANDARD_STATES[current];
}
