/**
 * A Matter device for testing GIAP against (bridges, device types nobody owns). Prefer MVD
 * where it can express the device: Google's stack is the independent check.
 *
 *   node --import tsx tools/virtual-device.ts --device dimmable-light
 *
 * Paste the printed code into GIAP's Register-device dialog; stdin takes
 * `set kitchen.onOff.onOff = true`, `list` and `quit`.
 */

import {
  DeviceTypeId,
  Environment,
  ServerNode,
  VendorId,
  type Endpoint,
  type EndpointType,
} from "@matter/main";
import { AggregatorEndpoint } from "@matter/main/endpoints/aggregator";
import { BridgedDeviceBasicInformationServer } from "@matter/main/behaviors/bridged-device-basic-information";

import { parseArgs, type Spec, type DeviceSpec } from "./virtual-device-args.js";

/** Matter's Bridged Node device type. A child claiming this IS a separate device. */
const BRIDGED_NODE_DEVICE_TYPE = 19;

/**
 * A device type by module name (`dimmable-light` -> `DimmableLightDevice`). Imports from
 * `@matter/main/...`: only those install the Node.js platform, and `@matter/node` is transitive.
 */
async function deviceTypeNamed(name: string): Promise<DeviceType> {
  const exportName = `${name.replace(/(^|-)([a-z])/g, (_, __, c: string) => c.toUpperCase())}Device`;
  let module: Record<string, unknown>;
  try {
    module = (await import(`@matter/main/devices/${name}`)) as Record<string, unknown>;
  } catch {
    throw new Error(
      `no such device type '${name}'. The name is the matter.js module name, ` +
        `kebab-cased: dimmable-light, room-air-conditioner, temperature-controlled-cabinet.`,
    );
  }
  const type = module[exportName];
  if (!isDeviceType(type)) {
    throw new Error(`'${name}' resolved to no ${exportName} export`);
  }
  return type;
}

/** The members this tool uses; the dynamic import erases matter.js's precise device type. */
type DeviceType = EndpointType & {
  deviceRevision: number;
  with(...behaviors: unknown[]): DeviceType;
};

function isDeviceType(value: unknown): value is DeviceType {
  return (
    typeof value === "object" &&
    value !== null &&
    typeof (value as { deviceType?: unknown }).deviceType === "number" &&
    "behaviors" in value
  );
}

/**
 * Add one bridged child. It needs `BridgedDeviceBasicInformationServer` (its own name and
 * reachability) and a hand-seeded `deviceTypeList`: `DescriptorServer` would list only the
 * device's own type, and a controller spots a bridged child by Bridged Node (19).
 */
async function addBridgedChild(
  aggregator: Endpoint,
  spec: DeviceSpec,
): Promise<Endpoint> {
  const type = await deviceTypeNamed(spec.device);
  const bridged = type.with(BridgedDeviceBasicInformationServer);

  return aggregator.add(bridged, {
    id: spec.id,
    ...(spec.number === undefined ? {} : { number: spec.number }),
    ...spec.state,
    bridgedDeviceBasicInformation: {
      nodeLabel: spec.label ?? spec.id,
      productName: spec.device,
      vendorName: "GIAP virtual device",
      reachable: true,
    },
    descriptor: {
      deviceTypeList: [
        { deviceType: DeviceTypeId(BRIDGED_NODE_DEVICE_TYPE), revision: 3 },
        { deviceType: type.deviceType, revision: type.deviceRevision },
      ],
    },
  });
}

/** Add a plain child endpoint — a part of a composed device, not a bridged one. */
async function addPart(parent: Endpoint, spec: DeviceSpec): Promise<Endpoint> {
  const type = await deviceTypeNamed(spec.device);
  return parent.add(type, {
    id: spec.id,
    ...(spec.number === undefined ? {} : { number: spec.number }),
    ...spec.state,
  });
}

async function build(spec: Spec): Promise<{ node: ServerNode; endpoints: Map<string, Endpoint> }> {
  Environment.default.vars.set("storage.path", spec.storage);

  const primary = spec.device === undefined ? undefined : await deviceTypeNamed(spec.device.device);

  const node = await ServerNode.create({
    // The node id names its storage directory, so distinct ids isolate concurrent instances.
    id: spec.id,
    // Not 5540: whoever holds it stops every other Matter device here starting (docs/matter.md).
    network: { port: spec.port },
    commissioning: { passcode: spec.passcode, discriminator: spec.discriminator },
    productDescription: {
      name: spec.name,
      deviceType: (primary ?? AggregatorEndpoint).deviceType,
    },
    basicInformation: {
      vendorId: VendorId(spec.vendorId),
      vendorName: "GIAP virtual device",
      productId: spec.productId,
      productName: spec.name,
    },
  });

  const endpoints = new Map<string, Endpoint>();

  if (primary !== undefined && spec.device !== undefined) {
    const own = await node.add(primary, {
      id: spec.device.id,
      ...(spec.device.number === undefined ? {} : { number: spec.device.number }),
      ...spec.device.state,
    });
    endpoints.set(spec.device.id, own);
    for (const part of spec.parts) {
      endpoints.set(part.id, await addPart(own, part));
    }
  }

  if (spec.bridged.length > 0) {
    const aggregator = await node.add(AggregatorEndpoint, { id: "aggregator" });
    endpoints.set("aggregator", aggregator);
    for (const child of spec.bridged) {
      const endpoint = await addBridgedChild(aggregator, child);
      endpoints.set(child.id, endpoint);
      // A part of a bridged child, so a composed device behind a hub is expressible.
      for (const part of child.parts) {
        endpoints.set(part.id, await addPart(endpoint, part));
      }
    }
  }

  await node.start();
  return { node, endpoints };
}

/**
 * Apply `set <endpoint>.<behavior>.<attribute> = <value>` in one transaction (no torn
 * reads). The value is JSON if it parses, else a bare string.
 */
async function applySet(endpoints: Map<string, Endpoint>, line: string): Promise<string> {
  const match = /^set\s+([\w-]+)\.(\w+)\.(\w+)\s*=\s*(.+)$/.exec(line);
  if (match === null) {
    return "usage: set <endpoint>.<behavior>.<attribute> = <value>";
  }
  const [, endpointId, behavior, attribute, raw] = match;
  const endpoint = endpoints.get(endpointId!);
  if (endpoint === undefined) {
    return `no endpoint '${endpointId}'. Known: ${[...endpoints.keys()].join(", ")}`;
  }
  let value: unknown;
  try {
    value = JSON.parse(raw!);
  } catch {
    value = raw;
  }
  try {
    await endpoint.set({ [behavior!]: { [attribute!]: value } });
    return `${endpointId}.${behavior}.${attribute} = ${JSON.stringify(value)}`;
  } catch (error) {
    return `refused: ${error instanceof Error ? error.message : String(error)}`;
  }
}

/** The innermost cause's message: matter.js nests the useful conformance error three deep. */
function deepestCause(error: unknown): string {
  let current = error;
  const seen = new Set<unknown>();
  while (current instanceof Error && current.cause !== undefined && !seen.has(current)) {
    seen.add(current);
    current = current.cause;
  }
  if (current instanceof AggregateError && current.errors.length > 0) {
    return deepestCause(current.errors[0]);
  }
  return current instanceof Error ? current.message : String(current);
}

async function main(): Promise<void> {
  let spec: Spec;
  try {
    spec = parseArgs(process.argv.slice(2));
  } catch (error) {
    // A wrong flag is a typo, not a crash: print the message, not a matter.js FATAL.
    console.error(`\n  ${error instanceof Error ? error.message : String(error)}\n`);
    process.exit(2);
  }

  let node: ServerNode;
  let endpoints: Map<string, Endpoint>;
  try {
    ({ node, endpoints } = await build(spec));
  } catch (error) {
    const reason = deepestCause(error);
    console.error(`\n  could not build the device: ${reason}\n`);
    if (/requires you to set this attribute|not within bounds defined by constraint/i.test(reason)) {
      // No mandatory-defaults table: matter.js names the attribute, and a table would drift.
      console.error("  Matter constrains that attribute on this device type, and matter.js");
      console.error("  enforces it device-side. Give it a value with --attr, e.g.");
      console.error("    --attr front.doorLock.lockType=0 --attr front.doorLock.wrongCodeEntryLimit=5\n");
    }
    process.exit(1);
  }

  const { manualPairingCode, qrPairingCode } = node.state.commissioning.pairingCodes;
  console.log("");
  console.log(`  ${spec.name} is up on Matter port ${spec.port}.`);
  console.log("");
  console.log(`  manual pairing code   ${manualPairingCode}`);
  console.log(`  QR payload            ${qrPairingCode}`);
  console.log("");
  console.log(`  endpoints             ${[...endpoints.keys()].join(", ") || "(none)"}`);
  console.log("");
  console.log("  Commands: set <endpoint>.<behavior>.<attribute> = <value> | list | quit");
  console.log("");

  // `close()`, never `stop()`: only close releases the storage directory lock.
  let closing = false;
  const shutdown: () => Promise<never> = async () => {
    if (!closing) {
      closing = true;
      await node.close();
    }
    process.exit(0);
  };
  process.once("SIGINT", () => void shutdown());
  process.once("SIGTERM", () => void shutdown());

  // Never exit on stdin EOF: backgrounding or piping closes stdin immediately.
  void readCommands(endpoints, shutdown);
  await new Promise<never>(() => {});
}

async function readCommands(
  endpoints: Map<string, Endpoint>,
  shutdown: () => Promise<never>,
): Promise<void> {
  process.stdin.setEncoding("utf8");
  for await (const chunk of process.stdin) {
    const lines = String(chunk)
      .split("\n")
      .map(line => line.trim())
      .filter(line => line.length > 0);
    for (const line of lines) {
      if (line === "quit" || line === "exit") {
        await shutdown();
      } else if (line === "list") {
        for (const [id, endpoint] of endpoints) {
          console.log(`  ${id}  endpoint ${endpoint.number}  ${endpoint.type.name}`);
        }
      } else {
        console.log(`  ${await applySet(endpoints, line)}`);
      }
    }
  }
}

await main();
