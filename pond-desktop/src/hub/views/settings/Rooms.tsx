import { useState, useEffect, useCallback, useRef } from "react";
import { Plus, RefreshCw } from "lucide-react";
import { HubIco } from "../../primitives/HubIco";
import { HP_PATHS } from "../../primitives/icons";
import { DetailShell } from "./DetailShell";
import { Card, Row } from "./controls";
import { api } from "../../../api/PondApiClient";
import type { Device } from "../../../api/types";

// ─── Icon path strings for this view ─────────────────────────
const SICN = {
  device: "M6 4h12a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H6a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2zM9 9h6v6H9zM9 1v3M15 1v3M9 20v3M15 20v3M1 9h3M1 15h3M20 9h3M20 15h3",
  sensor: "M22 12h-4l-3 9L9 3l-3 9H2",
  host:   "M20 17a2 2 0 0 0 2-2V4a2 2 0 0 0-2-2H9.5a2 2 0 0 0-2 1.57L5 14.5A2 2 0 0 0 7 17M16 17H7M12 17v4M8 21h8",
} as const;

// ─── Room → icon key mapping ───────────────────────────────────
function roomIconKey(name: string): keyof typeof HP_PATHS | null {
  const n = name.toLowerCase();
  if (n.includes("living"))  return "sofa";
  if (n.includes("kitchen")) return "utensils";
  if (n.includes("bed"))     return "bed";
  if (n.includes("office"))  return "list";
  if (n.includes("outdoor") || n.includes("garden") || n.includes("yard")) return "tree";
  if (n.includes("bath"))    return "droplet";
  if (n.includes("hall"))    return "home";
  if (n.includes("garage"))  return "plug";
  return "home";
}

// ─── Offline mock fallback ─────────────────────────────────────
const MOCK_DEVICES: Device[] = [
  { id: "mock-1", name: "Jetson Orin Nano",   device_type: "host",   room: "Office",  is_online: true,  last_seen: new Date().toISOString() },
  { id: "mock-2", name: "Motion Sensor A",    device_type: "sensor", room: "Office",  is_online: true,  last_seen: new Date().toISOString() },
  { id: "mock-3", name: "Front Door Camera",  device_type: "sensor", room: "Outdoor", is_online: false, last_seen: new Date(Date.now() - 3_600_000).toISOString() },
  { id: "mock-4", name: "Backyard Sensor",    device_type: "sensor", room: "Outdoor", is_online: true,  last_seen: new Date().toISOString() },
  { id: "mock-5", name: "Hub Controller",     device_type: "host",   room: "Living Room", is_online: true, last_seen: new Date().toISOString() },
  { id: "mock-6", name: "Climate Sensor",     device_type: "sensor", room: "Living Room", is_online: false, last_seen: new Date(Date.now() - 7_200_000).toISOString() },
];

// ─── Group devices by room ─────────────────────────────────────
function groupByRoom(devices: Device[]): Map<string, Device[]> {
  const map = new Map<string, Device[]>();
  for (const d of devices) {
    const room = d.room ?? "Unassigned";
    const list = map.get(room) ?? [];
    list.push(d);
    map.set(room, list);
  }
  return map;
}

// ─── Device icon path helper ───────────────────────────────────
function deviceIconPath(kind: string | undefined): string {
  if (kind === "sensor") return SICN.sensor;
  if (kind === "host")   return SICN.host;
  return SICN.device;
}

// ─── Device sub-label ─────────────────────────────────────────
function deviceSubLabel(d: Device): string {
  const type = d.device_type ?? "device";
  if (!d.is_online) return `${type} · offline`;
  if (d.last_seen) {
    const diff = Date.now() - new Date(d.last_seen).getTime();
    const mins = Math.floor(diff / 60_000);
    if (mins < 1) return `${type} · online`;
    if (mins < 60) return `${type} · ${mins}m ago`;
    const hrs = Math.floor(mins / 60);
    return `${type} · ${hrs}h ago`;
  }
  return `${type} · online`;
}

// ─── Skeleton room card ────────────────────────────────────────
function SkeletonRoom() {
  return (
    <div className="setcard" style={{ opacity: 0.5 }}>
      <div className="setcard__head">
        <span style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <span style={{ width: 16, height: 16, background: "#e2e8f0", borderRadius: 4, display: "inline-block" }} />
          <span style={{ display: "inline-block", height: 12, width: 100, background: "#e2e8f0", borderRadius: 4 }} />
          <span style={{ display: "inline-block", height: 10, width: 50, background: "#f1f5f9", borderRadius: 4 }} />
        </span>
        <span style={{ display: "inline-block", height: 22, width: 60, background: "#f1f5f9", borderRadius: 6 }} />
      </div>
      {[0, 1].map((i) => (
        <div key={i} className="srow" style={{ gap: 8 }}>
          <span className="srow__text">
            <span style={{ display: "block", height: 11, width: 120, background: "#e2e8f0", borderRadius: 4, marginBottom: 4 }} />
            <span style={{ display: "block", height: 9, width: 80, background: "#f1f5f9", borderRadius: 4 }} />
          </span>
          <span style={{ width: 36, height: 20, background: "#f1f5f9", borderRadius: 10, display: "inline-block" }} />
        </div>
      ))}
    </div>
  );
}

// ─── Per-device toggle — controlled + optimistic ─────────────
interface DeviceToggleProps {
  device: Device;
  onFlash: (text: string, ok: boolean) => void;
}

function DeviceToggle({ device, onFlash }: DeviceToggleProps) {
  const [on, setOn] = useState(device.is_online);
  const [busy, setBusy] = useState(false);

  async function handleChange() {
    const next = !on;
    setOn(next);   // optimistic
    setBusy(true);
    try {
      await api.invokeTool({
        server: "giap-device-control",
        tool: "set_device_state",
        args: { device_id: device.id, power: next },
      });
      onFlash(`${device.name} turned ${next ? "on" : "off"}.`, true);
    } catch (e) {
      setOn(!next);  // revert on failure
      onFlash(`Failed to update ${device.name}: ${String(e)}`, false);
    } finally {
      setBusy(false);
    }
  }

  return (
    <button
      className="htoggle"
      data-on={on}
      onClick={handleChange}
      aria-pressed={on}
      disabled={busy}
      type="button"
      aria-label={`${on ? "Turn off" : "Turn on"} ${device.name}`}
      style={{ opacity: busy ? 0.5 : 1, cursor: busy ? "wait" : "pointer" }}
    >
      <span className="htoggle__knob" />
    </button>
  );
}

// ─── Component ───────────────────────────────────────────────
interface RoomsDetailProps {
  go: (route: string) => void;
}

export function RoomsDetail({ go }: RoomsDetailProps) {
  const [devices, setDevices] = useState<Device[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [flash, setFlash] = useState<{ text: string; ok: boolean } | null>(null);
  const flashTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  function showFlash(text: string, ok = true) {
    if (flashTimer.current) clearTimeout(flashTimer.current);
    setFlash({ text, ok });
    flashTimer.current = setTimeout(() => setFlash(null), 3_000);
  }

  const loadData = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const fetched = await api.listDevices();
      setDevices(fetched);
    } catch (e) {
      console.warn("[RoomsDetail] API offline — using mock fallback:", e);
      setDevices(MOCK_DEVICES);
      setError("Could not reach the server. Showing offline view.");
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadData();
    return () => {
      if (flashTimer.current) clearTimeout(flashTimer.current);
    };
  }, [loadData]);

  // ── Derived ──────────────────────────────────────────────────
  const byRoom = groupByRoom(devices);
  const roomNames = Array.from(byRoom.keys()).sort();
  const totalCount = devices.length;
  const roomCount = byRoom.size;

  return (
    <DetailShell
      title="Rooms & Devices"
      subtitle={
        loading
          ? "Loading devices..."
          : `${roomCount} room${roomCount !== 1 ? "s" : ""} · ${totalCount} device${totalCount !== 1 ? "s" : ""} connected.`
      }
      accent="#7C3AED"
      onBack={() => go("settings")}
      headRight={
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <button
            className="mrow__btn"
            type="button"
            onClick={loadData}
            aria-label="Refresh devices"
            style={{ minWidth: 32, display: "flex", alignItems: "center", justifyContent: "center" }}
          >
            <RefreshCw size={13} strokeWidth={2} style={{ opacity: loading ? 0.4 : 1 }} />
          </button>
          {/* TODO Phase 8 wave 5: open add-device wizard — api.registerDevice(...) */}
          <button
            className="primary-btn"
            type="button"
            onClick={() => { /* TODO: open add-device wizard */ }}
          >
            <Plus size={15} color="#fff" strokeWidth={2.2} /> Add device
          </button>
        </div>
      }
    >
      {/* Flash feedback */}
      {flash && (
        <div
          style={{
            padding: "8px 12px",
            borderRadius: 6,
            fontSize: 13,
            background: flash.ok ? "#f0fdf4" : "#fef2f2",
            color: flash.ok ? "#16a34a" : "#dc2626",
            border: `1px solid ${flash.ok ? "#bbf7d0" : "#fecaca"}`,
          }}
          role="status"
          aria-live="polite"
        >
          {flash.text}
        </div>
      )}

      {/* Offline error banner */}
      {error && (
        <div
          style={{
            padding: "8px 12px",
            borderRadius: 6,
            fontSize: 13,
            background: "#fffbeb",
            color: "#92400e",
            border: "1px solid #fde68a",
          }}
        >
          {error}
        </div>
      )}

      {/* Skeleton while loading */}
      {loading && (
        <>
          <SkeletonRoom />
          <SkeletonRoom />
          <SkeletonRoom />
        </>
      )}

      {/* Empty state */}
      {!loading && devices.length === 0 && (
        <div className="memempty" style={{ textAlign: "center", padding: "32px 16px" }}>
          No devices registered yet. Add one to get started.
        </div>
      )}

      {/* Room cards */}
      {!loading &&
        roomNames.map((roomName) => {
          const devs = byRoom.get(roomName) ?? [];
          const iconKey = roomIconKey(roomName);
          const iconPath = iconKey ? HP_PATHS[iconKey] : null;

          return (
            <Card
              key={roomName}
              title={
                <span style={{ display: "inline-flex", alignItems: "center", gap: 8 }}>
                  {iconPath && (
                    <HubIco d={iconPath} size={16} color="#7C3AED" />
                  )}
                  {roomName}
                  <span className="room-count">
                    {devs.length} device{devs.length !== 1 ? "s" : ""}
                  </span>
                </span>
              }
              right={
                /* TODO: open a room-editor modal once there is an edit-room API. */
                <button
                  className="mrow__btn"
                  type="button"
                  disabled
                  title="Room editing coming soon"
                >
                  Edit room
                </button>
              }
            >
              {devs.length > 0 ? (
                <div className="devlist">
                  {devs.map((d) => (
                    <Row
                      key={d.id}
                      label={d.name}
                      sub={deviceSubLabel(d)}
                      control={
                        <span style={{ display: "flex", alignItems: "center", gap: 6 }}>
                          <span
                            style={{
                              width: 6,
                              height: 6,
                              borderRadius: "50%",
                              background: d.is_online ? "#22c55e" : "var(--color-text-tertiary)",
                              display: "inline-block",
                              flexShrink: 0,
                            }}
                            title={d.is_online ? "Online" : "Offline"}
                          />
                          <DeviceToggle device={d} onFlash={showFlash} />
                        </span>
                      }
                    />
                  ))}
                </div>
              ) : (
                <div className="memempty">No devices in this room yet.</div>
              )}
            </Card>
          );
        })}
    </DetailShell>
  );
}
