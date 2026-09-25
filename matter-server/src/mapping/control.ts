/** GIAP control verbs as pure Matter action plans; only `controller.ts` touches the network. */

import { OpError, type DeviceStatePatch, type Verb } from "../protocol.js";
import {
  levelIsBrightness,
  speakerEndpoint,
  valveHasLevel,
  CLUSTER_COLOR_CONTROL,
  CLUSTER_DOOR_LOCK,
  CLUSTER_FAN_CONTROL,
  CLUSTER_LEVEL_CONTROL,
  CLUSTER_ON_OFF,
  CLUSTER_THERMOSTAT,
  CLUSTER_VALVE,
  CLUSTER_WINDOW_COVERING,
} from "./devices.js";
import { operationsOf, settingNamed, settingsOf } from "./settings.js";
import { endpointWith, hasCluster, type NodeSnapshot } from "./snapshot.js";
import { applianceSetpoint, targetSetpoint } from "./thermostat.js";

export type { Verb };

/** Every verb, as a `Record` so the build fails if one is missing; `server.ts` rejects the rest. */
const ALL_VERBS: Record<Verb, true> = {
  power: true,
  brightness: true,
  volume: true,
  target_temp: true,
  locked: true,
  color: true,
  color_temp: true,
  fan_speed: true,
  fan_mode: true,
  position: true,
  tilt: true,
  mode: true,
  operation: true,
  valve: true,
};

export const VERBS: ReadonlySet<string> = new Set(Object.keys(ALL_VERBS));

/** What the server must actually do to the device. */
export type Action =
  | { kind: "command"; endpoint: number; cluster: string; command: string; payload: Record<string, unknown> }
  | { kind: "write"; endpoint: number; cluster: string; attribute: string; value: unknown };

export interface Plan {
  actions: Action[];
  /** The state the device is in once the actions succeed. */
  applied: DeviceStatePatch;
}

/**
 * What the device now reports for a verb whose result can differ from the request (quantised,
 * clamped, in transit), read via `state`'s inverses. Booleans are skipped: they can't land elsewhere.
 */
export function observedFor(node: NodeSnapshot, verb: Verb): DeviceStatePatch {
  const at = (cluster: string, attribute: string): number | undefined => {
    const raw = endpointWith(node, cluster)?.clusters[cluster]?.[attribute];
    return typeof raw === "number" && Number.isFinite(raw) ? raw : undefined;
  };

  switch (verb) {
    case "fan_speed": {
      // percentCurrent, not percentSetting: the setting only echoes what was written.
      const pct = at(CLUSTER_FAN_CONTROL, "percentCurrent");
      return pct === undefined ? {} : { fan_speed: clampPercent(pct) };
    }
    case "brightness": {
      // A television's Level Control is its volume, not a brightness (as in `describe`).
      if (!levelIsBrightness(node)) return {};
      const level = at(CLUSTER_LEVEL_CONTROL, "currentLevel");
      return level === undefined ? {} : { brightness: levelToBrightness(level) };
    }
    case "volume": {
      const speaker = speakerEndpoint(node);
      const level = speaker?.clusters[CLUSTER_LEVEL_CONTROL]?.["currentLevel"];
      return typeof level === "number" ? { volume: levelToBrightness(level) } : {};
    }
    case "color": {
      const hue = at(CLUSTER_COLOR_CONTROL, "currentHue");
      const saturation = at(CLUSTER_COLOR_CONTROL, "currentSaturation");
      if (hue === undefined || saturation === undefined) return {};
      return { hue: matterToHue(hue), saturation: matterToSaturation(saturation) };
    }
    case "color_temp": {
      const mireds = at(CLUSTER_COLOR_CONTROL, "colorTemperatureMireds");
      if (mireds === undefined) return {};
      const kelvin = miredsToKelvin(mireds);
      return kelvin > 0 ? { color_temp: kelvin } : {};
    }
    case "target_temp": {
      // The live setpoint, by the rule `target_temp` writes with: appliance's own, else the mode's.
      if (applianceSetpoint(node) !== undefined) {
        const set = at("temperatureControl", "temperatureSetpoint");
        return set === undefined ? {} : { target_temp: setpointToCelsius(set) };
      }
      const target = targetSetpoint(node);
      const setpoint = target === undefined ? undefined : at(CLUSTER_THERMOSTAT, target.attribute);
      return setpoint === undefined ? {} : { target_temp: setpointToCelsius(setpoint) };
    }
    case "position": {
      const lift = at(CLUSTER_WINDOW_COVERING, "currentPositionLiftPercent100ths");
      if (lift !== undefined) return { position: lift100thsToPositionOpen(lift) };
      // A valve's level is already a plain percentage, so there is nothing to convert.
      const level = at(CLUSTER_VALVE, "currentLevel");
      return typeof level === "number" ? { position: clampPercent(level) } : {};
    }
    case "valve": {
      const state = at(CLUSTER_VALVE, "currentState");
      // Transitioning: report nothing rather than guess a direction.
      if (state === VALVE_OPEN) return { valve: true };
      if (state === VALVE_CLOSED) return { valve: false };
      return {};
    }
    case "tilt": {
      const tilt = at(CLUSTER_WINDOW_COVERING, "currentPositionTiltPercent100ths");
      return tilt === undefined ? {} : { tilt: lift100thsToPositionOpen(tilt) };
    }
    default:
      // power, locked, fan_mode, mode land as asked; `operation` settles in the controller.
      return {};
  }
}

/** Valve `currentState`: shut or open; the third value, Transitioning, must not read as settled. */
const VALVE_CLOSED = 0;
const VALVE_OPEN = 1;

// ── Unit conversions ─────────────────────────────────────────────────────────

/** A 0-100 GIAP brightness percentage onto Matter's 0-254 level scale. */
export function brightnessToLevel(percent: number): number {
  const pct = clampPercent(percent);
  return Math.floor((pct * 254 + 50) / 100);
}

/** Matter's 0-254 level back to a 0-100 GIAP percentage, for reading state. */
export function levelToBrightness(level: number): number {
  return clampPercent((level * 100) / 254);
}

/** Kelvin to ColorControl mireds (1e6/K; the order inverts), clamped to the field's 1..0xfeff. */
export function kelvinToMireds(kelvin: number): number {
  if (!Number.isFinite(kelvin) || kelvin <= 0) return 0xfeff;
  return Math.min(0xfeff, Math.max(1, Math.round(1_000_000 / kelvin)));
}

/** Mireds back to kelvin, rounded to a whole degree — no device is that precise. */
export function miredsToKelvin(mireds: number): number {
  if (!Number.isFinite(mireds) || mireds <= 0) return 0;
  return Math.round(1_000_000 / mireds);
}

/** Celsius onto a Matter thermostat setpoint (hundredths of a degree). */
export function celsiusToSetpoint(celsius: number): number {
  return Math.min(32767, Math.max(-32768, Math.round(celsius * 100)));
}

/** A Matter thermostat setpoint back to Celsius. */
export function setpointToCelsius(setpoint: number): number {
  return Math.round(setpoint) / 100;
}

/** A 0-360 degree hue onto ColorControl's 0-254 scale; 360 wraps to 0. */
export function hueToMatter(degrees: number): number {
  const wrapped = ((Math.round(degrees) % 360) + 360) % 360;
  return Math.floor((wrapped * 254 + 180) / 360);
}

/** ColorControl's 0-254 hue back to degrees, for reading state. */
export function matterToHue(raw: number): number {
  const clamped = Math.min(254, Math.max(0, Math.round(raw)));
  return Math.round((clamped * 360) / 254) % 360;
}

/** A 0-100 saturation percentage onto Matter's 0-254 scale. */
export function saturationToMatter(percent: number): number {
  const pct = clampPercent(percent);
  return Math.floor((pct * 254 + 50) / 100);
}

/** Matter's 0-254 saturation back to a percentage. */
export function matterToSaturation(raw: number): number {
  return clampPercent((Math.min(254, Math.max(0, raw)) * 100) / 254);
}

/** GIAP percent OPEN onto WindowCovering lift in hundredths of a percent CLOSED (0 = fully open). */
export function positionOpenToLift100ths(percentOpen: number): number {
  return (100 - clampPercent(percentOpen)) * 100;
}

/** WindowCovering's hundredths-of-a-percent CLOSED back to GIAP percent OPEN. */
export function lift100thsToPositionOpen(lift100ths: number): number {
  return clampPercent(100 - lift100ths / 100);
}

function clampPercent(value: number): number {
  if (!Number.isFinite(value)) return 0;
  return Math.min(100, Math.max(0, Math.round(value)));
}

// ── Fan modes ────────────────────────────────────────────────────────────────

const CLUSTER_TEMPERATURE_CONTROL = "temperatureControl";

export const FAN_MODE_OFF = 0;

/**
 * High (3), not the deprecated `FanMode.On` (4): High is the only non-Off mode in every sequence.
 * TODO: pick the gentlest mode from `FanModeSequence`; `fan_mode` validation has the same gap.
 */
export const FAN_MODE_ON = 3;

/** `FanMode` by the name a user says; Auto and Smart aren't speeds, so fans need modes too. */
export function fanModeFromName(name: string): number | undefined {
  switch (name.trim().toLowerCase()) {
    case "off":
      return FAN_MODE_OFF;
    case "low":
      return 1;
    case "medium":
    case "med":
      return 2;
    case "high":
      return 3;
    case "on":
      return FAN_MODE_ON;
    case "auto":
      return 5;
    case "smart":
      return 6;
    default:
      return undefined;
  }
}

/** `FanMode` back to the name it is sent by, for reading state. */
export function fanModeName(code: number): string | undefined {
  switch (code) {
    case FAN_MODE_OFF:
      return "off";
    case 1:
      return "low";
    case 2:
      return "medium";
    case 3:
      return "high";
    case 4:
      return "on";
    case 5:
      return "auto";
    case 6:
      return "smart";
    default:
      return undefined;
  }
}

// ── Planning ─────────────────────────────────────────────────────────────────

function endpointFor(node: NodeSnapshot, cluster: string, deviceId: string): number {
  const endpoint = endpointWith(node, cluster);
  if (endpoint === undefined) {
    throw new OpError(
      "capability_unsupported",
      `Matter device '${deviceId}' does not support this capability`,
    );
  }
  return endpoint.number;
}

function asPercent(value: unknown, verb: string): number {
  if (typeof value !== "number" || !Number.isFinite(value)) {
    throw new OpError("bad_request", `${verb} needs a number from 0 to 100`);
  }
  return clampPercent(value);
}

function asBoolean(value: unknown, verb: string): boolean {
  if (typeof value !== "boolean") {
    throw new OpError("bad_request", `${verb} needs true or false`);
  }
  return value;
}

/**
 * Resolves `verb` against what `node` exposes.
 * @throws {OpError} `capability_unsupported` (no cluster for the verb) or `bad_request` (bad value).
 */
export function planControl(
  node: NodeSnapshot,
  deviceId: string,
  verb: Verb,
  value: unknown,
): Plan {
  switch (verb) {
    case "power": {
      const on = asBoolean(value, "power");
      const onOff = endpointWith(node, CLUSTER_ON_OFF);
      if (onOff !== undefined) {
        return {
          actions: [
            { kind: "command", endpoint: onOff.number, cluster: CLUSTER_ON_OFF, command: on ? "on" : "off", payload: {} },
          ],
          applied: { on },
        };
      }
      // A fan's power is a FanMode write: most fans (MVD's too) have no On/Off cluster.
      const fan = endpointWith(node, CLUSTER_FAN_CONTROL);
      if (fan !== undefined) {
        return {
          actions: [
            { kind: "write", endpoint: fan.number, cluster: CLUSTER_FAN_CONTROL, attribute: "fanMode", value: on ? FAN_MODE_ON : FAN_MODE_OFF },
          ],
          applied: { on },
        };
      }
      throw new OpError(
        "capability_unsupported",
        `Matter device '${deviceId}' cannot be switched on or off`,
      );
    }

    case "volume": {
      const pct = asPercent(value, "volume");
      const speaker = speakerEndpoint(node);
      if (speaker === undefined) {
        throw new OpError(
          "capability_unsupported",
          `Matter device '${deviceId}' has no speaker to set a volume on`,
        );
      }
      // `moveToLevel`, since `currentLevel` is read-only; not `...WithOnOff`, as a muted TV stays on.
      return {
        actions: [
          {
            kind: "command",
            endpoint: speaker.number,
            cluster: CLUSTER_LEVEL_CONTROL,
            command: "moveToLevel",
            payload: {
              level: brightnessToLevel(pct),
              transitionTime: 0,
              optionsMask: {},
              optionsOverride: {},
            },
          },
        ],
        applied: { volume: pct },
      };
    }

    case "brightness": {
      const pct = asPercent(value, "brightness");
      const endpoint = endpointFor(node, CLUSTER_LEVEL_CONTROL, deviceId);
      return {
        actions: [
          {
            kind: "command",
            endpoint,
            cluster: CLUSTER_LEVEL_CONTROL,
            command: "moveToLevelWithOnOff",
            payload: { level: brightnessToLevel(pct), transitionTime: 0, optionsMask: {}, optionsOverride: {} },
          },
        ],
        applied: { brightness: pct, on: pct > 0 },
      };
    }

    case "target_temp": {
      if (typeof value !== "number" || !Number.isFinite(value)) {
        throw new OpError("bad_request", "target_temp needs a temperature in Celsius");
      }
      // Appliances take it by command; a thermostat needs the setpoint its mode is running.
      const appliance = applianceSetpoint(node);
      if (appliance !== undefined) {
        return {
          actions: [
            {
              kind: "command",
              endpoint: appliance.endpoint,
              cluster: CLUSTER_TEMPERATURE_CONTROL,
              command: "setTemperature",
              payload: { targetTemperature: celsiusToSetpoint(value) },
            },
          ],
          applied: { target_temp: value },
        };
      }

      const setpoint = targetSetpoint(node, value);
      if (setpoint === undefined) {
        throw new OpError(
          "capability_unsupported",
          `Matter device '${deviceId}' does not support this capability`,
        );
      }
      return {
        actions: [
          { kind: "write", endpoint: setpoint.endpoint, cluster: CLUSTER_THERMOSTAT, attribute: setpoint.attribute, value: celsiusToSetpoint(value) },
        ],
        applied: { target_temp: value },
      };
    }

    case "tilt": {
      // Slat angle, independent of lift; percent OPEN as for lift, since the spec treats 0 as open.
      const pct = asPercent(value, "tilt");
      const endpoint = endpointFor(node, CLUSTER_WINDOW_COVERING, deviceId);
      return {
        actions: [
          {
            kind: "command",
            endpoint,
            cluster: CLUSTER_WINDOW_COVERING,
            command: "goToTiltPercentage",
            payload: { tiltPercent100thsValue: positionOpenToLift100ths(pct) },
          },
        ],
        applied: { tilt: pct },
      };
    }

    case "valve": {
      const open = asBoolean(value, "valve");
      const endpoint = endpointFor(node, CLUSTER_VALVE, deviceId);
      return {
        actions: [
          // No payload: nobody asked for an auto-close or a level, which the valve may not support.
          { kind: "command", endpoint, cluster: CLUSTER_VALVE, command: open ? "open" : "close", payload: {} },
        ],
        applied: { valve: open },
      };
    }

    case "locked": {
      const locked = asBoolean(value, "locked");
      const endpoint = endpointFor(node, CLUSTER_DOOR_LOCK, deviceId);
      return {
        actions: [
          { kind: "command", endpoint, cluster: CLUSTER_DOOR_LOCK, command: locked ? "lockDoor" : "unlockDoor", payload: {} },
        ],
        applied: { locked },
      };
    }

    case "color_temp": {
      if (typeof value !== "number" || !Number.isFinite(value) || value <= 0) {
        throw new OpError(
          "bad_request",
          "color_temp needs a colour temperature in kelvin, e.g. 2700 for warm white",
        );
      }
      const kelvin = value;
      const endpoint = endpointFor(node, CLUSTER_COLOR_CONTROL, deviceId);
      return {
        actions: [
          {
            kind: "command",
            endpoint,
            cluster: CLUSTER_COLOR_CONTROL,
            command: "moveToColorTemperature",
            payload: {
              colorTemperatureMireds: kelvinToMireds(kelvin),
              transitionTime: 0,
              optionsMask: {},
              optionsOverride: {},
            },
          },
        ],
        // The kelvin the device will sit at: whole mireds make the round trip lossy.
        applied: { color_temp: miredsToKelvin(kelvinToMireds(kelvin)) },
      };
    }

    case "color": {
      const { hue, saturation } = readColor(value);
      const endpoint = endpointFor(node, CLUSTER_COLOR_CONTROL, deviceId);
      return {
        actions: [
          {
            kind: "command",
            endpoint,
            cluster: CLUSTER_COLOR_CONTROL,
            command: "moveToHueAndSaturation",
            payload: {
              hue: hueToMatter(hue),
              saturation: saturationToMatter(saturation),
              transitionTime: 0,
              optionsMask: {},
              optionsOverride: {},
            },
          },
        ],
        applied: { hue: ((Math.round(hue) % 360) + 360) % 360, saturation },
      };
    }

    case "fan_speed": {
      const pct = asPercent(value, "fan_speed");
      const endpoint = endpointFor(node, CLUSTER_FAN_CONTROL, deviceId);
      // Fan speed is the `percentSetting` attribute (0-100), not a command.
      return {
        actions: [
          { kind: "write", endpoint, cluster: CLUSTER_FAN_CONTROL, attribute: "percentSetting", value: pct },
        ],
        applied: { fan_speed: pct, on: pct > 0 },
      };
    }

    case "fan_mode": {
      if (typeof value !== "string") {
        throw new OpError("bad_request", "fan_mode needs a mode name");
      }
      const code = fanModeFromName(value);
      if (code === undefined) {
        throw new OpError(
          "bad_request",
          `'${value}' is not a fan mode — use off, low, medium, high, on, auto or smart`,
        );
      }
      const endpoint = endpointFor(node, CLUSTER_FAN_CONTROL, deviceId);
      return {
        actions: [
          { kind: "write", endpoint, cluster: CLUSTER_FAN_CONTROL, attribute: "fanMode", value: code },
        ],
        // Off is the one mode that says something definite about power.
        applied: { fan_mode: value.trim().toLowerCase(), on: code !== FAN_MODE_OFF },
      };
    }

    case "position": {
      const pct = asPercent(value, "position");
      // A valve takes percent OPEN as `open`'s targetLevel; coverings win if a device has both.
      if (!hasCluster(node, CLUSTER_WINDOW_COVERING) && hasCluster(node, CLUSTER_VALVE)) {
        if (!valveHasLevel(node)) {
          throw new OpError(
            "capability_unsupported",
            `Matter device '${deviceId}' is a valve with no level -- it can only be opened or shut, with the 'valve' verb`,
          );
        }
        return {
          actions: [
            {
              kind: "command",
              endpoint: endpointFor(node, CLUSTER_VALVE, deviceId),
              cluster: CLUSTER_VALVE,
              // 0% means close: the spec limits `open`'s targetLevel to 1..100.
              command: pct === 0 ? "close" : "open",
              payload: pct === 0 ? {} : { targetLevel: pct },
            },
          ],
          applied: pct === 0 ? { position: 0, valve: false } : { position: pct, valve: true },
        };
      }
      const endpoint = endpointFor(node, CLUSTER_WINDOW_COVERING, deviceId);
      return {
        actions: [
          {
            kind: "command",
            endpoint,
            cluster: CLUSTER_WINDOW_COVERING,
            command: "goToLiftPercentage",
            payload: { liftPercent100thsValue: positionOpenToLift100ths(pct) },
          },
        ],
        applied: { position: pct },
      };
    }

    case "mode": {
      const { setting: wanted, value: choice } = readModeRequest(value);
      const setting = settingNamed(node, wanted);
      if (setting === undefined) {
        const available = settingsOf(node).map(s => s.name);
        throw new OpError(
          "capability_unsupported",
          available.length === 0
            ? `Matter device '${deviceId}' has no settings that can be chosen`
            : `'${wanted}' is not a setting on '${deviceId}' — it has: ${available.join(", ")}`,
        );
      }

      // Only the device's own labels; guessing the nearest could pick the wrong wash cycle.
      const encoded = setting.valueFor(choice);
      if (encoded === undefined) {
        throw new OpError(
          "bad_request",
          `'${choice}' is not a ${setting.name} on '${deviceId}' — it accepts: ${setting.values.join(", ")}`,
        );
      }

      const action: Action =
        setting.write.kind === "command"
          ? {
              kind: "command",
              endpoint: setting.endpoint,
              cluster: setting.cluster,
              command: setting.write.command,
              payload: { [setting.write.field]: encoded },
            }
          : {
              kind: "write",
              endpoint: setting.endpoint,
              cluster: setting.cluster,
              attribute: setting.write.attribute,
              value: encoded,
            };

      return {
        actions: [action],
        // Reported with the label the device uses, not the one the user typed.
        applied: {
          mode: {
            setting: setting.name,
            value: setting.values.find(v => v.toLowerCase() === choice.trim().toLowerCase()) ?? choice,
          },
        },
      };
    }

    case "operation": {
      const wanted = asString(value, "operation").toLowerCase();
      const operations = operationsOf(node);
      if (operations === undefined) {
        throw new OpError(
          "capability_unsupported",
          `Matter device '${deviceId}' does not run cycles, so it cannot be started or stopped`,
        );
      }
      if (!operations.values.includes(wanted)) {
        throw new OpError(
          "bad_request",
          `'${wanted}' is not an operation — use ${operations.values.join(", ")}`,
        );
      }
      return {
        actions: [
          {
            kind: "command",
            endpoint: operations.endpoint,
            cluster: operations.cluster,
            command: wanted,
            payload: {},
          },
        ],
        applied: { operation: wanted },
      };
    }
  }
}

/** A `mode` request names the setting and the choice, both as the device words them. */
function readModeRequest(value: unknown): { setting: string; value: string } {
  if (
    typeof value === "object" &&
    value !== null &&
    typeof (value as { setting?: unknown }).setting === "string" &&
    typeof (value as { value?: unknown }).value === "string"
  ) {
    const request = value as { setting: string; value: string };
    return { setting: request.setting.trim(), value: request.value.trim() };
  }
  throw new OpError(
    "bad_request",
    "a mode needs both the setting and the value, as {setting, value}",
  );
}

function asString(value: unknown, verb: string): string {
  if (typeof value !== "string") {
    throw new OpError("bad_request", `${verb} takes a name, not ${typeof value}`);
  }
  return value.trim();
}

function readColor(value: unknown): { hue: number; saturation: number } {
  if (
    typeof value === "object" &&
    value !== null &&
    typeof (value as { hue?: unknown }).hue === "number" &&
    typeof (value as { saturation?: unknown }).saturation === "number"
  ) {
    const color = value as { hue: number; saturation: number };
    return { hue: color.hue, saturation: clampPercent(color.saturation) };
  }
  throw new OpError("bad_request", "color needs { hue, saturation }");
}
