import { useCallback, useEffect, useMemo, useRef, useState, type ReactElement } from "react";
import { api } from "../../../api/PondApiClient";
import { powerStateOf } from "../../../sections/devicePower";
import { timeSince } from "../../../sections/timeSince";
import type { DeviceData } from "../../data/mockHome";
import { HubIco } from "../../primitives/HubIco";
import { HP_PATHS } from "../../primitives/icons";
import { useHomeData } from "../../state/hubDataStore";
import "./home-controls-card.css";

const DEVICE_SERVER = "giap-device-control";

/** What a device says about its switch: on, off, or (undefined) it did not say. */
type PowerRead = boolean | undefined;

/** Everything this card needs per device from `/api/v1/devices` besides the name/room/kind
    already in `home.devices` -- capabilities for the power-tile split, plus connectivity,
    which used to need its own fetch (HomeAlerts.tsx's) when this card only kept capabilities. */
interface WireDevice {
  capabilities: string[];
  is_online: boolean;
  last_seen?: string;
}

/** The device-list read. Loading and failed are distinct: one says wait, the other offers a retry. */
type Wire =
  | { status: "loading" }
  | { status: "failed" }
  | { status: "ready"; byId: Record<string, WireDevice> };

/**
 * One device's power; undefined (never `false`) on a throw or an unparseable reply. The body
 * check matters: `request<T>` returns `undefined` or `index.html` for empty or misrouted replies.
 */
async function readPower(id: string): Promise<PowerRead> {
  try {
    const result = await api.invokeTool({
      server: DEVICE_SERVER,
      tool: "get_device_state",
      args: { device_id: id },
    });
    return typeof result?.content === "string" ? powerStateOf(result.content) : undefined;
  } catch {
    return undefined;
  }
}

export interface HomeControlsCardProps {
  /** Max tiles; the integrator passes 2, 4 or 6 by widget size. */
  limit: number;
  /** Where an empty house is sent to add its first device. */
  onManageDevices: () => void;
}

/** Device tiles showing only what `get_device_state` reports; not hubStore's mock-seeded `useDeviceState`. */
export function HomeControlsCard({ limit, onManageDevices }: HomeControlsCardProps): ReactElement {
  const home = useHomeData();

  // Capabilities by device id, off the wire: the only source for whether a device has `power`,
  // and whether it exists at all (hubDataStore fills an empty pond with the mock house).
  const [wire, setWire] = useState<Wire>({ status: "loading" });
  const [attempt, setAttempt] = useState(0);
  const [reads, setReads] = useState<Record<string, PowerRead>>({});
  const [pending, setPending] = useState<ReadonlySet<string>>(() => new Set());

  // Re-armed on mount: StrictMode's double-invoke would otherwise leave the first cleanup's `false`.
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  // Keyed on `attempt` so "Try again" re-runs it; a launch read often fails before the sidecar serves.
  useEffect(() => {
    let cancelled = false;
    api
      .listDevices()
      .then((list) => {
        if (cancelled) return;
        const byId: Record<string, WireDevice> = {};
        for (const d of list) {
          byId[d.id] = { capabilities: d.capabilities ?? [], is_online: d.is_online, last_seen: d.last_seen };
        }
        setWire({ status: "ready", byId });
      })
      .catch(() => {
        // `failed`, never an empty map: not knowing the house is not an empty house.
        if (!cancelled) setWire({ status: "failed" });
      });
    return () => {
      cancelled = true;
    };
  }, [attempt]);

  const room = Math.max(0, Math.floor(limit));

  // Needs both reads: until `devicesAreReal`, `home.devices` is the mock house, whose ids never
  // match a real one, so the empty state would wrongly call the house empty.
  const settled = wire.status === "ready" && home.devicesAreReal;

  // Store order, intersected with what the backend actually knows.
  const known = useMemo(
    () => (wire.status === "ready" && home.devicesAreReal ? home.devices.filter((d) => d.id in wire.byId) : []),
    [home.devices, home.devicesAreReal, wire],
  );
  const visible = useMemo(() => known.slice(0, room), [known, room]);

  const canPower = useCallback(
    (id: string): boolean =>
      wire.status === "ready" ? (wire.byId[id]?.capabilities.includes("power") ?? false) : false,
    [wire],
  );

  /** Undefined only while `wire` hasn't resolved; `known` is already gated on `wire.status === "ready"`. */
  const connectivityOf = (id: string): WireDevice | undefined =>
    wire.status === "ready" ? wire.byId[id] : undefined;

  // Serialised so the read effect re-runs on changed ids, not on each new-but-equal array.
  const powerKey = useMemo(
    () => JSON.stringify(visible.filter((d) => canPower(d.id)).map((d) => d.id)),
    [visible, canPower],
  );

  useEffect(() => {
    const ids = JSON.parse(powerKey) as string[];
    if (ids.length === 0) return;
    void (async () => {
      const settled = await Promise.allSettled(ids.map((id) => readPower(id)));
      if (!alive.current) return;
      setReads((prev) => {
        const next = { ...prev };
        settled.forEach((r, i) => {
          next[ids[i]] = r.status === "fulfilled" ? r.value : undefined;
        });
        return next;
      });
    })();
  }, [powerKey]);

  function markPending(id: string, busy: boolean) {
    setPending((prev) => {
      const next = new Set(prev);
      if (busy) next.add(id);
      else next.delete(id);
      return next;
    });
  }

  /** Sends the switch, then re-reads: a dispatch that returns says the tool ran, not that the lamp moved. */
  async function toggle(id: string, next: boolean) {
    markPending(id, true);
    try {
      await api.invokeTool({
        server: DEVICE_SERVER,
        tool: "set_device_state",
        args: { device_id: id, power: next },
      });
    } catch {
      // Swallowed: the re-read below decides the label.
    }
    const state = await readPower(id);
    if (!alive.current) return;
    setReads((prev) => ({ ...prev, [id]: state }));
    markPending(id, false);
  }

  /** Hand the device to the existing control sheet, which is addressed by id. */
  function openControls(device: DeviceData) {
    window.dispatchEvent(new CustomEvent("hub:device", { detail: device.id }));
  }

  /** Dot plus text, not colour alone: offline tiles carry both (plan §6). */
  function connectivity(id: string): ReactElement | null {
    const info = connectivityOf(id);
    if (!info) return null;
    return info.is_online ? (
      <span className="hcc__status" data-online="true">
        <span className="hcc__dot" aria-hidden="true" />
        Online
      </span>
    ) : (
      <span className="hcc__status" data-online="false">
        <span className="hcc__dot" aria-hidden="true" />
        Offline · {info.last_seen ? timeSince(info.last_seen) : "unknown"}
      </span>
    );
  }

  const head = (showManage: boolean) => (
    <div className="hcc__head">
      <HubIco d={HP_PATHS.sliders} size={18} color="var(--color-text)" sw={1.9} />
      <span className="hcc__title">Devices</span>
      {showManage && (
        <button type="button" className="hcc__manage" onClick={onManageDevices}>
          Manage devices
        </button>
      )}
    </div>
  );

  if (wire.status === "failed") {
    return (
      <div className="hcc" data-hook="home-controls" data-state="failed">
        {head(false)}
        <div className="hcc__note" role="status">
          <HubIco d={HP_PATHS.alert} size={16} color="var(--color-text-secondary)" sw={1.9} />
          <span className="hcc__note-title">Could not reach the pond</span>
          <span className="hcc__note-sub">
            The device list did not answer, so this card does not know what the house holds.
          </span>
          {/* Back to `loading` as well as bumping the attempt, so the press has an
              answer straight away. Leaving the failure on screen for the 30s the
              request is allowed would read as a button that did nothing. */}
          <button
            className="hcc__retry"
            type="button"
            onClick={() => {
              setWire({ status: "loading" });
              setAttempt((n) => n + 1);
            }}
          >
            Try again
          </button>
        </div>
      </div>
    );
  }

  if (!settled) {
    return (
      <div className="hcc" data-hook="home-controls" data-state="loading">
        {head(false)}
        <span className="hcc__quiet" role="status">
          Checking what the house holds
        </span>
      </div>
    );
  }

  if (known.length === 0) {
    return (
      <div className="hcc" data-hook="home-controls" data-state="empty">
        <button className="hcc__empty" type="button" onClick={onManageDevices}>
          <HubIco d={HP_PATHS.plus} size={16} color="var(--pp)" sw={2} />
          Add your first device
        </button>
      </div>
    );
  }

  return (
    <div className="hcc" data-hook="home-controls" data-state="ready">
      {head(true)}

      {visible.length > 0 && (
        <div className="hcc__grid">
          {visible.map((device) => {
            const online = connectivityOf(device.id)?.is_online ?? true;

            if (!canPower(device.id)) {
              return (
                <div className="hcc__tile hcc__static" key={device.id} data-online={String(online)}>
                  <span className="hcc__name">{device.name}</span>
                  <span className="hcc__room">{device.room}</span>
                  {connectivity(device.id)}
                </div>
              );
            }

            const read = reads[device.id];
            // The last read, shown even while a write is in flight.
            const last = read === true ? "on" : read === false ? "off" : "unknown";
            const busy = pending.has(device.id);

            if (last === "unknown") {
              return (
                <button
                  className="hcc__tile"
                  key={device.id}
                  type="button"
                  data-state={busy ? "pending" : "unknown"}
                  data-last="unknown"
                  data-online={String(online)}
                  aria-busy={busy}
                  aria-label={`${device.name}, not reporting — open controls`}
                  onClick={() => openControls(device)}
                >
                  <span className="hcc__name">{device.name}</span>
                  <span className="hcc__value">Not reporting</span>
                  {connectivity(device.id)}
                </button>
              );
            }

            return (
              <button
                className="hcc__tile"
                key={device.id}
                type="button"
                data-state={busy ? "pending" : last}
                data-last={last}
                data-online={String(online)}
                aria-busy={busy}
                aria-pressed={last === "on"}
                aria-label={`${device.name}, ${last === "on" ? "on" : "off"}`}
                onClick={() => {
                  if (busy) return;
                  void toggle(device.id, last !== "on");
                }}
              >
                <span className="hcc__name">{device.name}</span>
                <span className="hcc__value">{last === "on" ? "On" : "Off"}</span>
                {connectivity(device.id)}
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
