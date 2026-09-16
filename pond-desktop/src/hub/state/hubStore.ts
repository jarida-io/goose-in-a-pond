import { useSyncExternalStore } from "react";
import { HOME } from "../data/mockHome";
import { api } from "../../api/PondApiClient";

// ─── Device state shape ────────────────────────────────────────

export interface DeviceState {
  on: boolean;
  locked: boolean;
  target: number;
  cur: number;
  brightness: number;
  temp: string;
  watts: number;
  mode: string;
}

export type DevicePatch = Partial<DeviceState>;

// ─── Store implementation (mirrors HUBSTORE from design) ───────

type Subscriber = () => void;

interface HubStoreInternals {
  data: Record<string, DeviceState>;
  /** Snapshot is a new object reference each mutation so useSyncExternalStore detects the change */
  snapshot: Record<string, DeviceState>;
  subs: Set<Subscriber>;
}

const store: HubStoreInternals = {
  data: {},
  snapshot: {},
  subs: new Set(),
};

// Initialise from mock data — also set snapshot to the same initial object
HOME.devices.forEach((d) => {
  store.data[d.id] = {
    on:         d.on ?? false,
    locked:     d.locked ?? true,
    target:     d.target ?? 70,
    cur:        d.value ?? 70,
    brightness: d.kind === "light" ? (d.on ? 80 : 40) : 0,
    temp:       "warm",
    watts:      d.kind === "plug" ? 42 : 0,
    mode:       "Heat",
  };
});
// Initial snapshot is a shallow copy of data
store.snapshot = { ...store.data };

function getDevice(id: string): DeviceState {
  return store.data[id] ?? {
    on: false, locked: true, target: 70, cur: 70,
    brightness: 40, temp: "warm", watts: 0, mode: "Heat",
  };
}

function setDevice(id: string, patch: DevicePatch): void {
  store.data[id] = { ...getDevice(id), ...patch };
  // Shallow-copy data so useSyncExternalStore detects a new snapshot reference
  store.snapshot = { ...store.data };
  store.subs.forEach((f) => f());
}

function subscribe(f: Subscriber): () => void {
  store.subs.add(f);
  return () => store.subs.delete(f);
}

// Returns a new object reference on every mutation so useSyncExternalStore
// detects the change via Object.is comparison.
function getSnapshot(): Record<string, DeviceState> {
  return store.snapshot;
}

// ─── Backend actuation ─────────────────────────────────────────

/**
 * Actuate a device through the backend device-control MCP tool, bypassing the
 * LLM via `POST /api/v1/tools/invoke`. Optimistic: the patch is applied locally
 * immediately, then reverted if the call fails. Patches with no actuatable field
 * (e.g. color-temp or mode only) stay local and skip the round-trip.
 * Returns true on success (or local-only), false if the backend call failed.
 */
export async function controlDevice(id: string, patch: DevicePatch): Promise<boolean> {
  const prev = getDevice(id);
  setDevice(id, patch); // optimistic

  const args: Record<string, unknown> = { device_id: id };
  if (patch.on !== undefined) args.power = patch.on;
  if (patch.brightness !== undefined) args.brightness = patch.brightness;
  if (patch.target !== undefined) args.target_temp = patch.target;
  if (patch.locked !== undefined) args.locked = patch.locked;

  // Only device_id present → nothing the backend can actuate; keep it local.
  if (Object.keys(args).length === 1) return true;

  try {
    await api.invokeTool({
      server: "giap-device-control",
      tool: "set_device_state",
      args,
    });
    return true;
  } catch {
    setDevice(id, prev); // revert on failure
    return false;
  }
}

// ─── React hook ────────────────────────────────────────────────

/**
 * Returns [deviceState, setDeviceState] for a given device id.
 * Reactive: any call to setDeviceState re-renders all subscribers.
 */
export function useDeviceState(
  id: string,
): [DeviceState, (patch: DevicePatch) => void, (patch: DevicePatch) => Promise<boolean>] {
  const snapshot = useSyncExternalStore(subscribe, getSnapshot);
  const state = snapshot[id] ?? getDevice(id);
  const set = (patch: DevicePatch) => setDevice(id, patch);
  const control = (patch: DevicePatch) => controlDevice(id, patch);
  return [state, set, control];
}

// Expose raw setDevice + backend actuation for non-React contexts — event
// handlers that toggle several devices at once and are not inside a component.
export {
  setDevice as hubSetDevice,
  getDevice as hubGetDevice,
  controlDevice as hubControlDevice,
};
