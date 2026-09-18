import { useEffect, useMemo, useRef, useState, type ReactElement } from "react";
import { api } from "../../../api/PondApiClient";
import { powerStateOf } from "../../../sections/Devices";
import type { DeviceData } from "../../data/mockHome";
import { HubIco } from "../../primitives/HubIco";
import { HP_PATHS } from "../../primitives/icons";
import { useHomeData } from "../../state/hubDataStore";
import "./home-controls-card.css";

/**
 * The design's 2x2 device tiles, reporting only what a device actually said.
 *
 * `GET /api/v1/devices` sends identity and capabilities and no metadata at all, so
 * the mockup's "On · 80%", "Heat to 70°F" and "Locked" have no source anywhere in
 * the pond. The one honest read is the `get_device_state` MCP tool, which answers
 * with text lines including `power: on|off` — so this card says `On`, `Off`, or
 * that the device did not say, and nothing else.
 *
 * It deliberately does not use `useDeviceState`/`controlDevice` from hubStore:
 * that map is seeded from the mock house and answers a real device id with a
 * hardcoded on/locked/target/brightness, which is the single largest fabrication
 * on today's Home.
 */

const DEVICE_SERVER = "giap-device-control";

/** What a device says about its switch: on, off, or (undefined) it did not say. */
type PowerRead = boolean | undefined;

/**
 * What this card knows about the pond's device list.
 *
 * Three answers, not two. A single nullable map collapsed "has not answered yet"
 * and "the read failed" into one value, and since neither can enter the empty
 * state nor the grid, both rendered the same thing: a full-height card holding an
 * icon, the word "Devices", and nothing else — permanently, because the read is
 * fired once on mount and nothing retries it. They are different sentences to a
 * household. One says wait; the other says the pond was not reachable, and offers
 * the read again.
 */
type Wire =
  | { status: "loading" }
  | { status: "failed" }
  | { status: "ready"; byId: Record<string, string[]> };

/**
 * Ask one device what it is.
 *
 * A throw, a body that is not text, and a reply this cannot parse are all the same
 * answer: it did not say. Never coerced to `false` — labelling a tile "Off" because
 * a read failed is the mock store's mistake with a different wrong input. The body
 * check is not paranoia: `request<T>` hands back `undefined` or `index.html` for an
 * empty or misrouted response, and both would reach `powerStateOf` as a non-string.
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
  /** How many tiles to show before the card would become a list. The integrator passes 2 | 4 | 6 by widget size. */
  limit: number;
  /** Where an empty house is sent to add its first device. */
  onManageDevices: () => void;
}

export function HomeControlsCard({ limit, onManageDevices }: HomeControlsCardProps): ReactElement {
  const home = useHomeData();

  // Capabilities by device id, straight off the wire. Two facts come from here and
  // nowhere else: whether a device declares `power` — a contact sensor's list is
  // empty, and that is what stops it being offered a switch — and whether the id
  // exists on the backend at all. hubDataStore substitutes the mock house when the
  // pond has no devices, and a demo tile is the exact thing the empty state is
  // here to replace.
  const [wire, setWire] = useState<Wire>({ status: "loading" });
  const [attempt, setAttempt] = useState(0);
  const [reads, setReads] = useState<Record<string, PowerRead>>({});
  const [pending, setPending] = useState<ReadonlySet<string>>(() => new Set());

  // Re-armed on mount because a StrictMode double-invoke would otherwise leave the
  // first cleanup's `false` standing for the life of the component.
  const alive = useRef(true);
  useEffect(() => {
    alive.current = true;
    return () => {
      alive.current = false;
    };
  }, []);

  // Keyed on `attempt` so "Try again" re-runs it. The old `[]` deps meant a read
  // that failed at launch — routine, the shell paints before the sidecar serves —
  // stayed failed for the life of the mount, since none of the store's refresh
  // paths re-render their way into this effect.
  useEffect(() => {
    let cancelled = false;
    api
      .listDevices()
      .then((list) => {
        if (cancelled) return;
        const byId: Record<string, string[]> = {};
        for (const d of list) byId[d.id] = d.capabilities ?? [];
        setWire({ status: "ready", byId });
      })
      .catch(() => {
        // `failed`, never an empty map. A failed read means the card does not know
        // what the house holds, which is a different sentence from "you have no
        // devices" — and only one of the two is true.
        if (!cancelled) setWire({ status: "failed" });
      });
    return () => {
      cancelled = true;
    };
  }, [attempt]);

  const room = Math.max(0, Math.floor(limit));

  // Both halves have to have answered before this card can say anything about the
  // house: its own capability read, AND the store's device list. Until
  // `devicesAreReal` flips, `home.devices` is the demo house from mockHome, whose
  // ids are strings like "driveway" while a real one is a UUID — so the
  // intersection below is empty by construction, and the empty state would call a
  // pond with a paired lamp a house with nothing in it. Same guard, same reason, as
  // DashboardGrid's `home.devicesAreReal ? home.devices : []`.
  const settled = wire.status === "ready" && home.devicesAreReal;

  // Store order, intersected with what the backend actually knows.
  const known = useMemo(
    () => (wire.status === "ready" && home.devicesAreReal ? home.devices.filter((d) => d.id in wire.byId) : []),
    [home.devices, home.devicesAreReal, wire],
  );
  const visible = useMemo(() => known.slice(0, room), [known, room]);

  const canPower = (id: string): boolean =>
    wire.status === "ready" ? (wire.byId[id]?.includes("power") ?? false) : false;

  // Serialised rather than passed as an array, so the read effect re-runs when the
  // visible ids change and not when a render hands it an equal-but-new array.
  const powerKey = useMemo(
    () => JSON.stringify(visible.filter((d) => canPower(d.id)).map((d) => d.id)),
    [visible, wire],
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

  /**
   * Send the switch, then ask the device what happened.
   *
   * The write's own result is never displayed. A dispatch that returns without
   * throwing says the tool ran, not that the lamp moved, so the tile keeps showing
   * the last read until a newer read replaces it.
   */
  async function toggle(id: string, next: boolean) {
    markPending(id, true);
    try {
      await api.invokeTool({
        server: DEVICE_SERVER,
        tool: "set_device_state",
        args: { device_id: id, power: next },
      });
    } catch {
      // Swallowed because the re-read below is what decides the label either way.
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

  // The head is the card's identity, so every state carries it. The count is left
  // blank until `settled`, because a number there is a claim about the house.
  const head = (label: string) => (
    <div className="hcc__head">
      <HubIco d={HP_PATHS.sliders} size={18} color="var(--color-text)" sw={1.9} />
      <span className="hcc__title">Devices</span>
      <span className="hcc__count">{label}</span>
    </div>
  );

  if (wire.status === "failed") {
    return (
      <div className="hcc" data-hook="home-controls" data-state="failed">
        {head("")}
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
        {head("")}
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

  const countText = known.length > room ? `${room} of ${known.length}` : String(known.length);

  return (
    <div className="hcc" data-hook="home-controls" data-state="ready">
      {head(countText)}

      {visible.length > 0 && (
        <div className="hcc__grid">
          {visible.map((device) => {
            if (!canPower(device.id)) {
              return (
                <div className="hcc__tile hcc__static" key={device.id}>
                  <span className="hcc__name">{device.name}</span>
                  <span className="hcc__room">{device.room}</span>
                </div>
              );
            }

            const read = reads[device.id];
            // The last thing the device said. It is what the tile shows even while a
            // write is in flight, which is the difference between optimism and
            // invention.
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
                  aria-busy={busy}
                  aria-label={`${device.name}, not reporting — open controls`}
                  onClick={() => openControls(device)}
                >
                  <span className="hcc__name">{device.name}</span>
                  <span className="hcc__value">Not reporting</span>
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
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
