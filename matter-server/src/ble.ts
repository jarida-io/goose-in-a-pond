/**
 * Bluetooth Low Energy, for pairing a device that has never been on the network.
 *
 * Why this exists at all. A Matter device out of its box has no Wi-Fi credentials,
 * so it cannot advertise on mDNS and IP commissioning cannot see it: the first
 * conversation has to happen over BLE, and the commissioner hands the network
 * credentials across during it. Without BLE, GIAP could only ever pair a device
 * something else had already onboarded — which is most of a Matter integration
 * missing, and it looked like "no device found in pairing mode".
 *
 * Why it is off by default, and dynamically imported.
 *
 * The radio comes from `@stoprocent/noble`, a native module, itself an OPTIONAL
 * dependency of `@matter/nodejs-ble` — which is an optional dependency here. So
 * `npm ci --omit=dev` on a Jetson with no build toolchain installs neither, and
 * that must degrade to IP-only rather than failing the install or, worse, taking
 * the whole controller down at import time. A static `import` would do exactly
 * that: the package installs fine when noble does not build, and then IMPORTING
 * it throws.
 *
 * It also needs permission a headless service does not have by default —
 * `cap_net_raw` on Linux, a Bluetooth usage prompt on macOS — and turning a radio
 * on is not something to do to someone's machine because a controller started.
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

/**
 * Register a BLE transport with matter.js's environment, if one can be had.
 *
 * matter.js resolves `Ble` out of the environment on its own: `ControllerBehavior`
 * adds `Ble.scanner` to its scanner set and `Ble.centralInterface` to its
 * transports, with its own try/catch that disables BLE on an init failure. So the
 * whole of the wiring is putting the instance where it will look, before the
 * `ServerNode` is created.
 *
 * Never throws. An unavailable radio is a controller that pairs over IP, which is
 * what it did before this existed — and a failure here that took the process down
 * would turn an optional transport into a hard dependency.
 */
export async function enableBle(): Promise<BleStatus> {
  try {
    // Both imports inside the try, and both dynamic. `Ble` lives in
    // `@matter/protocol`, which `@matter/main` does not re-export -- the same
    // reason `@matter/types` is a declared dependency of its own.
    const { Ble } = await import("@matter/protocol");
    const { NodeJsBle } = await import("@matter/nodejs-ble");

    Environment.default.set(Ble, new NodeJsBle());
      log.info("ble_enabled", "the BLE transport is registered; devices can pair over Bluetooth");
    return "on";
  } catch (error) {
    // A warning, not an error: the controller works, with less reach. Said out
    // loud either way, because a user whose new device will not pair needs to be
    // able to find out that the transport they asked for is not there.
    log.warn(
      "ble_unavailable",
      "BLE was asked for and could not be loaded; pairing over IP only",
      { error: describeError(error) },
    );
    return "unavailable";
  }
}
