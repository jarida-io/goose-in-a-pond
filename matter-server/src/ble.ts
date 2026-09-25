/**
 * BLE, to commission devices not yet on a network. Off by default (needs `cap_net_raw` or a macOS
 * prompt); imported lazily since the optional native noble may not build, and then importing throws.
 */

import { Environment } from "@matter/main";

import { log, describeError } from "./log.js";

/** What happened when BLE was asked for, as the greeting reports it. */
export type BleStatus =
  /** Not asked for. */
  | "off"
  /** Asked for, loaded, and registered with matter.js. */
  | "on"
  /** Asked for and unavailable — the package or its radio is not installed. */
  | "unavailable";

/** Registers BLE in matter.js's environment; call before creating the `ServerNode`. Never throws. */
export async function enableBle(): Promise<BleStatus> {
  try {
    // `Ble` comes from `@matter/protocol`: `@matter/main` doesn't re-export it.
    const { Ble } = await import("@matter/protocol");
    const { NodeJsBle } = await import("@matter/nodejs-ble");

    Environment.default.set(Ble, new NodeJsBle());
      log.info("ble_enabled", "the BLE transport is registered; devices can pair over Bluetooth");
    return "on";
  } catch (error) {
    // Warn, not error: the controller still pairs over IP.
    log.warn(
      "ble_unavailable",
      "BLE was asked for and could not be loaded; pairing over IP only",
      { error: describeError(error) },
    );
    return "unavailable";
  }
}
