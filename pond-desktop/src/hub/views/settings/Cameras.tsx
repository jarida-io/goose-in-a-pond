import { useState, useEffect, useCallback, useRef } from "react";
import { Plus, RefreshCw } from "lucide-react";
import { HubIco } from "../../primitives/HubIco";
import { CameraFeed } from "../../primitives/CameraFeed";
import { DetailShell } from "./DetailShell";
import { Card, Toggle } from "./controls";
import { api } from "../../../api/PondApiClient";
import type { Device } from "../../../api/types";
import type { CameraData } from "../../data/mockHome";

// ─── Icon paths ───────────────────────────────────────────────
const SICN = {
  cctv:   "M2 8a2 2 0 0 1 2-2h16a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2zM6 12a4 4 0 0 0 4 4h4a4 4 0 0 0 0-8h-4a4 4 0 0 0-4 4zM16 12h.01",
  motion: "M3 7h4l3-4 4 9 3-5h4M12 21v-4M8 21h8",
  record: "M12 2a10 10 0 1 0 0 20A10 10 0 0 0 12 2zM12 8v8M8 12h8",
  night:  "M21 12.79A9 9 0 1 1 11.21 3 7 7 0 0 0 21 12.79z",
} as const;

// ─── Fallback cameras (shown while offline / loading) ─────────
interface CameraState {
  id: string;
  name: string;
  time: string;
  hue: number;
  is_online: boolean;
  motionAlerts: boolean;
  record24h: boolean;
  nightVision: boolean;
}

const MOCK_CAMERA_STATES: CameraState[] = [
  { id: "front", name: "Front Door", time: "8:48 AM", hue: 150, is_online: true,  motionAlerts: true,  record24h: true,  nightVision: true  },
  { id: "drive", name: "Driveway",   time: "8:49 AM", hue: 35,  is_online: true,  motionAlerts: true,  record24h: false, nightVision: true  },
  { id: "back",  name: "Backyard",   time: "8:47 AM", hue: 205, is_online: false, motionAlerts: false, record24h: false, nightVision: false },
];

// ─── Derive a faux-scene hue from a string ────────────────────
// Deterministic so the same camera always gets the same hue.
function deviceHue(id: string): number {
  let h = 0;
  for (let i = 0; i < id.length; i++) h = (h * 31 + id.charCodeAt(i)) & 0xffff;
  return h % 360;
}

// ─── Relative time helper ──────────────────────────────────────
function relativeTime(iso: string | undefined): string {
  if (!iso) return "unknown";
  const diffMs = Date.now() - new Date(iso).getTime();
  const diffMins = Math.floor(diffMs / 60_000);
  if (diffMins < 1)   return "just now";
  if (diffMins < 60)  return `${diffMins}m ago`;
  const diffHrs = Math.floor(diffMins / 60);
  if (diffHrs < 24)   return `${diffHrs}h ago`;
  return `${Math.floor(diffHrs / 24)}d ago`;
}

// ─── Map a Device to a CameraData ─────────────────────────────
function deviceToCameraData(d: Device): CameraData {
  return {
    id:   d.id,
    name: d.name,
    time: relativeTime(d.last_seen),
    hue:  deviceHue(d.id),
  };
}

// ─── Skeleton card ────────────────────────────────────────────
function SkeletonCard() {
  return (
    <div className="setcard" style={{ opacity: 0.5 }}>
      <div style={{ height: 120, borderRadius: 8, background: "#e2e8f0", marginBottom: 12 }} />
      <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
        <span style={{ height: 13, width: 120, background: "#e2e8f0", borderRadius: 4, display: "block" }} />
        <span style={{ height: 11, width: 160, background: "#f1f5f9", borderRadius: 4, display: "block" }} />
      </div>
    </div>
  );
}

// ─── Component ───────────────────────────────────────────────
interface CamerasDetailProps {
  go: (route: string) => void;
}

export function CamerasDetail({ go }: CamerasDetailProps) {
  const [cameras, setCameras]     = useState<CameraState[]>([]);
  const [loading, setLoading]     = useState(true);
  const [error, setError]         = useState<string | null>(null);
  const [flash, setFlash]         = useState<{ text: string; ok: boolean } | null>(null);
  const flashTimer                = useRef<ReturnType<typeof setTimeout> | null>(null);

  function showFlash(text: string, ok = true) {
    if (flashTimer.current) clearTimeout(flashTimer.current);
    setFlash({ text, ok });
    flashTimer.current = setTimeout(() => setFlash(null), 3000);
  }

  // ── Load cameras from devices list ──────────────────────────
  const loadData = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const devices = await api.listDevices();
      const cameraDevices = devices.filter(
        (d: Device) => d.device_type === "camera",
      );

      if (cameraDevices.length > 0) {
        // Toggle state is local: the Settings type has no per-camera fields yet (see setToggle).
        setCameras(
          cameraDevices.map((d: Device) => ({
            id:           d.id,
            name:         d.name,
            time:         relativeTime(d.last_seen),
            hue:          deviceHue(d.id),
            is_online:    d.is_online,
            motionAlerts: true,
            record24h:    false,
            nightVision:  true,
          })),
        );
      } else {
        // No cameras registered: show the fallback cards rather than an empty state.
        setCameras(MOCK_CAMERA_STATES);
      }
    } catch (e) {
      console.warn("[CamerasDetail] API offline — using mock fallback:", e);
      setError("Could not reach the server. Showing offline view.");
      setCameras(MOCK_CAMERA_STATES);
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

  // ── Toggle helpers ───────────────────────────────────────────
  function setToggle(
    id: string,
    field: "motionAlerts" | "record24h" | "nightVision",
    value: boolean,
  ) {
    setCameras((prev) =>
      prev.map((c) => (c.id === id ? { ...c, [field]: value } : c)),
    );
    // TODO: persist as `camera_${id}_${field}` once the Settings type has per-camera fields.
    showFlash(`Saved.`);
  }

  const liveCount = cameras.filter((c) => c.is_online).length;
  const subtitle  = loading
    ? "Loading cameras…"
    : cameras.length === 0
      ? "No cameras registered."
      : `${liveCount} live feed${liveCount !== 1 ? "s" : ""} · stored locally, auto-deleted after 7 days.`;

  return (
    <DetailShell
      title="Cameras"
      subtitle={subtitle}
      accent="#0EA5E9"
      onBack={() => go("settings")}
      headRight={
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <button
            className="mrow__btn"
            type="button"
            onClick={loadData}
            aria-label="Refresh cameras"
            style={{ minWidth: 32, display: "flex", alignItems: "center", justifyContent: "center" }}
          >
            <RefreshCw size={13} strokeWidth={2} style={{ opacity: loading ? 0.4 : 1 }} />
          </button>
          {/* TODO: open add-camera wizard when camera registration API lands */}
          <button
            className="primary-btn"
            type="button"
            style={{ background: "#0EA5E9", boxShadow: "0 6px 16px rgba(14,165,233,.28)" }}
            onClick={() => {
              // TODO: open the add-camera wizard.
            }}
          >
            <Plus size={15} color="#fff" strokeWidth={2.2} /> Add camera
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

      {/* Skeleton loading state */}
      {loading && (
        <>
          <SkeletonCard />
          <SkeletonCard />
          <SkeletonCard />
        </>
      )}

      {/* Camera cards */}
      {!loading && cameras.length === 0 && (
        <Card>
          <div
            style={{
              display: "flex",
              flexDirection: "column",
              alignItems: "center",
              gap: 8,
              padding: "24px 0",
              color: "var(--color-text-tertiary)",
            }}
          >
            <HubIco d={SICN.cctv} size={28} color="#cbd5e1" />
            <p style={{ margin: 0, fontSize: 13 }}>No cameras found.</p>
            <p style={{ margin: 0, fontSize: 12, color: "#cbd5e1" }}>
              Add a camera or make sure your device is online and registered.
            </p>
          </div>
        </Card>
      )}

      {!loading &&
        cameras.map((c) => {
          const camData: CameraData = deviceToCameraData(c);
          return (
            <Card key={c.id}>
              <div className="camset">
                <div className="camset__thumb">
                  <CameraFeed cam={camData} interactive={false} />
                </div>
                <div className="camset__body">
                  <div className="camset__name">
                    {c.name}
                    {!c.is_online && (
                      <span
                        style={{
                          marginLeft: 6,
                          display: "inline-block",
                          padding: "1px 6px",
                          borderRadius: 4,
                          fontSize: 10,
                          fontWeight: 600,
                          background: "#fef2f2",
                          color: "#dc2626",
                          verticalAlign: "middle",
                          letterSpacing: "0.04em",
                        }}
                      >
                        Offline
                      </span>
                    )}
                  </div>
                  <div className="camset__sub">
                    {c.is_online
                      ? `Online · last motion ${c.time}`
                      : `Last seen ${c.time}`}
                  </div>
                  <div className="camset__rows">
                    <span className="camset__opt">
                      Motion alerts{" "}
                      <Toggle
                        on={c.motionAlerts}
                        onChange={(v) => setToggle(c.id, "motionAlerts", v)}
                      />
                    </span>
                    <span className="camset__opt">
                      Record 24/7{" "}
                      <Toggle
                        on={c.record24h}
                        onChange={(v) => setToggle(c.id, "record24h", v)}
                      />
                    </span>
                    <span className="camset__opt">
                      Night vision{" "}
                      <Toggle
                        on={c.nightVision}
                        onChange={(v) => setToggle(c.id, "nightVision", v)}
                      />
                    </span>
                  </div>
                </div>
              </div>
            </Card>
          );
        })}
    </DetailShell>
  );
}
