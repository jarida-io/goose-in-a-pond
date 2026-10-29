import { useCallback, useState, useEffect } from "react";
import { Button, Separator, Switch } from "@heroui/react";
import {
  Monitor, Cpu, Activity, Power, Settings, Plus, X, Radio, Smartphone,
  Lightbulb, Lock, Thermometer, Fan, Blinds, RefreshCw, Pencil, Trash2,
  PlugZap, WashingMachine, Waves, Wind, MonitorPlay, Bot, BellRing, ToggleLeft,
  Router,
  Droplet,
} from "lucide-react";
import { api } from "../api/PondApiClient";
import { ApiError, type Device, type MatterStatus, type Room } from "../api/types";
import { useConfirm, ErrorBanner } from "../components/shared";
import { refreshHomeData } from "../hub/state/hubDataStore";
import { powerStateOf } from "./devicePower";
import { timeSince } from "./timeSince";

/** The server's own message, without the `ApiError:` prefix `String(e)` adds. */
function errorText(e: unknown): string {
  return e instanceof ApiError ? e.message : String(e);
}

/** Matter state labels, kept as data so the chip, hint and commission gate agree. */
const MATTER_STATE_LABEL: Record<MatterStatus["state"], string> = {
  disabled:    "Off",
  connecting:  "Starting…",
  connected:   "Connected",
  unreachable: "Cannot reach controller",
};

/** How often to re-check while the controller is starting up. */
const MATTER_POLL_MS = 2000;


function DeviceIcon({ kind }: { kind: string | undefined }) {
  if (kind === "host")   return <Cpu size={22} />;
  if (kind === "gotg")   return <Smartphone size={22} />; // the mobile companion
  if (kind === "sensor") return <Activity size={22} />;
  // Matter device types the backend infers from clusters.
  if (kind === "light")      return <Lightbulb size={22} />;
  if (kind === "lock")       return <Lock size={22} />;
  if (kind === "thermostat") return <Thermometer size={22} />;
  if (kind === "fan")        return <Fan size={22} />;
  if (kind === "covering")   return <Blinds size={22} />;
  // Types the node declares in its Matter Descriptor; by clusters alone a dishwasher looks like a light.
  if (kind === "plug")       return <PlugZap size={22} />;
  if (kind === "appliance")  return <WashingMachine size={22} />;
  if (kind === "pump")       return <Waves size={22} />;
  if (kind === "air")        return <Wind size={22} />;
  if (kind === "media")      return <MonitorPlay size={22} />;
  if (kind === "vacuum")     return <Bot size={22} />;
  if (kind === "alarm")      return <BellRing size={22} />;
  // A Generic Switch reports its position and takes no commands, so it is not a light.
  if (kind === "switch")     return <ToggleLeft size={22} />;
  // A hub (Hue, Aqara, Tuya) that speaks for other devices and drives nothing itself.
  if (kind === "bridge")     return <Router size={22} />;
  // A valve opens and shuts; it has no On/Off cluster.
  if (kind === "valve")      return <Droplet size={22} />;
  return <Monitor size={22} />;
}

function iconClass(kind: string | undefined, isOnline: boolean): string {
  if (!isOnline) return "device-card__icon";
  if (kind === "host")   return "device-card__icon device-card__icon--host";
  // An alarm reads as a sensor: it is a thing that reports, not one you drive.
  if (kind === "sensor" || kind === "alarm") {
    return "device-card__icon device-card__icon--sensor";
  }
  return "device-card__icon device-card__icon--edge";
}

/** "now" while online (online means heard from recently); otherwise when it went quiet. */
function lastSeen(device: Pick<Device, "is_online" | "last_seen">): string {
  return device.is_online ? "now" : timeSince(device.last_seen);
}

function canPower(d: Device): boolean {
  return d.capabilities?.includes("power") ?? false;
}

/** Every room the household has named, each with its devices, plus a final bucket for devices
 *  whose `room_id` is absent or points at nothing (deleted, or never assigned). Named rooms
 *  sort alphabetically; "Unassigned" is always last, and only shown when it would hold
 *  something or there are no named rooms to group by at all. */
function groupDevicesByRoom(
  devices: Device[],
  rooms: Room[],
): Array<{ room: Room | null; devices: Device[] }> {
  const byId = new Map(rooms.map((r) => [r.id, r] as const));
  const grouped = new Map<string, Device[]>();
  for (const d of devices) {
    const key = d.room_id && byId.has(d.room_id) ? d.room_id : "";
    const list = grouped.get(key) ?? [];
    list.push(d);
    grouped.set(key, list);
  }
  const sections: Array<{ room: Room | null; devices: Device[] }> = rooms
    .slice()
    .sort((a, b) => a.name.localeCompare(b.name))
    .map((room) => ({ room, devices: grouped.get(room.id) ?? [] }));
  const unassigned = grouped.get("") ?? [];
  if (unassigned.length > 0 || rooms.length === 0) {
    sections.push({ room: null, devices: unassigned });
  }
  return sections;
}

interface DeviceCardProps {
  device: Device;
  isPoweredOn: boolean | undefined;
  busy: boolean;
  onPower: (d: Device) => void;
  onConfigure: (d: Device) => void;
}

function DeviceCard({ device: d, isPoweredOn, busy, onPower, onConfigure }: DeviceCardProps) {
  return (
    <div className={`device-card${!d.is_online ? " device-card--offline" : ""}`}>
      {/* Top: icon + name + IP */}
      <div className="device-card__top">
        <span className={iconClass(d.device_type, d.is_online)}>
          <DeviceIcon kind={d.device_type} />
        </span>
        <div className="device-card__info">
          <div className="device-card__name">{d.name}</div>
          <code className="device-card__ip">
            {/* `ip_address` is the field the server sends. This read
                `metadata.ip`, which `list_devices` has never set, so the
                line rendered an em dash for every device on the fabric. */}
            {d.ip_address ?? "—"}
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
        <span className="device-card__chip">{lastSeen(d)}</span>
      </div>

      {/* Actions */}
      <div className="device-card__actions">
        {/* Only a device that can be switched. A contact sensor's
            capability list is empty, and it was being offered "Turn on". */}
        {canPower(d) && (
          <button
            className="device-card__action-btn"
            onClick={() => onPower(d)}
            disabled={busy || !d.is_online}
            type="button"
          >
            <Power size={12} /> {isPoweredOn ? "Turn off" : "Turn on"}
          </button>
        )}
        <button
          className="device-card__action-btn"
          onClick={() => onConfigure(d)}
          type="button"
        >
          <Settings size={12} /> Configure
        </button>
      </div>
    </div>
  );
}

export function Devices() {
  const confirm = useConfirm();
  const [devices, setDevices]     = useState<Device[]>([]);
  const [rooms, setRooms]         = useState<Room[]>([]);
  const [loading, setLoading]     = useState(true);
  const [error, setError]         = useState<string | null>(null);
  const [showForm, setShowForm]   = useState(false);

  // Form state
  const [name, setName]               = useState("");
  const [setupCode, setSetupCode]     = useState("");
  const [submitting, setSubmitting]   = useState(false);
  const [formError, setFormError]     = useState<string | null>(null);

  // Matter section state
  const [matter, setMatter]           = useState<MatterStatus | null>(null);
  const [matterBusy, setMatterBusy]   = useState(false);
  const [matterError, setMatterError] = useState<string | null>(null);

  // Per-card action state
  const [busyId, setBusyId]           = useState<string | null>(null);
  const [detail, setDetail]           = useState<Device | null>(null);

  // Power as each device reports it; absent means it did not say (never inferred from reachability).
  const [powerOn, setPowerOn]         = useState<Record<string, boolean>>({});

  // Configure-modal edit state
  const [editName, setEditName]         = useState("");
  const [editHostname, setEditHostname] = useState("");
  // "" means Unassigned; otherwise a room id from `rooms`.
  const [editRoomId, setEditRoomId]     = useState("");
  const [editSubmitting, setEditSubmitting] = useState(false);
  const [editError, setEditError]       = useState<string | null>(null);

  // Room create/rename modal
  const [roomModal, setRoomModal] = useState<{ mode: "create" | "rename"; target?: Room } | null>(null);
  const [roomModalName, setRoomModalName] = useState("");
  const [roomModalSubmitting, setRoomModalSubmitting] = useState(false);
  const [roomModalError, setRoomModalError] = useState<string | null>(null);
  const [guestAccessBusy, setGuestAccessBusy] = useState<string | null>(null);

  /** Ask each switchable device its power state; a failed read leaves it absent, not "off". */
  const loadPowerStates = useCallback(async (list: Device[]) => {
    const switchable = list.filter(canPower);
    if (switchable.length === 0) return;
    const readings = await Promise.all(
      switchable.map(async (d) => {
        try {
          const result = await api.invokeTool({
            server: "giap-device-control",
            tool: "get_device_state",
            args: { device_id: d.id },
          });
          return [d.id, powerStateOf(result.content)] as const;
        } catch {
          return [d.id, undefined] as const;
        }
      }),
    );
    setPowerOn(
      Object.fromEntries(
        readings.filter((r): r is readonly [string, boolean] => r[1] !== undefined),
      ),
    );
  }, []);

  const load = useCallback(() => {
    setLoading(true);
    Promise.all([api.listDevices(), api.listRooms().catch(() => [])])
      .then(([deviceList, roomList]) => {
        setDevices(deviceList);
        setRooms(roomList);
        void loadPowerStates(deviceList);
      })
      .catch((e) => setError(errorText(e)))
      .finally(() => setLoading(false));
  }, [loadPowerStates]);

  /** The Matter runtime's live state: what it is doing, not what was saved. */
  function loadMatter() {
    return api.getMatterStatus()
      .then((s) => {
        setMatter(s);
        return s;
      })
      .catch((e) => { setMatterError(errorText(e)); return null; });
  }

  useEffect(() => { load(); void loadMatter(); }, [load]);

  // The settings save does not wait for the controller to start, so poll while connecting.
  useEffect(() => {
    if (matter?.state !== "connecting") return;
    const timer = setInterval(() => { void loadMatter(); }, MATTER_POLL_MS);
    return () => clearInterval(timer);
  }, [matter?.state]);

  /** Retry: the reconciler treats re-sent unchanged settings while unreachable as a reconnect. */
  async function retryMatter() {
    setMatterBusy(true);
    setMatterError(null);
    try {
      await api.updateSettings({ matter_ws_url: matter?.url ?? "" });
      // Optimistic; the poll replaces it with the real state.
      setMatter((m) => (m ? { ...m, state: "connecting", error: undefined } : m));
      await loadMatter();
    } catch (e) {
      setMatterError(errorText(e));
      void loadMatter();
    } finally {
      setMatterBusy(false);
    }
  }

  function openForm() {
    setName(""); setSetupCode("");
    setFormError(null);
    setShowForm(true);
  }

  function closeForm() { setShowForm(false); setFormError(null); }

  /** GIAP pairs the device and the bridge registers it from the controller's report, so just reload. */
  async function handleCommission() {
    if (!setupCode.trim()) { setFormError("Enter the device's setup code."); return; }
    setSubmitting(true);
    setFormError(null);
    try {
      await api.commissionDevice(setupCode.trim(), name.trim() || undefined);
      closeForm();
      load();
    } catch (e) {
      setFormError(errorText(e));
    } finally {
      setSubmitting(false);
    }
  }

  /** Switch the device's own power, then read it back rather than assume it took. */
  async function handlePower(d: Device) {
    const next = !(powerOn[d.id] ?? false);
    setBusyId(d.id);
    try {
      await api.invokeTool({
        server: "giap-device-control",
        tool: "set_device_state",
        args: { device_id: d.id, power: next },
      });
      await loadPowerStates([d]);
      void refreshHomeData();
    } catch (e) {
      setError(errorText(e));
    } finally {
      setBusyId(null);
    }
  }

  /** Registry reachability, not power. `is_online` means `last_seen` < 5 min old, so
   *  "offline" backdates it; there is no wake/restart primitive. */
  async function handleReachability(d: Device) {
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
      setEditError(errorText(e));
    } finally {
      setBusyId(null);
    }
  }

  function openDetail(d: Device) {
    setDetail(d);
    setEditName(d.name);
    setEditHostname(d.hostname ?? "");
    setEditRoomId(d.room_id ?? "");
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
        room_id: editRoomId || null,
      });
      setDetail(updated);
      load();
      void refreshHomeData();
    } catch (e) {
      setEditError(errorText(e));
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
      setError(errorText(e));
    } finally {
      setBusyId(null);
    }
  }

  function openCreateRoom() {
    setRoomModal({ mode: "create" });
    setRoomModalName("");
    setRoomModalError(null);
  }

  function openRenameRoom(room: Room) {
    setRoomModal({ mode: "rename", target: room });
    setRoomModalName(room.name);
    setRoomModalError(null);
  }

  function closeRoomModal() {
    setRoomModal(null);
    setRoomModalError(null);
  }

  async function handleSaveRoom() {
    if (!roomModal) return;
    const trimmed = roomModalName.trim();
    if (!trimmed) { setRoomModalError("A room needs a name."); return; }
    setRoomModalSubmitting(true);
    setRoomModalError(null);
    try {
      if (roomModal.mode === "create") {
        await api.createRoom(trimmed);
      } else if (roomModal.target) {
        await api.renameRoom(roomModal.target.id, trimmed);
      }
      closeRoomModal();
      load();
    } catch (e) {
      setRoomModalError(errorText(e));
    } finally {
      setRoomModalSubmitting(false);
    }
  }

  async function handleDeleteRoom(room: Room) {
    const ok = await confirm(
      `Delete "${room.name}"? Any device still placed in it must be moved first.`,
      { title: "Delete room", confirmLabel: "Delete", destructive: true },
    );
    if (!ok) return;
    try {
      await api.deleteRoom(room.id);
      load();
    } catch (e) {
      setError(errorText(e));
    }
  }

  /** Whether an unidentified guest voice may use the devices in this room, household-wide. */
  async function handleToggleGuestAccess(room: Room, next: boolean) {
    setGuestAccessBusy(room.id);
    setRooms((prev) => prev.map((r) => (r.id === room.id ? { ...r, guest_accessible: next } : r)));
    try {
      await api.setRoomGuestAccess(room.id, next);
    } catch (e) {
      // Revert: the call failed, so this room's access is whatever it was before.
      setRooms((prev) => prev.map((r) => (r.id === room.id ? { ...r, guest_accessible: !next } : r)));
      setError(errorText(e));
    } finally {
      setGuestAccessBusy(null);
    }
  }

  const sections = groupDevicesByRoom(devices, rooms);

  return (
    <div className="screen screen--devices">
      {/* ── Page header ────────────────────────────────────── */}
      <div className="page-header">
        <div>
          <h1 className="page-header__title">Devices</h1>
          <p className="dev-header__sub">
            {devices.length} device{devices.length === 1 ? "" : "s"} across{" "}
            {rooms.length} room{rooms.length === 1 ? "" : "s"}.
          </p>
        </div>
        <div className="page-header__action">
          <Button size="sm" variant="outline" className="page-header-btn" onPress={openCreateRoom}>
            <Plus size={14} /> New room
          </Button>
          <Button size="sm" variant="primary" className="page-header-btn" onPress={openForm}>
            <Plus size={14} /> Register device
          </Button>
        </div>
      </div>

      {/* ── Matter ────────────────────────────────────────────
          Status only. Matter runs by default and installs its own controller,
          so there is nothing here to switch on — the panel exists to say
          whether pairing will work right now, and to offer the one action worth
          offering when it will not. */}
      <section className="matter-panel">
        <div className="matter-panel__head">
          <Radio size={15} />
          <h2 className="matter-panel__title">Matter</h2>
          <span
            className={`matter-panel__chip matter-panel__chip--${matter?.state ?? "connecting"}`}
            data-testid="matter-state"
          >
            {MATTER_STATE_LABEL[matter?.state ?? "connecting"]}
          </span>
          <span className="matter-panel__spacer" />
          {matter?.state === "unreachable" && (
            <Button size="sm" variant="outline" isDisabled={matterBusy} onPress={() => void retryMatter()}>
              <RefreshCw size={13} /> Retry
            </Button>
          )}
        </div>

        <p className="matter-panel__hint">
          Lights, locks, and sensors paired onto your local fabric. The first
          device you add starts a Matter controller here, which can take a couple
          of minutes; after that it is always ready.
        </p>

        {matter?.state === "unreachable" && matter.error && (
          <p className="text-error text-error--sm">{matter.error}</p>
        )}
        {matterError && <p className="text-error text-error--sm">{matterError}</p>}
      </section>

      {loading && <p className="muted-12">Loading devices…</p>}
      {error   && <ErrorBanner error={error} />}

      {!loading && !error && devices.length === 0 && rooms.length === 0 && (
        <div className="empty-state">
          <Monitor size={32} />
          <span>No devices registered yet.</span>
          <button className="empty-state__cta" onClick={openForm}>
            <Plus size={14} /> Register device
          </button>
        </div>
      )}

      {!loading && (devices.length > 0 || rooms.length > 0) && (
        <>
          {sections.map(({ room, devices: roomDevices }) => (
            <section className="room-section" key={room?.id ?? "unassigned"}>
              <div className="room-section__head">
                <h2 className="room-section__name">{room?.name ?? "Unassigned"}</h2>
                <span className="room-section__count">
                  {roomDevices.length} device{roomDevices.length === 1 ? "" : "s"}
                </span>
                <span className="room-section__spacer" />
                {room && (
                  <>
                    <span className="room-section__guest">
                      <Switch
                        aria-label={`Let a guest use devices in ${room.name}`}
                        isSelected={room.guest_accessible}
                        isDisabled={guestAccessBusy === room.id}
                        onChange={() => handleToggleGuestAccess(room, !room.guest_accessible)}
                      >
                        <Switch.Content><Switch.Control><Switch.Thumb /></Switch.Control></Switch.Content>
                      </Switch>
                      Guest access
                    </span>
                    <div className="room-section__actions">
                      <button
                        className="device-card__action-btn"
                        onClick={() => openRenameRoom(room)}
                        type="button"
                      >
                        <Pencil size={12} /> Rename
                      </button>
                      <button
                        className="device-card__action-btn"
                        onClick={() => handleDeleteRoom(room)}
                        type="button"
                      >
                        <Trash2 size={12} /> Delete
                      </button>
                    </div>
                  </>
                )}
              </div>

              {roomDevices.length === 0 ? (
                <p className="muted-12">No devices in this room yet.</p>
              ) : (
                <div className="devices-grid">
                  {roomDevices.map((d) => (
                    <DeviceCard
                      key={d.id}
                      device={d}
                      isPoweredOn={powerOn[d.id]}
                      busy={busyId === d.id}
                      onPower={handlePower}
                      onConfigure={openDetail}
                    />
                  ))}
                </div>
              )}
            </section>
          ))}
        </>
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
              {/* Only Matter devices are registered here. Phones pair with a
                  pairing code and the desktop app is this app, so a chooser
                  would have had one real option in it. */}
              <div className="sched-modal__field">
                <label className="sched-modal__label">Setup code</label>
                <input
                  className="sched-modal__input"
                  placeholder="20202021 or MT:-24J0AFN00KA0648G00"
                  value={setupCode}
                  onChange={(e) => setSetupCode(e.target.value)}
                  autoFocus
                />
                <p className="sched-modal__cron-hint">
                  The 11-digit pairing code or QR payload on the device, or its
                  8-digit passcode. GIAP commissions it onto your fabric.
                </p>
              </div>

              <div className="sched-modal__field">
                <label className="sched-modal__label">Name <span className="sched-modal__cron-hint">(optional)</span></label>
                <input
                  className="sched-modal__input"
                  placeholder="Living Room Light"
                  value={name}
                  onChange={(e) => setName(e.target.value)}
                />
                <p className="sched-modal__cron-hint">
                  Written to the device so GIAP and other apps use it — say
                  "turn on the living room light". Left blank, the device's own
                  name is used.
                </p>
              </div>

              {/* Commissioning needs a live controller. Say so here rather than
                  letting the user fill the form in and fail on submit. */}
              {matter?.state !== "connected" && (
                <p className="sched-modal__cron-hint" data-testid="matter-not-ready">
                  {matter?.state === "unreachable"
                    ? "The Matter controller cannot be reached. See the Matter section of this tab."
                    : "Matter is still starting up. This will be ready in a moment."}
                </p>
              )}

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
                isDisabled={submitting || !setupCode.trim() || matter?.state !== "connected"}
                onPress={handleCommission}
              >
                {/* Commissioning is slow — say so rather than looking hung. */}
                {submitting ? "Commissioning…" : "Commission"}
              </Button>
            </div>
          </div>
        </div>
      )}

      {/* ── Create / rename room modal ──────────────────────── */}
      {roomModal && (
        <div className="sched-modal__overlay" onClick={closeRoomModal}>
          <div className="sched-modal__dialog" onClick={(e) => e.stopPropagation()}>
            <div className="sched-modal__header">
              <h2 className="sched-modal__title">
                {roomModal.mode === "create" ? "New room" : `Rename "${roomModal.target?.name}"`}
              </h2>
              <button className="sched-modal__close" onClick={closeRoomModal} aria-label="Close">
                <X size={16} />
              </button>
            </div>
            <Separator />
            <div className="sched-modal__body">
              <div className="sched-modal__field">
                <label className="sched-modal__label">Name</label>
                <input
                  className="sched-modal__input"
                  placeholder="Kitchen"
                  value={roomModalName}
                  onChange={(e) => setRoomModalName(e.target.value)}
                  onKeyDown={(e) => e.key === "Enter" && handleSaveRoom()}
                  disabled={roomModalSubmitting}
                  autoFocus
                />
              </div>
              {roomModalError && (
                <p className="text-error text-error--sm">{roomModalError}</p>
              )}
            </div>
            <Separator />
            <div className="sched-modal__footer">
              <Button size="sm" variant="ghost" onPress={closeRoomModal}>Cancel</Button>
              <Button
                size="sm"
                variant="primary"
                isDisabled={roomModalSubmitting || !roomModalName.trim()}
                onPress={handleSaveRoom}
              >
                {roomModalSubmitting ? "Saving…" : "Save"}
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
                <label className="sched-modal__label">Room</label>
                <select
                  className="sched-modal__select"
                  style={{ width: "100%" }}
                  value={editRoomId}
                  onChange={(e) => setEditRoomId(e.target.value)}
                  disabled={editSubmitting}
                >
                  <option value="">Unassigned</option>
                  {rooms.map((r) => (
                    <option key={r.id} value={r.id}>{r.name}</option>
                  ))}
                </select>
              </div>
              <div className="sched-modal__field">
                <label className="sched-modal__label">Type</label>
                <div className="muted-12">{detail.device_type ?? "—"}</div>
              </div>
              <div className="sched-modal__field">
                <label className="sched-modal__label">Status</label>
                <div className="muted-12">
                  {detail.is_online ? "online" : "offline"} · last seen {lastSeen(detail)}
                </div>
                {/* Reachability, which is not the device's power. It lived on the
                    card labelled "Turn on"/"Turn off" and wrote nothing but
                    `last_seen`, so switching a lamp "off" made GIAP forget the lamp.
                    Still worth being able to set by hand -- named for what it is. */}
                <button
                  className="device-card__action-btn"
                  onClick={() => handleReachability(detail)}
                  disabled={busyId === detail.id}
                  type="button"
                >
                  <Radio size={12} /> {detail.is_online ? "Mark offline" : "Mark online"}
                </button>
                <p className="sched-modal__cron-hint">
                  Whether GIAP believes it can reach the device, not whether the
                  device is switched on.
                </p>
              </div>
              <div className="sched-modal__field">
                <label className="sched-modal__label">Address</label>
                <code className="device-card__ip">
                  {detail.ip_address ?? "—"}
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
