// The dashboard, the desktop app and the CLI are on the devices list because they paired, and
// they check in only while someone has them open. Being closed is their normal state, not a fault.

import i18n from "../i18n";
import type { Device } from "../api/types";

/** Whether this device is a client that is only heard from while it is open. */
export function isOpenWhenUsed(device: Pick<Device, "device_type">): boolean {
  return device.device_type === "desktop" || device.device_type === "cli";
}

/** "Open" or "Not open" for the dashboard and the desktop app; null for every other type. */
export function openStateLabel(device: Pick<Device, "device_type" | "is_online">): string | null {
  if (device.device_type !== "desktop") return null;
  return i18n.t(device.is_online ? "devices.open" : "devices.notOpen");
}
