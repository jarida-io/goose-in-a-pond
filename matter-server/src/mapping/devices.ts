/** A commissioned node projected onto GIAP's `Device`, from what the node itself states. */

import type { Device } from "../protocol.js";
import { deviceIdForNode } from "../protocol.js";
import {
  applicationEndpoints,
  endpointWith,
  hasCluster,
  rootAttribute,
  type EndpointSnapshot,
  type NodeSnapshot,
} from "./snapshot.js";
import { clusterHasFeature } from "./sensors.js";
import { operationsOf, settingsOf } from "./settings.js";
import { applianceSetpoint } from "./thermostat.js";

export const CLUSTER_ON_OFF = "onOff";
export const CLUSTER_LEVEL_CONTROL = "levelControl";
export const CLUSTER_COLOR_CONTROL = "colorControl";
export const CLUSTER_THERMOSTAT = "thermostat";
export const CLUSTER_DOOR_LOCK = "doorLock";
export const CLUSTER_FAN_CONTROL = "fanControl";
export const CLUSTER_WINDOW_COVERING = "windowCovering";
export const CLUSTER_BASIC_INFORMATION = "basicInformation";
export const CLUSTER_OCCUPANCY = "occupancySensing";
export const CLUSTER_BOOLEAN_STATE = "booleanState";
export const CLUSTER_TEMPERATURE = "temperatureMeasurement";
export const CLUSTER_HUMIDITY = "relativeHumidityMeasurement";
export const CLUSTER_SMOKE_CO_ALARM = "smokeCoAlarm";
export const CLUSTER_SWITCH = "switch";
/** A bridged device's own identity and reachability, as its hub reports them. */
export const CLUSTER_BRIDGED_DEVICE_INFO = "bridgedDeviceBasicInformation";
/** A valve: open, shut, and -- where it says so -- how far. */
export const CLUSTER_VALVE = "valveConfigurationAndControl";

/** Every cluster this module names, for the snapshot allowlist. */
export function deviceClusters(): ReadonlySet<string> {
  return new Set([
    CLUSTER_ON_OFF,
    CLUSTER_LEVEL_CONTROL,
    CLUSTER_COLOR_CONTROL,
    CLUSTER_THERMOSTAT,
    CLUSTER_DOOR_LOCK,
    CLUSTER_FAN_CONTROL,
    CLUSTER_WINDOW_COVERING,
    CLUSTER_BASIC_INFORMATION,
    CLUSTER_OCCUPANCY,
    CLUSTER_BOOLEAN_STATE,
    CLUSTER_TEMPERATURE,
    CLUSTER_HUMIDITY,
    CLUSTER_SMOKE_CO_ALARM,
    CLUSTER_SWITCH,
    CLUSTER_BRIDGED_DEVICE_INFO,
    CLUSTER_VALVE,
  ]);
}

/** Matter's Speaker device type. Its Level Control is volume, not brightness. */
export const SPEAKER_DEVICE_TYPE = 0x0022;

/** The endpoint whose Level Control is a volume (e.g. a TV's Speaker endpoint), if there is one. */
export function speakerEndpoint(node: NodeSnapshot): EndpointSnapshot | undefined {
  return applicationEndpoints(node).find(
    e => e.deviceTypes.includes(SPEAKER_DEVICE_TYPE) && CLUSTER_LEVEL_CONTROL in e.clusters,
  );
}

/** Matter Device Library type ids (Descriptor DeviceTypeList) → the GIAP types the UI has icons for. */
const DEVICE_TYPES: ReadonlyMap<number, string> = new Map([
  // Lighting
  [0x0100, "light"], // On/Off Light
  [0x0101, "light"], // Dimmable Light
  [0x010c, "light"], // Colour Temperature Light
  [0x010d, "light"], // Extended Colour Light
  // Plugs: their clusters alone cannot tell them from a bulb.
  [0x010a, "plug"], // On/Off Plug-in Unit
  [0x010b, "plug"], // Dimmable Plug-in Unit
  // In-wall modules: a plug, since Matter does not state what load is wired to them.
  [0x010f, "plug"], // Mounted On/Off Control
  [0x0110, "plug"], // Mounted Dimmable Load Control
  // Closures
  [0x000a, "lock"], // Door Lock
  [0x0202, "covering"], // Window Covering
  // Climate and air
  [0x0301, "thermostat"], // Thermostat
  [0x0072, "thermostat"], // Room Air Conditioner
  [0x002b, "fan"], // Fan
  [0x002c, "air"], // Air Purifier
  // Driven by a Thermostat setpoint; a water heater's own modes come via the ModeBase rule.
  [0x0309, "thermostat"], // Heat Pump
  [0x050f, "thermostat"], // Water Heater
  // Sensors
  [0x0015, "sensor"], // Contact Sensor
  [0x002d, "sensor"], // Air Quality Sensor
  [0x0106, "sensor"], // Light Sensor
  [0x0107, "sensor"], // Occupancy Sensor
  [0x0302, "sensor"], // Temperature Sensor
  [0x0305, "sensor"], // Pressure Sensor
  [0x0306, "sensor"], // Flow Sensor
  [0x0307, "sensor"], // Humidity Sensor
  // Boolean-state detectors: one bit and nothing sounds, so sensors rather than alarms.
  [0x0041, "sensor"], // Water Freeze Detector
  [0x0043, "sensor"], // Water Leak Detector
  [0x0044, "sensor"], // Rain Sensor
  // An alarm is not a sensor to a user: it is the thing that wakes them.
  [0x0076, "alarm"], // Smoke/CO Alarm
  // A switch only reports its position. Client types (remotes, e.g. 0x0103 On/Off Light Switch,
  // 0x0104 Dimmer Switch) are deliberately unmapped: they hold no server cluster to read or drive.
  [0x000f, "switch"], // Generic Switch
  // Not drivable, but a device: it owns the fabric membership and is what `decommission` acts on.
  [0x000e, "bridge"], // Aggregator
  // Appliances
  [0x0073, "appliance"], // Laundry Washer
  [0x0075, "appliance"], // Dishwasher
  [0x007c, "appliance"], // Laundry Dryer
  [0x0079, "appliance"], // Microwave Oven
  [0x0078, "appliance"], // Cooktop
  // Composed appliances, typed here rather than by whatever their first cabinet claims.
  [0x007b, "appliance"], // Oven
  [0x0070, "appliance"], // Refrigerator
  // Their parts too, for a cabinet or hob ring commissioned on its own.
  [0x0071, "appliance"], // Temperature Controlled Cabinet
  [0x0077, "appliance"], // Cook Surface
  // A cooker hood is a fan with a filter, and Fan Control is what it publishes.
  [0x007a, "fan"], // Extractor Hood
  [0x0074, "vacuum"], // Robotic Vacuum Cleaner
  [0x0303, "pump"], // Pump
  // Valves take `open`/`close`, not On/Off; an irrigation system is one or more of them.
  [0x0042, "valve"], // Water Valve
  [0x0040, "valve"], // Irrigation System
  // Media
  [0x0023, "media"], // Casting Video Player
  [0x0028, "media"], // Basic Video Player
  // A standalone speaker; `speakerEndpoint` already reads its Level Control as volume.
  [0x0022, "media"], // Speaker
]);

/** The GIAP type the node states: the first application endpoint naming a known type wins. */
export function deviceTypeFromDescriptor(node: NodeSnapshot): string | undefined {
  for (const endpoint of applicationEndpoints(node)) {
    for (const id of endpoint.deviceTypes) {
      const known = DEVICE_TYPES.get(id);
      if (known !== undefined) return known;
    }
  }
  return undefined;
}

/** Local to this module: matter.js hands numbers over as numbers, or not at all. */
function asNumber(value: unknown): number | undefined {
  return typeof value === "number" && Number.isFinite(value) ? value : undefined;
}

/**
 * Which colour controls the device has: its `colorCapabilities` claim, or where that claims
 * nothing (Google's Matter Virtual Device), the attributes it publishes.
 */
export function colorSupport(node: NodeSnapshot): { hueSaturation: boolean; temperature: boolean } {
  const raw = endpointWith(node, CLUSTER_COLOR_CONTROL)?.clusters[CLUSTER_COLOR_CONTROL]?.[
    "colorCapabilities"
  ];

  // Raw bitmap: bit 0 HueSaturation, bit 4 ColorTemperature (Matter 1.4, ColorControl 5.2.2.9).
  const numeric = asNumber(raw);
  const claimed =
    numeric !== undefined
      ? { hueSaturation: (numeric & 0x01) !== 0, temperature: (numeric & 0x10) !== 0 }
      : typeof raw === "object" && raw !== null
        ? {
            hueSaturation: (raw as { hueSaturation?: unknown }).hueSaturation === true,
            temperature: (raw as { colorTemperature?: unknown }).colorTemperature === true,
          }
        : { hueSaturation: false, temperature: false };

  if (claimed.hueSaturation || claimed.temperature) return claimed;

  // Claimed nothing: matter.js publishes an attribute only when a feature covers it.
  const state = endpointWith(node, CLUSTER_COLOR_CONTROL)?.clusters[CLUSTER_COLOR_CONTROL];
  return {
    hueSaturation: state?.["currentHue"] !== undefined || state?.["currentSaturation"] !== undefined,
    temperature: state?.["colorTemperatureMireds"] !== undefined,
  };
}

/** Is there a Level Control off any speaker endpoint? A TV with a backlight has both. */
export function levelIsBrightness(node: NodeSnapshot): boolean {
  return applicationEndpoints(node).some(
    e => CLUSTER_LEVEL_CONTROL in e.clusters && !e.deviceTypes.includes(SPEAKER_DEVICE_TYPE),
  );
}

/** Does this valve have a level: the LVL feature claimed, or a level published anyway? */
export function valveHasLevel(node: NodeSnapshot): boolean {
  const state = endpointWith(node, CLUSTER_VALVE)?.clusters[CLUSTER_VALVE];
  if (state === undefined) return false;
  if (clusterHasFeature(state, "level")) return true;
  return state["currentLevel"] !== undefined || state["targetLevel"] !== undefined;
}

/** Must cover what `describe` offers: a model reads this list before deciding to look closer. */
function capabilitiesOf(node: NodeSnapshot): string[] {
  const capabilities: string[] = [];
  const hasOnOff = hasCluster(node, CLUSTER_ON_OFF);

  if (hasOnOff) capabilities.push("power");
  if (hasCluster(node, CLUSTER_FAN_CONTROL)) {
    // A fan need not implement On/Off (MVD's does not); FanMode is its power switch.
    if (!hasOnOff) capabilities.push("power");
    capabilities.push("fan_speed");
  }
  const speaker = speakerEndpoint(node);
  if (speaker !== undefined) capabilities.push("volume");
  if (levelIsBrightness(node)) capabilities.push("brightness");
  if (hasCluster(node, CLUSTER_THERMOSTAT) || applianceSetpoint(node) !== undefined) {
    capabilities.push("temperature");
  }
  if (hasCluster(node, CLUSTER_DOOR_LOCK)) capabilities.push("lock");
  if (hasCluster(node, CLUSTER_VALVE)) {
    capabilities.push("valve");
    if (valveHasLevel(node)) capabilities.push("position");
  }
  if (hasCluster(node, CLUSTER_WINDOW_COVERING)) {
    capabilities.push("position");
    // Only a covering with slats to turn.
    const tilting = endpointWith(node, CLUSTER_WINDOW_COVERING)?.clusters[CLUSTER_WINDOW_COVERING]?.[
      "currentPositionTiltPercent100ths"
    ];
    if (tilting !== undefined) capabilities.push("tilt");
  }

  if (hasCluster(node, CLUSTER_COLOR_CONTROL)) {
    const colour = colorSupport(node);
    if (colour.hueSaturation) capabilities.push("color");
    if (colour.temperature) capabilities.push("color_temp");
  }

  if (settingsOf(node).length > 0) capabilities.push("mode");
  if (operationsOf(node) !== undefined) capabilities.push("operation");

  return capabilities;
}

function typeOf(node: NodeSnapshot): string {
  // The node's own word first: clusters only say what is drivable.
  const stated = deviceTypeFromDescriptor(node);
  if (stated !== undefined) return stated;

  if (hasCluster(node, CLUSTER_DOOR_LOCK)) return "lock";
  if (hasCluster(node, CLUSTER_THERMOSTAT)) return "thermostat";
  // Before On/Off: a fan with On/Off is still a fan.
  if (hasCluster(node, CLUSTER_FAN_CONTROL)) return "fan";
  if (hasCluster(node, CLUSTER_ON_OFF)) return "light";
  if (
    hasCluster(node, CLUSTER_OCCUPANCY) ||
    hasCluster(node, CLUSTER_BOOLEAN_STATE) ||
    hasCluster(node, CLUSTER_TEMPERATURE) ||
    hasCluster(node, CLUSTER_HUMIDITY)
  ) {
    return "sensor";
  }
  if (hasCluster(node, CLUSTER_SWITCH)) return "switch";
  // An unmapped device lands here with no capabilities: a missing mapping, not a broken device.
  return "matter";
}

function nonEmpty(value: unknown): string | undefined {
  if (typeof value !== "string") return undefined;
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : undefined;
}

function basicInfo(node: NodeSnapshot, attribute: string): string | undefined {
  return nonEmpty(rootAttribute(node, CLUSTER_BASIC_INFORMATION, attribute));
}

/** What a bridged device says about itself, which its hub relays on its behalf. */
function bridgedInfo(node: NodeSnapshot, attribute: string): string | undefined {
  if (node.rootEndpoint === undefined) return undefined;
  const own = node.endpoints.find(e => e.number === node.rootEndpoint);
  return nonEmpty(own?.clusters[CLUSTER_BRIDGED_DEVICE_INFO]?.[attribute]);
}

/** Hub-reported reachability. Fails open: `reachable` arrives by subscription, after the hub itself. */
function bridgedReachable(node: NodeSnapshot): boolean {
  if (node.rootEndpoint === undefined) return true;
  const own = node.endpoints.find(e => e.number === node.rootEndpoint);
  return own?.clusters[CLUSTER_BRIDGED_DEVICE_INFO]?.["reachable"] !== false;
}

/** Fallback names must be unique (hence the endpoint): duplicates make `resolve_device` ambiguous. */
export function nodeToDevice(node: NodeSnapshot): Device {
  const device_type = typeOf(node);
  const suffix =
    node.rootEndpoint === undefined
      ? node.nodeId.toString()
      : `${node.nodeId.toString()}-${node.rootEndpoint}`;
  const name =
    bridgedInfo(node, "nodeLabel") ??
    bridgedInfo(node, "productName") ??
    bridgedInfo(node, "vendorName") ??
    basicInfo(node, "nodeLabel") ??
    basicInfo(node, "productName") ??
    `${device_type.charAt(0).toUpperCase()}${device_type.slice(1)} ${suffix}`;

  return {
    id: deviceIdForNode(node.nodeId, node.rootEndpoint),
    name,
    device_type,
    capabilities: capabilitiesOf(node),
    online: node.online && bridgedReachable(node),
  };
}
