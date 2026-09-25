/**
 * A device's current state, named as `describe` names things (verb, setting or sensor type) and
 * read through the inverses of `control`'s conversions. Unreported values are absent, not guessed.
 */

import type { DeviceState, StateValue } from "../protocol.js";
import { deviceIdForNode } from "../protocol.js";
import {
  fanModeName,
  matterToHue,
  matterToSaturation,
  miredsToKelvin,
  levelToBrightness,
  lift100thsToPositionOpen,
  setpointToCelsius,
} from "./control.js";
import {
  levelIsBrightness,
  speakerEndpoint,
  CLUSTER_COLOR_CONTROL,
  CLUSTER_DOOR_LOCK,
  CLUSTER_SMOKE_CO_ALARM,
  CLUSTER_SWITCH,
  CLUSTER_FAN_CONTROL,
  CLUSTER_LEVEL_CONTROL,
  CLUSTER_ON_OFF,
  CLUSTER_THERMOSTAT,
  CLUSTER_VALVE,
  CLUSTER_WINDOW_COVERING,
} from "./devices.js";
import { doorStateWord, expressedStateWord, switchKindOf, valveStateWord } from "./describe.js";
import { sensorApplies, SENSORS } from "./sensors.js";
import { observedOperation, settingsOf } from "./settings.js";
import { applianceSetpoint, targetSetpoint } from "./thermostat.js";
import { endpointWith, type NodeSnapshot } from "./snapshot.js";

/** DoorLock's `lockState`: 0 is not-fully-locked, which is neither of the two. */
const LOCK_STATES: Record<number, string> = {
  0: "jammed between locked and unlocked",
  1: "locked",
  2: "unlocked",
};

function valueAt(node: NodeSnapshot, cluster: string, attribute: string): unknown {
  return endpointWith(node, cluster)?.clusters[cluster]?.[attribute];
}

function numberAt(node: NodeSnapshot, cluster: string, attribute: string): number | undefined {
  const value = valueAt(node, cluster, attribute);
  return typeof value === "number" ? value : undefined;
}

/** Is a valve reporting any fault? `valveFault` may arrive as decoded flags or a raw bitmap. */
function faultsPresent(raw: unknown): boolean {
  if (typeof raw === "number") return raw !== 0;
  if (typeof raw === "object" && raw !== null) {
    return Object.values(raw as Record<string, unknown>).some(flag => flag === true);
  }
  return false;
}

/** Everything this device currently reports, in the order a person would ask. */
export function stateOf(node: NodeSnapshot): DeviceState {
  const values: StateValue[] = [];
  const add = (name: string, value: string | undefined) => {
    if (value !== undefined) values.push({ name, value });
  };

  const on = endpointWith(node, CLUSTER_ON_OFF)?.clusters[CLUSTER_ON_OFF]?.["onOff"];
  if (typeof on === "boolean") add("power", on ? "on" : "off");

  const speaker = speakerEndpoint(node);
  const speakerLevel = speaker?.clusters[CLUSTER_LEVEL_CONTROL]?.["currentLevel"];
  if (typeof speakerLevel === "number") add("volume", `${levelToBrightness(speakerLevel)}%`);

  if (levelIsBrightness(node)) {
    const level = numberAt(node, CLUSTER_LEVEL_CONTROL, "currentLevel");
    if (level !== undefined) add("brightness", `${levelToBrightness(level)}%`);
  }

  // The setpoint `target_temp` would write: the one the current mode has live.
  const appliance = applianceSetpoint(node);
  if (appliance !== undefined) {
    const set = numberAt(node, "temperatureControl", "temperatureSetpoint");
    if (set !== undefined) add("target_temp", `${setpointToCelsius(set)} C`);
  } else {
    const target = targetSetpoint(node);
    const setpoint =
      target === undefined ? undefined : numberAt(node, CLUSTER_THERMOSTAT, target.attribute);
    if (setpoint !== undefined) add("target_temp", `${setpointToCelsius(setpoint)} C`);
  }

  const lock = numberAt(node, CLUSTER_DOOR_LOCK, "lockState");
  if (lock !== undefined) add("locked", LOCK_STATES[lock]);

  // Door position, which `locked` cannot answer: a bolt thrown into an open frame reads "locked".
  add("door", doorStateWord(valueAt(node, CLUSTER_DOOR_LOCK, "doorState")));

  // A valve's level is reported as `position`, the control that sets it.
  add("valve_state", valveStateWord(valueAt(node, CLUSTER_VALVE, "currentState")));
  const valveLevel = numberAt(node, CLUSTER_VALVE, "currentLevel");
  if (valveLevel !== undefined) add("position", `${Math.round(valveLevel)}%`);
  const valveFault = valueAt(node, CLUSTER_VALVE, "valveFault");
  if (valveFault !== undefined) add("valve_fault", faultsPresent(valveFault) ? "yes" : "no");

  // Reported so it can be checked, never set. See `statesOf` in describe.ts.
  const pin = valueAt(node, CLUSTER_DOOR_LOCK, "requirePinForRemoteOperation");
  if (typeof pin === "boolean") add("pin_required", pin ? "required" : "not required");

  const position = numberAt(node, CLUSTER_SWITCH, "currentPosition");
  if (position !== undefined) add("switch_position", `${position}`);
  add("switch_kind", switchKindOf(node));
  // By the `colorMode` it is IN: a bulb at 2700K keeps a stale `currentHue`.
  const colorMode = valueAt(node, CLUSTER_COLOR_CONTROL, "colorMode");
  const inTemperatureMode =
    colorMode === 2 || (typeof colorMode === "string" && /temperature|mireds/i.test(colorMode));

  if (inTemperatureMode) {
    const mireds = numberAt(node, CLUSTER_COLOR_CONTROL, "colorTemperatureMireds");
    const kelvin = mireds === undefined ? 0 : miredsToKelvin(mireds);
    if (kelvin > 0) add("color_temp", `${kelvin} K`);
  } else {
    const hue = numberAt(node, CLUSTER_COLOR_CONTROL, "currentHue");
    const saturation = numberAt(node, CLUSTER_COLOR_CONTROL, "currentSaturation");
    if (hue !== undefined && saturation !== undefined) {
      add("color", `hue ${matterToHue(hue)}, saturation ${matterToSaturation(saturation)}%`);
    }
  }

  add("alarm", expressedStateWord(valueAt(node, CLUSTER_SMOKE_CO_ALARM, "expressedState")));

  const service = valueAt(node, CLUSTER_SMOKE_CO_ALARM, "endOfServiceAlert");
  if (service !== undefined) {
    // EndOfServiceEnum: 0 normal, 1 expired. Tolerant of the name, as everywhere else.
    const expired =
      service === 1 || (typeof service === "string" && /expire/i.test(service));
    add("alarm_service", expired ? "expired" : "normal");
  }

  const fault = valueAt(node, CLUSTER_SMOKE_CO_ALARM, "hardwareFaultAlert");
  if (typeof fault === "boolean") add("alarm_fault", fault ? "faulty" : "ok");

  const speed = numberAt(node, CLUSTER_FAN_CONTROL, "percentCurrent");
  if (speed !== undefined) add("fan_speed", `${speed}%`);
  const fanMode = numberAt(node, CLUSTER_FAN_CONTROL, "fanMode");
  if (fanMode !== undefined) add("fan_mode", fanModeName(fanMode));

  // Percent open (WindowCovering stores percent closed), plus the target while it differs: a
  // covering that took the command but has not moved otherwise looks like one that ignored it.
  const lift = numberAt(node, CLUSTER_WINDOW_COVERING, "currentPositionLiftPercent100ths");
  if (lift !== undefined) {
    const target = numberAt(node, CLUSTER_WINDOW_COVERING, "targetPositionLiftPercent100ths");
    const here = `${lift100thsToPositionOpen(lift)}% open`;
    add(
      "position",
      target === undefined || target === lift
        ? here
        : `${here}, moving to ${lift100thsToPositionOpen(target)}% open`,
    );
  }

  // The slat angle, read the same way and reported under the name that sets it.
  const tilt = numberAt(node, CLUSTER_WINDOW_COVERING, "currentPositionTiltPercent100ths");
  if (tilt !== undefined) {
    const target = numberAt(node, CLUSTER_WINDOW_COVERING, "targetPositionTiltPercent100ths");
    const here = `${lift100thsToPositionOpen(tilt)}% open`;
    add(
      "tilt",
      target === undefined || target === tilt
        ? here
        : `${here}, turning to ${lift100thsToPositionOpen(target)}% open`,
    );
  }

  for (const setting of settingsOf(node)) {
    add(setting.name, currentLabel(node, setting));
  }

  add("operation", observedOperation(node));

  for (const sensor of SENSORS) {
    const endpoint = endpointWith(node, sensor.cluster);
    if (!sensorApplies(sensor, endpoint?.deviceTypes ?? [])) continue;
    const raw = endpoint?.clusters[sensor.cluster]?.[sensor.attribute];
    const reading = sensor.read(raw);
    if (reading === undefined) continue;

    // Enum readings in the device's words; the number stays in the reading for rule thresholds.
    const worded = sensor.words?.[reading];
    if (worded !== undefined) {
      add(sensor.sensorType, worded);
      continue;
    }

    // A percentage closes up, matching fan speed's "50%"; other units keep their space.
    add(sensor.sensorType, sensor.unit === "%" ? `${reading}%` : `${reading} ${sensor.unit}`);
  }

  // Endpoint included, or every bridged child reports its hub's id.
  return { device_id: deviceIdForNode(node.nodeId, node.rootEndpoint), values };
}

/** The label a setting is currently on, as the device words it. */
function currentLabel(
  node: NodeSnapshot,
  setting: ReturnType<typeof settingsOf>[number],
): string | undefined {
  const state = node.endpoints.find(e => e.number === setting.endpoint)?.clusters[setting.cluster];
  if (state === undefined) return undefined;

  const current =
    setting.current !== undefined
      ? state[setting.current]
      : setting.write.kind === "command"
        ? state["currentMode"]
        : state[setting.write.attribute];
  if (typeof current !== "number") return undefined;

  // ModeBase codes need not be list positions, so match by code rather than index.
  return setting.values.find(label => setting.valueFor(label) === current);
}
