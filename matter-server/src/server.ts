#!/usr/bin/env node
/**
 * GIAP's Matter controller, spawned by pond-server (`server_setup.rs`):
 *   node --import tsx src/server.ts --port 5580 --storage-path <dir>
 * `--storage-path` is parsed here: matter.js's own argv parser reads it as a boolean.
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
 * Loopback only, not configurable: the socket is unauthenticated and holds the fabric's credentials,
 * so anything reaching it can drive or unpair every device. A shared controller is the operator's own.
 */
const BIND_HOST = "127.0.0.1";

/** Named for the protocol, so a stale address fails loudly rather than half-working. */
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

/** `--ble` opts in: BLE needs a native module and permissions a headless service may lack. */
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
 * Moves matter.js logging (stdout by default) to stderr, which the Rust side relays into `tracing`;
 * stdout must stay clean. Plain format, because the relay reads lines.
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
  // Before the ServerNode exists: matter.js resolves `Ble` from the environment at creation.
  const ble: BleStatus = parseBle(argv) ? await enableBle() : "off";
  const clients = new Set<WebSocket>();

  const broadcast = (name: EventName, payload: unknown): void => {
    const frame = JSON.stringify(event(name, payload));
    for (const client of clients) {
      if (client.readyState === client.OPEN) client.send(frame);
    }
  };

  // Matter's UDP operational port reuses the WebSocket's TCP port number: no collision, and it
  // moves with `--port`, keeping two Ponds on one host apart (and off the default 5540).
  const controller = await Controller.start(storagePath, port, {
    deviceAdded: (device: Device) => broadcast("device_added", { device }),
    deviceUpdated: (device: Device) => broadcast("device_updated", { device }),
    deviceRemoved: (deviceId: string) => broadcast("device_removed", { device_id: deviceId }),
    availabilityChanged: (deviceId: string, online: boolean) =>
      broadcast("device_availability", { device_id: deviceId, online }),
    reading: (reading: Reading) => broadcast("reading", reading),
  });

  // Logs over the socket too: a controller pond-server did not spawn has no stderr relay.
  onLog((record: LogRecord) => broadcast("log", record));

  const wss = new WebSocketServer({ host: BIND_HOST, port, path: PATH });

  wss.on("connection", (socket: WebSocket) => {
    clients.add(socket);
    const greeting: Greeting = {
      protocol: PROTOCOL_NAME,
      version: PROTOCOL_VERSION,
      fabric_id: controller.fabricId(),
      matter_js: matterJsVersion(),
      // What the transport IS, not what was asked for: `unavailable` is off.
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

  // SIGTERM and SIGINT must close the fabric cleanly, or devices keep sessions they won't reuse.
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
    // No id to reply against; logged, since a client sending garbage is a bug.
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
      // Full snapshot; events carry every change after, so re-check every peer is wired for them.
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
  // Informational only (for logs), so failing to resolve it is not fatal.
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
  // The Rust side reports the last stderr line as the reason a startup failed.
  log.error("startup_failed", describeError(error), { stack: stackOf(error) });
  process.exit(1);
});

function stackOf(error: unknown): string | undefined {
  return error instanceof Error && error.stack !== undefined
    ? redactSetupCode(error.stack)
    : undefined;
}
