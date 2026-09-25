/**
 * The virtual device's command line. Shapes: `--device X` (one device), `--device X
 * --part Y` (composed: still one device), `--bridged X=Label` (one device per child).
 */

import { tmpdir } from "node:os";
import { join } from "node:path";

export interface DeviceSpec {
  /** matter.js device module name, kebab-cased: `dimmable-light`. */
  device: string;
  /** Endpoint id, also the handle stdin commands use. */
  id: string;
  /** The name the device reports for itself. */
  label?: string;
  /** A forced Matter endpoint number, so a bridged device can sit above its own part. */
  number?: number;
  /** Construction-time `behavior -> attribute -> value`; matter.js refuses some types bare. */
  state: Record<string, Record<string, unknown>>;
  parts: DeviceSpec[];
}

export interface Spec {
  id: string;
  name: string;
  port: number;
  storage: string;
  passcode: number;
  discriminator: number;
  vendorId: number;
  productId: number;
  device?: DeviceSpec;
  parts: DeviceSpec[];
  bridged: DeviceSpec[];
}

/** The same values MVD's own form offers, so a case can be reproduced on either. */
const DEFAULTS = {
  // Not 5540: that belongs to whichever device app wants it, MVD included.
  port: 5541,
  passcode: 20202021,
  discriminator: 3841,
  vendorId: 0xfff1,
  productId: 0x8001,
} as const;

function fail(message: string): never {
  throw new Error(message);
}

function asNumber(raw: string, what: string): number {
  // Hex too: MVD's form shows vendor and product ids as 0xFFF1 / 0x8000.
  const value = /^0x/i.test(raw) ? Number.parseInt(raw, 16) : Number.parseInt(raw, 10);
  if (!Number.isFinite(value)) fail(`${what} must be a number, got '${raw}'`);
  return value;
}

/** `dimmable-light=Kitchen Light@7` -> device, label, forced endpoint number. */
export function parseDevice(raw: string, fallbackId: string): DeviceSpec {
  const at = raw.lastIndexOf("@");
  const withoutNumber = at === -1 ? raw : raw.slice(0, at);
  const number = at === -1 ? undefined : asNumber(raw.slice(at + 1), "an endpoint number");

  const equals = withoutNumber.indexOf("=");
  const device = (equals === -1 ? withoutNumber : withoutNumber.slice(0, equals)).trim();
  const label = equals === -1 ? undefined : withoutNumber.slice(equals + 1).trim();

  if (device.length === 0) fail(`expected a device type, got '${raw}'`);
  if (!/^[a-z0-9-]+$/.test(device)) {
    fail(`'${device}' is not a device module name — they are kebab-case, like dimmable-light`);
  }

  const id = slug(label ?? device) || fallbackId;
  return {
    device,
    id,
    ...(label === undefined || label.length === 0 ? {} : { label }),
    ...(number === undefined ? {} : { number }),
    state: {},
    parts: [],
  };
}

function slug(text: string): string {
  return text.toLowerCase().replace(/[^a-z0-9]+/g, "-").replace(/^-|-$/g, "");
}

/** Make every endpoint id distinct, since stdin addresses endpoints by id. */
function uniquify(specs: DeviceSpec[]): void {
  const seen = new Map<string, number>();
  const walk = (spec: DeviceSpec): void => {
    const count = (seen.get(spec.id) ?? 0) + 1;
    seen.set(spec.id, count);
    if (count > 1) spec.id = `${spec.id}-${count}`;
    spec.parts.forEach(walk);
  };
  specs.forEach(walk);
}

/** `kitchen.onOff.onOff=true` -> that endpoint's construction-time state. */
function applyAttrs(roots: DeviceSpec[], attrs: string[]): void {
  const byId = new Map<string, DeviceSpec>();
  const index = (spec: DeviceSpec): void => {
    byId.set(spec.id, spec);
    spec.parts.forEach(index);
  };
  roots.forEach(index);

  for (const raw of attrs) {
    const match = /^([\w-]+)\.(\w+)\.(\w+)=(.*)$/.exec(raw);
    if (match === null) {
      fail(`--attr wants <endpoint>.<behavior>.<attribute>=<value>, got '${raw}'`);
    }
    const [, endpointId, behavior, attribute, value] = match;
    const spec = byId.get(endpointId!);
    if (spec === undefined) {
      fail(`--attr names endpoint '${endpointId}', which is not one of: ${[...byId.keys()].join(", ")}`);
    }
    let parsed: unknown;
    try {
      parsed = JSON.parse(value!);
    } catch {
      parsed = value;
    }
    (spec.state[behavior!] ??= {})[attribute!] = parsed;
  }
}

export function parseArgs(argv: string[]): Spec {
  let device: DeviceSpec | undefined;
  const bridged: DeviceSpec[] = [];
  const rootParts: DeviceSpec[] = [];
  let name: string | undefined;
  let id: string | undefined;
  let storage: string | undefined;
  const attrs: string[] = [];
  const numbers = { ...DEFAULTS } as Record<string, number>;

  const value = (index: number, flag: string): string => {
    const next = argv[index + 1];
    if (next === undefined || next.startsWith("--")) fail(`${flag} needs a value`);
    return next;
  };

  for (let i = 0; i < argv.length; i++) {
    const flag = argv[i]!;
    switch (flag) {
      case "--device":
        if (device !== undefined) fail("--device may only be given once; use --part or --bridged");
        device = parseDevice(value(i, flag), "device");
        i++;
        break;
      case "--bridged":
        bridged.push(parseDevice(value(i, flag), `bridged-${bridged.length + 1}`));
        i++;
        break;
      case "--part": {
        // Attaches to whatever was named last, so a composed device can sit behind a bridge.
        const owner = bridged.at(-1);
        const parts = owner?.parts ?? (device === undefined ? undefined : rootParts);
        if (parts === undefined) fail("--part needs a --device or --bridged before it");
        parts.push(parseDevice(value(i, flag), `part-${parts.length + 1}`));
        i++;
        break;
      }
      case "--name":
        name = value(i, flag);
        i++;
        break;
      case "--id":
        id = slug(value(i, flag));
        i++;
        break;
      // Not `--storage`: matter.js's `Environment` parses our argv, so `--storage /path` makes
      // `storage` a scalar and `vars.set("storage.path", ...)` then fails.
      case "--storage-dir":
        storage = value(i, flag);
        i++;
        break;
      case "--attr":
        attrs.push(value(i, flag));
        i++;
        break;
      case "--port":
      case "--passcode":
      case "--discriminator":
        numbers[flag.slice(2)] = asNumber(value(i, flag), flag);
        i++;
        break;
      case "--vendor-id":
        numbers["vendorId"] = asNumber(value(i, flag), flag);
        i++;
        break;
      case "--product-id":
        numbers["productId"] = asNumber(value(i, flag), flag);
        i++;
        break;
      default:
        fail(`unknown option '${flag}'`);
    }
  }

  if (device === undefined && bridged.length === 0) {
    fail("nothing to be: give --device <type> or --bridged <type> (repeatable)");
  }

  device?.parts.push(...rootParts);
  const roots = device === undefined ? bridged : [device, ...bridged];
  uniquify(roots);
  // After uniquify, so `--attr` names the same endpoint id `list` prints.
  applyAttrs(roots, attrs);

  const resolvedName = name ?? (device === undefined ? "Virtual Bridge" : `Virtual ${device.device}`);
  const resolvedId = id ?? slug(resolvedName);

  return {
    id: resolvedId,
    name: resolvedName,
    // Per-instance by default, so two running at once cannot share a lock.
    storage: storage ?? join(tmpdir(), "giap-virtual-device", resolvedId),
    port: numbers["port"]!,
    passcode: numbers["passcode"]!,
    discriminator: numbers["discriminator"]!,
    vendorId: numbers["vendorId"]!,
    productId: numbers["productId"]!,
    ...(device === undefined ? {} : { device }),
    parts: device?.parts ?? [],
    bridged,
  };
}
