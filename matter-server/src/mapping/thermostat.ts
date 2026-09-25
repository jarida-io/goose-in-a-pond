/**
 * Which of a thermostat's two setpoints a request is about: Cool → cooling, Heat → heating,
 * Auto/Off → the one nearer the requested value. `minSetpointDeadBand` makes each bound the other.
 */

import type { EndpointSnapshot, NodeSnapshot } from "./snapshot.js";
import { endpointWith } from "./snapshot.js";

export const CLUSTER_THERMOSTAT = "thermostat";

/** Matter's SystemModeEnum, for the modes that name one setpoint unambiguously. */
const MODE_COOL = 3;
const MODE_HEAT = 4;
const MODE_EMERGENCY_HEAT = 5;

/** One of a thermostat's two setpoints, with the range it will actually accept. */
export interface Setpoint {
  /** "heating" or "cooling", for saying which one moved. */
  which: "heating" | "cooling";
  endpoint: number;
  attribute: string;
  /** Hundredths of a degree, as Matter reports them. Absent where unstated. */
  min?: number;
  max?: number;
}

function attr(endpoint: EndpointSnapshot, name: string): number | undefined {
  const value = endpoint.clusters[CLUSTER_THERMOSTAT]?.[name];
  return typeof value === "number" ? value : undefined;
}

/** Matter's SystemModeEnum by name, for the encoding matter.js may hand over instead. */
const SYSTEM_MODE_NAMES: ReadonlyMap<string, number> = new Map([
  ["off", 0],
  ["auto", 1],
  ["cool", MODE_COOL],
  ["heat", MODE_HEAT],
  ["emergencyheat", MODE_EMERGENCY_HEAT],
  ["precooling", 6],
  ["fanonly", 7],
  ["dry", 8],
  ["sleep", 9],
]);

/** The thermostat's mode, whether matter.js decoded it as a number or a name. */
export function systemMode(endpoint: EndpointSnapshot): number | undefined {
  const raw = endpoint.clusters[CLUSTER_THERMOSTAT]?.["systemMode"];
  if (typeof raw === "number") return raw;
  if (typeof raw === "string") {
    return SYSTEM_MODE_NAMES.get(raw.toLowerCase().replace(/[\s_-]/g, ""));
  }
  return undefined;
}

/**
 * Which setpoints it has: `controlSequenceOfOperation`, else the reported values (matter.js keys
 * every model attribute, so `in` proves nothing), else both rather than strip one that works.
 */
export function setpointsAvailable(endpoint: EndpointSnapshot): {
  heating: boolean;
  cooling: boolean;
} {
  // 0 CoolingOnly, 1 CoolingWithReheat, 2 HeatingOnly, 3 HeatingWithReheat,
  // 4 CoolingAndHeating, 5 CoolingAndHeatingWithReheat.
  const sequence = attr(endpoint, "controlSequenceOfOperation");
  if (sequence !== undefined) {
    return { cooling: sequence <= 1 || sequence >= 4, heating: sequence >= 2 };
  }

  const heating = attr(endpoint, "occupiedHeatingSetpoint") !== undefined;
  const cooling = attr(endpoint, "occupiedCoolingSetpoint") !== undefined;
  if (heating || cooling) return { heating, cooling };

  return { heating: true, cooling: true };
}

/** The configured limit, else the absolute one the hardware states. */
function floor(endpoint: EndpointSnapshot, configured: string, absolute: string) {
  return attr(endpoint, configured) ?? attr(endpoint, absolute);
}

/** The setpoints' required gap in hundredths; `minSetpointDeadBand` is in TENTHS of a degree (0–25). */
function deadband(endpoint: EndpointSnapshot): number {
  return (attr(endpoint, "minSetpointDeadBand") ?? 0) * 10;
}

function heatingSetpoint(endpoint: EndpointSnapshot): Setpoint {
  const cooling = attr(endpoint, "occupiedCoolingSetpoint");
  let max = floor(endpoint, "maxHeatSetpointLimit", "absMaxHeatSetpointLimit");

  // Capped below the cooling setpoint; an absent deadband is zero, so they still may not cross.
  if (cooling !== undefined) {
    const ceiling = cooling - deadband(endpoint);
    max = max === undefined ? ceiling : Math.min(max, ceiling);
  }

  const min = floor(endpoint, "minHeatSetpointLimit", "absMinHeatSetpointLimit");
  return {
    which: "heating",
    endpoint: endpoint.number,
    attribute: "occupiedHeatingSetpoint",
    ...(min === undefined ? {} : { min }),
    ...(max === undefined ? {} : { max }),
  };
}

function coolingSetpoint(endpoint: EndpointSnapshot): Setpoint {
  const heating = attr(endpoint, "occupiedHeatingSetpoint");
  let min = floor(endpoint, "minCoolSetpointLimit", "absMinCoolSetpointLimit");

  // The mirror image: held above the heating setpoint by the same band.
  if (heating !== undefined) {
    const bottom = heating + deadband(endpoint);
    min = min === undefined ? bottom : Math.max(min, bottom);
  }

  const max = floor(endpoint, "maxCoolSetpointLimit", "absMaxCoolSetpointLimit");
  return {
    which: "cooling",
    endpoint: endpoint.number,
    attribute: "occupiedCoolingSetpoint",
    ...(min === undefined ? {} : { min }),
    ...(max === undefined ? {} : { max }),
  };
}

/** The setpoint a request is about; without `celsius`, Auto and Off fall back to heating. */
export function targetSetpoint(node: NodeSnapshot, celsius?: number): Setpoint | undefined {
  const endpoint = endpointWith(node, CLUSTER_THERMOSTAT);
  if (endpoint === undefined) return undefined;

  const available = setpointsAvailable(endpoint);
  const heating = heatingSetpoint(endpoint);
  if (!available.cooling) return available.heating ? heating : undefined;
  if (!available.heating) return coolingSetpoint(endpoint);

  const cooling = coolingSetpoint(endpoint);
  switch (systemMode(endpoint)) {
    case MODE_COOL:
      return cooling;
    case MODE_HEAT:
    case MODE_EMERGENCY_HEAT:
      return heating;
    default:
      break;
  }

  // Auto or Off: the setpoint nearer the requested value.
  if (celsius === undefined) return heating;
  const hundredths = celsius * 100;
  const toHeating = Math.abs(hundredths - (attr(endpoint, "occupiedHeatingSetpoint") ?? 0));
  const toCooling = Math.abs(hundredths - (attr(endpoint, "occupiedCoolingSetpoint") ?? 0));
  return toCooling < toHeating ? cooling : heating;
}

/** Both setpoints' ranges together, for a mode that does not settle which one applies. */
export function reachableRange(node: NodeSnapshot): { min?: number; max?: number } | undefined {
  const endpoint = endpointWith(node, CLUSTER_THERMOSTAT);
  if (endpoint === undefined) return undefined;

  const available = setpointsAvailable(endpoint);
  const heating = heatingSetpoint(endpoint);
  if (!available.cooling) {
    return { ...(heating.min === undefined ? {} : { min: heating.min }),
             ...(heating.max === undefined ? {} : { max: heating.max }) };
  }

  const cooling = coolingSetpoint(endpoint);
  // Cool-only: the union would take its floor from a nonexistent heating setpoint.
  if (!available.heating) {
    return { ...(cooling.min === undefined ? {} : { min: cooling.min }),
             ...(cooling.max === undefined ? {} : { max: cooling.max }) };
  }

  const min = heating.min ?? cooling.min;
  const max = cooling.max ?? heating.max;
  return { ...(min === undefined ? {} : { min }), ...(max === undefined ? {} : { max }) };
}

/** An appliance's numeric Temperature Control setpoint, driven via `target_temp`; levels are a mode. */
export interface ApplianceSetpoint {
  endpoint: number;
  /** Hundredths of a degree, as Matter states them. Absent where unstated. */
  min?: number;
  max?: number;
  /** The increment the device accepts, if it says. */
  step?: number;
}

const TEMPERATURE_CONTROL = "temperatureControl";

export function applianceSetpoint(node: NodeSnapshot): ApplianceSetpoint | undefined {
  const endpoint = endpointWith(node, TEMPERATURE_CONTROL);
  const state = endpoint?.clusters[TEMPERATURE_CONTROL];
  if (endpoint === undefined || state === undefined) return undefined;

  // The number feature only; levels are read as a mode.
  const number = (name: string) => {
    const value = state[name];
    return typeof value === "number" ? value : undefined;
  };
  if (number("temperatureSetpoint") === undefined) return undefined;

  const min = number("minTemperature");
  const max = number("maxTemperature");
  const step = number("step");
  return {
    endpoint: endpoint.number,
    ...(min === undefined ? {} : { min }),
    ...(max === undefined ? {} : { max }),
    ...(step === undefined ? {} : { step }),
  };
}
