#!/usr/bin/env node
/**
 * GIAP's Matter controller — matter.js behind the `giap-matter` WebSocket protocol.
 *
 * Started by pond-server (`crates/pond-adapters-matter/src/server_setup.rs`), which
 * owns the process lifetime. See `docs/matter-protocol.md` for the wire contract.
 *
 *   node --import tsx src/server.ts --port 5580 --storage-path <dir>
 *
 * Both arguments are required in practice and parsed here. `--storage-path` is NOT
 * left to matter.js's own argv parser, which reads it as a boolean — see
 * `Controller.start`.
 */

import { readFileSync } from "node:fs";

import { LogFormat, Logger } from "@matter/main";
import { WebSocketServer, type WebSocket } from "ws";

import { enableBle, type BleStatus } from "./ble.js";
import { Controller } from "./controller.js";
import { log, describeError, onLog, redactSetupCode, type LogRecord } from "./log.js";
import { VERBS, type Verb } from "./mapping/control.js";
import {
  OpError,
  PROTOCOL_NAME,
  PROTOCOL_VERSION,
  event,
  failure,
  response,
  type Device,
  type EventName,
  type Greeting,
  type Reading,
  type Request,
} from "./protocol.js";

/**
 * Loopback only, and not configurable.
 *
 * A Matter controller holds the fabric's operational credentials: anything that can
 * reach this socket can drive and unpair every device in the house, with no
 * authentication of its own. GIAP only ever manages a controller on 127.0.0.1 (see
 * `local_port_from_ws_url`), so binding wider would create exposure nothing asked for.
 * An operator who wants a shared controller runs their own and points `matter_ws_url`
 * at it, which is a deliberate act rather than a default.
 */
const BIND_HOST = "127.0.0.1";

/** The protocol's path. Naming the protocol means an address left over from an
 *  earlier release fails loudly rather than half-working. */
const PATH = "/giap";

const DEFAULT_PORT = 5580;

/** `--name value`, or undefined when the flag is absent or has nothing after it. */
function flag(argv: string[], name: string): string | undefined {
  const index = argv.indexOf(name);
  if (index === -1 || index + 1 >= argv.length) return undefined;
  return argv[index + 1];
}

function parsePort(argv: string[]): number {
  const raw = flag(argv, "--port");
  if (raw === undefined) return DEFAULT_PORT;
  const port = Number(raw);
  if (!Number.isInteger(port) || port < 1 || port > 65535) {
    throw new Error(`--port must be a TCP port, got '${raw}'`);
  }
  return port;
}

/**
 * `--ble` asks for the Bluetooth transport. Absent means off.
 *
 * A flag rather than a default: BLE needs a native module that may not be
 * installed and permission a headless service does not have, and turning a radio
 * on is not something to do to someone's machine because a controller started.
 * See `ble.ts`.
 */
function parseBle(argv: string[]): boolean {
  return argv.includes("--ble");
}

function parseStoragePath(argv: string[]): string {
  const raw = flag(argv, "--storage-path");
  if (raw === undefined || raw.trim().length === 0) {
    throw new Error("--storage-path is required: the fabric has to be stored somewhere stable");
  }
  return raw;
}

/**
 * Put matter.js's own logging on stderr.
 *
 * It writes to stdout by default, and stdout is the one stream this process must keep
 * clean — the Rust side treats stderr as the diagnostic channel and relays it into
 * `tracing`, so matter.js's output would otherwise be the half of the story that never
 * reaches a GIAP log. Plain format because the relay reads lines, and ANSI escapes in
 * a log file help nobody.
 */
function routeMatterLogsToStderr(): void {
  Logger.format = LogFormat.PLAIN;
  for (const destination of Object.values(Logger.destinations)) {
    destination.write = (text: string) => {
      process.stderr.write(`${text}\n`);
    };
  }
}

async function main(): Promise<void> {
  const argv = process.argv.slice(2);
  const port = parsePort(argv);
  const storagePath = parseStoragePath(argv);
  routeMatterLogsToStderr();
  // Before the ServerNode exists: matter.js resolves `Ble` out of the environment
  // when the node is created, so registering it afterwards registers it for
  // nothing. See `ble.ts`.
  const ble: BleStatus = parseBle(argv) ? await enableBle() : "off";
  const clients = new Set<WebSocket>();

  const broadcast = (name: EventName, payload: unknown): void => {
    const frame = JSON.stringify(event(name, payload));
    for (const client of clients) {
      if (client.readyState === client.OPEN) client.send(frame);
    }
  };

  // The Matter operational port takes the same NUMBER as the WebSocket port.
  // They are different protocols (UDP and TCP) so they cannot collide, it needs
  // no extra configuration, and it moves with `--port` — so two Ponds on one
  // host stay out of each other's way as well as out of 5540's.
  const controller = await Controller.start(storagePath, port, {
    deviceAdded: (device: Device) => broadcast("device_added", { device }),
    deviceUpdated: (device: Device) => broadcast("device_updated", { device }),
    deviceRemoved: (deviceId: string) => broadcast("device_removed", { device_id: deviceId }),
    availabilityChanged: (deviceId: string, online: boolean) =>
      broadcast("device_availability", { device_id: deviceId, online }),
    reading: (reading: Reading) => broadcast("reading", reading),
  });

  // Also fan log records out over the socket. stderr already carries them, but the
  // relay on the other side only reads stderr while the process is a child it spawned
  // — an operator running their own controller has no such pipe, and this keeps their
  // GIAP log as informative as a managed one.
  onLog((record: LogRecord) => broadcast("log", record));

  const wss = new WebSocketServer({ host: BIND_HOST, port, path: PATH });

  wss.on("connection", (socket: WebSocket) => {
    clients.add(socket);
    const greeting: Greeting = {
      protocol: PROTOCOL_NAME,
      version: PROTOCOL_VERSION,
      fabric_id: controller.fabricId(),
      matter_js: matterJsVersion(),
      // What the transport actually IS, not what was asked for: `unavailable`
      // reads as off to a client, which is the truth about what it can pair.
      ble: ble === "on",
    };
    socket.send(JSON.stringify(greeting));
    log.info("client_connected", "a client attached to the controller", {
      clients: clients.size,
    });

    socket.on("message", (data: unknown) => {
      void handleMessage(controller, socket, String(data));
    });
    socket.on("close", () => {
      clients.delete(socket);
      log.info("client_disconnected", "a client detached", { clients: clients.size });
    });
    socket.on("error", error => {
      log.warn("client_socket_error", "client socket failed", { error: describeError(error) });
    });
  });

  wss.on("listening", () =>
    log.info("listening", "controller is accepting connections", { port, path: PATH }),
  );

  // SIGTERM is how pond-server stops us; SIGINT is a human at a terminal. Both must
  // close the fabric cleanly, because an abandoned subscription leaves every device
  // holding a session it will not reuse.
  const stop = async (signal: string): Promise<void> => {
    log.info("stopping", "shutting the controller down", { signal });
    wss.close();
    await controller.close();
    process.exit(0);
  };
  process.on("SIGTERM", () => void stop("SIGTERM"));
  process.on("SIGINT", () => void stop("SIGINT"));
}

async function handleMessage(
  controller: Controller,
  socket: WebSocket,
  raw: string,
): Promise<void> {
  let request: Request;
  try {
    request = JSON.parse(raw) as Request;
  } catch {
    // No id to answer against, so there is nothing to reply to. Logged rather than
    // dropped silently: a client sending garbage is a bug worth seeing.
    log.warn("unparseable_request", "a client sent something that is not JSON");
    return;
  }

  if (typeof request.id !== "string" || typeof request.op !== "string") {
    log.warn("malformed_request", "a request arrived without an id or an op");
    return;
  }

  const started = Date.now();
  try {
    const result = await dispatch(controller, request);
    socket.send(JSON.stringify(response(request.id, result)));
    log.debug("op_completed", "handled a request", {
      op: request.op,
      duration_ms: Date.now() - started,
    });
  } catch (error) {
    const wire =
      error instanceof OpError
        ? error.toWire()
        : { code: "internal" as const, message: describeError(error) };
    socket.send(JSON.stringify(failure(request.id, wire)));
    log.warn("op_failed", "a request failed", {
      op: request.op,
      error_code: wire.code,
      error: redactSetupCode(wire.message),
      duration_ms: Date.now() - started,
    });
  }
}

async function dispatch(controller: Controller, request: Request): Promise<unknown> {
  const params = request.params ?? {};

  switch (request.op) {
    case "ping":
      return {};

    case "subscribe":
      // The full snapshot, so a fresh connection knows the fabric without waiting for
      // something to change. Events carry every change from here on — which is why
      // this also re-checks that every commissioned peer is wired for them.
      controller.observeCommissioned();
      return { devices: controller.devices(), readings: controller.readings() };

    case "discover":
      return { commissionable: await controller.discover() };

    case "commission": {
      const code = params.code;
      if (typeof code !== "string" || code.trim().length === 0) {
        throw new OpError("invalid_setup_code", "no setup code was given");
      }
      const name = typeof params.name === "string" ? params.name : undefined;
      return { device: await controller.commission(code, name) };
    }

    case "decommission": {
      await controller.decommission(requireDeviceId(params.device_id));
      return {};
    }

    case "control": {
      const deviceId = requireDeviceId(params.device_id);
      const verb = params.verb;
      if (typeof verb !== "string" || !VERBS.has(verb)) {
        throw new OpError("bad_request", `'${String(verb)}' is not a control verb`);
      }
      return { applied: await controller.control(deviceId, verb as Verb, params.value) };
    }

    case "describe": {
      const deviceId = requireDeviceId(params.device_id);
      return { description: controller.describe(deviceId) };
    }

    case "state": {
      const deviceId = requireDeviceId(params.device_id);
      return { state: controller.state(deviceId) };
    }

    default:
      throw new OpError("bad_request", `'${String(request.op)}' is not an op`);
  }
}

function requireDeviceId(value: unknown): string {
  if (typeof value !== "string" || value.length === 0) {
    throw new OpError("bad_request", "device_id is required");
  }
  return value;
}

function matterJsVersion(): string {
  // Reported in the greeting purely so an operator reading a log can tell which
  // controller answered. Never load-bearing, so a failure to resolve it is not fatal.
  try {
    const manifest = readFileSync(
      new URL("../node_modules/@matter/main/package.json", import.meta.url),
      "utf8",
    );
    return String((JSON.parse(manifest) as { version?: unknown }).version ?? "unknown");
  } catch {
    return "unknown";
  }
}

main().catch((error: unknown) => {
  // The last line the Rust side's stderr ring will hold, and therefore the reason it
  // reports when the controller dies before it is ready.
  log.error("startup_failed", describeError(error), { stack: stackOf(error) });
  process.exit(1);
});

function stackOf(error: unknown): string | undefined {
  return error instanceof Error && error.stack !== undefined
    ? redactSetupCode(error.stack)
    : undefined;
}
