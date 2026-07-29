import { useState, useEffect } from "react";
import { Button, Separator } from "@heroui/react";
import { Monitor, Cpu, Activity, Power, Settings, Plus, X } from "lucide-react";
import { api } from "../api/PondApiClient";
import type { Device } from "../api/types";
import { refreshHomeData } from "../hub/state/hubDataStore";

const DEVICE_TYPES = [
  { value: "host",          label: "Host / PC" },
  { value: "sensor",        label: "Sensor" },
  { value: "gotg",          label: "Mobile (GOTG)" },
  { value: "smart_speaker", label: "Smart speaker" },
  { value: "pond",          label: "Pond instance" },
  { value: "edge",          label: "Edge device" },
];

function DeviceIcon({ kind }: { kind: string | undefined }) {
  if (kind === "host")   return <Cpu size={22} />;
  if (kind === "sensor") return <Activity size={22} />;
  return <Monitor size={22} />;
}

function iconClass(kind: string | undefined, isOnline: boolean): string {
  if (!isOnline) return "device-card__icon";
  if (kind === "host")   return "device-card__icon device-card__icon--host";
  if (kind === "sensor") return "device-card__icon device-card__icon--sensor";
  return "device-card__icon device-card__icon--edge";
}

function timeSince(iso: string | null | undefined): string {
  if (!iso) return "—";
  const diff = Date.now() - new Date(iso).getTime();
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return "now";
  if (mins < 60) return `${mins} min ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

export function Devices() {
  const [devices, setDevices]     = useState<Device[]>([]);
  const [loading, setLoading]     = useState(true);
  const [error, setError]         = useState<string | null>(null);
  const [showForm, setShowForm]   = useState(false);

  // Form state
  const [name, setName]               = useState("");
  const [deviceType, setDeviceType]   = useState("host");
  const [hostname, setHostname]       = useState("");
  const [room, setRoom]               = useState("");
  const [submitting, setSubmitting]   = useState(false);
  const [formError, setFormError]     = useState<string | null>(null);

  // Per-card action state
  const [busyId, setBusyId]           = useState<string | null>(null);
  const [detail, setDetail]           = useState<Device | null>(null);

  // Configure-modal edit state
  const [editName, setEditName]         = useState("");
  const [editHostname, setEditHostname] = useState("");
  const [editRoom, setEditRoom]         = useState("");
  const [editSubmitting, setEditSubmitting] = useState(false);
  const [editError, setEditError]       = useState<string | null>(null);

  function load() {
    setLoading(true);
    api.listDevices()
      .then(setDevices)
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }

  useEffect(() => { load(); }, []);

  function openForm() {
    setName(""); setDeviceType("host"); setHostname(""); setRoom("");
    setFormError(null);
    setShowForm(true);
  }

  function closeForm() { setShowForm(false); setFormError(null); }

  async function handleRegister() {
    if (!name.trim()) { setFormError("Name is required."); return; }
    setSubmitting(true);
    setFormError(null);
    try {
      await api.registerDevice({
        name: name.trim(),
        device_type: deviceType,
        hostname: hostname.trim() || undefined,
        capabilities: [],
        room: room.trim() || undefined,
      });
      closeForm();
      load();
      void refreshHomeData();
    } catch (e) {
      setFormError(String(e));
    } finally {
      setSubmitting(false);
    }
  }

  // Toggle registry connectivity directly (heartbeat / offline), not the
  // giap-device-control MCP tool — that tool actuates a smart device's own
  // power state (a light/plug), a different concept from whether the device
  // itself is reachable. There is no "wake"/"restart" primitive in the
  // backend, so this is an honest on/off toggle: turn on when offline, off
  // when online.
  async function handlePower(d: Device) {
    setBusyId(d.id);
    try {
      if (d.is_online) {
        await api.markDeviceOffline(d.id);
      } else {
        await api.markDeviceOnline(d.id);
      }
      load();
      void refreshHomeData();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyId(null);
    }
  }

  function openDetail(d: Device) {
    setDetail(d);
    setEditName(d.name);
    setEditHostname(d.hostname ?? "");
    setEditRoom(d.room ?? "");
    setEditError(null);
  }

  async function handleUpdateDevice() {
    if (!detail) return;
    if (!editName.trim()) { setEditError("Name is required."); return; }
    setEditSubmitting(true);
    setEditError(null);
    try {
      const updated = await api.updateDevice(detail.id, {
        name: editName.trim(),
        hostname: editHostname.trim() || undefined,
        room: editRoom.trim() || undefined,
      });
      setDetail(updated);
      load();
      void refreshHomeData();
    } catch (e) {
      setEditError(String(e));
    } finally {
      setEditSubmitting(false);
    }
  }

  async function handleUnregister(d: Device) {
    setBusyId(d.id);
    try {
      await api.unregisterDevice(d.id);
      setDetail(null);
      load();
      void refreshHomeData();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyId(null);
    }
  }

  return (
    <div className="screen screen--devices">
      {/* ── Page header ────────────────────────────────────── */}
      <div className="page-header">
        <div>
          <h1 className="page-header__title">Devices</h1>
          <p className="dev-header__sub">All registered nodes on your local network.</p>
        </div>
        <div className="page-header__action">
          <Button size="sm" variant="primary" onPress={openForm}>
            <Plus size={14} /> Register device
          </Button>
        </div>
      </div>

      {loading && <p className="muted-12">Loading devices…</p>}
      {error   && <p className="muted-12 text-error">{error}</p>}

      {!loading && !error && devices.length === 0 && (
        <div className="empty-state">
          <Monitor size={32} />
          <span>No devices registered yet.</span>
          <button className="empty-state__cta" onClick={openForm}>
            <Plus size={14} /> Register device
          </button>
        </div>
      )}

      {devices.length > 0 && (
        <div className="devices-grid">
          {devices.map((d) => (
            <div
              key={d.id}
              className={`device-card${!d.is_online ? " device-card--offline" : ""}`}
            >
              {/* Top: icon + name + IP */}
              <div className="device-card__top">
                <span className={iconClass(d.device_type, d.is_online)}>
                  <DeviceIcon kind={d.device_type} />
                </span>
                <div className="device-card__info">
                  <div className="device-card__name">{d.name}</div>
                  <code className="device-card__ip">
                    {d.metadata?.ip != null ? String(d.metadata.ip) : "—"}
                  </code>
                </div>
              </div>

              {/* Chips: status + type + last seen */}
              <div className="device-card__chips">
                <span className={`device-card__chip device-card__chip--${d.is_online ? "online" : "offline"}`}>
                  <span className="device-card__dot" />
                  {d.is_online ? "online" : "offline"}
                </span>
                {d.device_type && (
                  <span className="device-card__chip">{d.device_type}</span>
                )}
                <span className="device-card__chip">{timeSince(d.last_seen)}</span>
              </div>

              {/* Actions */}
              <div className="device-card__actions">
                <button
                  className="device-card__action-btn"
                  onClick={() => handlePower(d)}
                  disabled={busyId === d.id}
                  type="button"
                >
                  <Power size={12} /> {d.is_online ? "Turn off" : "Turn on"}
                </button>
                <button
                  className="device-card__action-btn"
                  onClick={() => openDetail(d)}
                  type="button"
                >
                  <Settings size={12} /> Configure
                </button>
              </div>
            </div>
          ))}
        </div>
      )}

      {/* ── Register device modal ─────────────────────────── */}
      {showForm && (
        <div className="sched-modal__overlay" onClick={closeForm}>
          <div className="sched-modal__dialog" onClick={(e) => e.stopPropagation()}>
            <div className="sched-modal__header">
              <h2 className="sched-modal__title">Register device</h2>
              <button className="sched-modal__close" onClick={closeForm} aria-label="Close">
                <X size={16} />
              </button>
            </div>
            <Separator />

            <div className="sched-modal__body">
              <div className="sched-modal__field">
                <label className="sched-modal__label">Name</label>
                <input
                  className="sched-modal__input"
                  placeholder="Living Room Pi"
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                  autoFocus
                />
              </div>

              <div className="sched-modal__field">
                <label className="sched-modal__label">Device type</label>
                <select
                  className="sched-modal__select"
                  value={deviceType}
                  onChange={(e) => setDeviceType(e.target.value)}
                >
                  {DEVICE_TYPES.map((t) => (
                    <option key={t.value} value={t.value}>{t.label}</option>
                  ))}
                </select>
              </div>

              <div className="sched-modal__field">
                <label className="sched-modal__label">Hostname <span className="sched-modal__cron-hint">(optional)</span></label>
                <input
                  className="sched-modal__input"
                  placeholder="raspberrypi.local"
                  value={hostname}
                  onChange={(e) => setHostname(e.target.value)}
                />
              </div>

              <div className="sched-modal__field">
                <label className="sched-modal__label">Room <span className="sched-modal__cron-hint">(optional)</span></label>
                <input
                  className="sched-modal__input"
                  placeholder="Living Room"
                  value={room}
                  onChange={(e) => setRoom(e.target.value)}
                />
              </div>

              {formError && (
                <p className="text-error text-error--sm">{formError}</p>
              )}
            </div>

            <Separator />

            <div className="sched-modal__footer">
              <Button size="sm" variant="ghost" onPress={closeForm}>Cancel</Button>
              <Button
                size="sm"
                variant="primary"
                isDisabled={submitting || !name.trim()}
                onPress={handleRegister}
              >
                {submitting ? "Registering…" : "Register"}
              </Button>
            </div>
          </div>
        </div>
      )}

      {/* ── Device detail / configure modal ─────────────────── */}
      {detail && (
        <div className="sched-modal__overlay" onClick={() => setDetail(null)}>
          <div className="sched-modal__dialog" onClick={(e) => e.stopPropagation()}>
            <div className="sched-modal__header">
              <h2 className="sched-modal__title">{editName.trim() || detail.name}</h2>
              <button className="sched-modal__close" onClick={() => setDetail(null)} aria-label="Close">
                <X size={16} />
              </button>
            </div>
            <Separator />
            <div className="sched-modal__body">
              <div className="sched-modal__field">
                <label className="sched-modal__label">Name</label>
                <input
                  className="sched-modal__input"
                  value={editName}
                  onChange={(e) => setEditName(e.target.value)}
                  disabled={editSubmitting}
                />
              </div>
              <div className="sched-modal__field">
                <label className="sched-modal__label">Hostname <span className="sched-modal__cron-hint">(optional)</span></label>
                <input
                  className="sched-modal__input"
                  placeholder="raspberrypi.local"
                  value={editHostname}
                  onChange={(e) => setEditHostname(e.target.value)}
                  disabled={editSubmitting}
                />
              </div>
              <div className="sched-modal__field">
                <label className="sched-modal__label">Room <span className="sched-modal__cron-hint">(optional)</span></label>
                <input
                  className="sched-modal__input"
                  placeholder="Living Room"
                  value={editRoom}
                  onChange={(e) => setEditRoom(e.target.value)}
                  disabled={editSubmitting}
                />
              </div>
              <div className="sched-modal__field">
                <label className="sched-modal__label">Type</label>
                <div className="muted-12">{detail.device_type ?? "—"}</div>
              </div>
              <div className="sched-modal__field">
                <label className="sched-modal__label">Status</label>
                <div className="muted-12">
                  {detail.is_online ? "online" : "offline"} · last seen {timeSince(detail.last_seen)}
                </div>
              </div>
              <div className="sched-modal__field">
                <label className="sched-modal__label">Address</label>
                <code className="device-card__ip">
                  {detail.metadata?.ip != null ? String(detail.metadata.ip) : "—"}
                </code>
              </div>
              {editError && (
                <p className="text-error text-error--sm">{editError}</p>
              )}
            </div>
            <Separator />
            <div className="sched-modal__footer">
              <Button size="sm" variant="ghost" onPress={() => setDetail(null)}>Close</Button>
              <Button
                size="sm"
                variant="danger"
                isDisabled={busyId === detail.id}
                onPress={() => handleUnregister(detail)}
              >
                {busyId === detail.id ? "Removing…" : "Unregister device"}
              </Button>
              <Button
                size="sm"
                variant="primary"
                isDisabled={editSubmitting || !editName.trim()}
                onPress={handleUpdateDevice}
              >
                {editSubmitting ? "Saving…" : "Save"}
              </Button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}
