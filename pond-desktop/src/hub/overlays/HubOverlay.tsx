import { useEffect, useState } from "react";
import { Switch } from "../../components/Switch";
import { HubIco } from "../primitives/HubIco";
import { HP_PATHS, SUBTYPE_ICON, lockEl, unlockEl, micEl } from "../primitives/icons";
import { HubModal as Modal } from "../primitives/HubModal";
import { CameraFeed } from "../primitives/CameraFeed";
import { type DeviceData, type DeviceKind } from "../data/mockHome";
import { useHomeData } from "../state/hubDataStore";
import { useDeviceState, hubControlDevice } from "../state/hubStore";

// ─── Light control ────────────────────────────────────────────
const TEMPS: Array<{ k: string; c: string; label: string }> = [
  { k: "warm",    c: "#F9A03F", label: "Warm" },
  { k: "soft",    c: "#FBC78A", label: "Soft" },
  { k: "neutral", c: "#FDE9C8", label: "Neutral" },
  { k: "cool",    c: "#D6E6FF", label: "Cool" },
];

function LightControl({ device }: { device: DeviceData }) {
  const [st, set, control] = useDeviceState(device.id);
  return (
    <>
      <div
        className="dc-hero"
        style={{ background: st.on ? "linear-gradient(150deg,#FBBF24,#F59E0B)" : "var(--hub-bg)" }}
      >
        <HubIco
          d={st.on ? HP_PATHS.bulb : HP_PATHS.bulbOff}
          size={42}
          color={st.on ? "#fff" : "#CBD5E1"}
          sw={1.8}
        />
        <span className="dc-hero__val" style={{ color: st.on ? "#fff" : "var(--mut)" }}>
          {st.on ? `${st.brightness}%` : "Off"}
        </span>
      </div>

      <div className="dc-block">
        <div className="dc-block__label">Brightness</div>
        <div className="dc-slider">
          <HubIco d={HP_PATHS.bulbOff} size={16} color="var(--mut)" />
          <input
            type="range"
            min={0}
            max={100}
            value={st.brightness}
            onChange={(e) => {
              const b = Number(e.target.value);
              control({ brightness: b, on: b > 0 });
            }}
            style={{
              background: `linear-gradient(90deg,#F59E0B ${st.brightness}%,var(--line) ${st.brightness}%)`,
            }}
          />
          <HubIco d={HP_PATHS.bulb} size={18} color="var(--mut)" />
        </div>
      </div>

      <div className="dc-block">
        <div className="dc-block__label">Color temperature</div>
        <div className="dc-temps">
          {TEMPS.map((t) => (
            <button
              key={t.k}
              className="dc-temp"
              data-active={st.temp === t.k}
              onClick={() => set({ temp: t.k })}
              type="button"
            >
              <span className="dc-temp__dot" style={{ background: t.c }}>
                {st.temp === t.k && (
                  <HubIco d={HP_PATHS.check} size={13} color="#7C3AED" sw={3} />
                )}
              </span>
              <span>{t.label}</span>
            </button>
          ))}
        </div>
      </div>
    </>
  );
}

// ─── Thermostat control ───────────────────────────────────────
function ThermostatControl({ device }: { device: DeviceData }) {
  const [st, set, control] = useDeviceState(device.id);
  const target = st.target ?? 70;
  const pct = Math.max(0, Math.min(100, ((target - 60) / 20) * 100));
  const bump = (d: number) => control({ target: Math.max(60, Math.min(80, target + d)) });
  return (
    <>
      <div className="thermo-dial">
        <button className="thermo-step" onClick={() => bump(-1)} type="button" aria-label="Decrease">
          <HubIco d={HP_PATHS.minus} size={22} color="var(--ink)" />
        </button>
        <div className="thermo-ring">
          <svg viewBox="0 0 200 200" width={190} height={190}>
            <circle cx={100} cy={100} r={86} fill="none" stroke="var(--line)" strokeWidth={14} />
            <circle
              cx={100}
              cy={100}
              r={86}
              fill="none"
              stroke="#EA580C"
              strokeWidth={14}
              strokeLinecap="round"
              pathLength={100}
              strokeDasharray={`${pct} 100`}
              transform="rotate(-90 100 100)"
            />
          </svg>
          <div className="thermo-ring__center">
            <span className="thermo-ring__target">
              {target}<span>°</span>
            </span>
            <span className="thermo-ring__mode">
              {st.mode} · now {st.cur}°
            </span>
          </div>
        </div>
        <button className="thermo-step" onClick={() => bump(1)} type="button" aria-label="Increase">
          <HubIco d={HP_PATHS.plus} size={22} color="var(--ink)" />
        </button>
      </div>

      <div className="dc-block">
        <div className="dc-block__label">Mode</div>
        <div className="dc-modes">
          {["Heat", "Cool", "Auto", "Eco"].map((m) => (
            <button
              key={m}
              className="dc-mode"
              data-active={st.mode === m}
              onClick={() => set({ mode: m })}
              type="button"
            >
              {m}
            </button>
          ))}
        </div>
      </div>

      <div className="dc-stats">
        <div className="dc-stat">
          <HubIco d={HP_PATHS.droplet} size={15} color="#3B82F6" />
          <span>62%</span><small>Humidity</small>
        </div>
        <div className="dc-stat">
          <HubIco d={HP_PATHS.flame} size={15} color="#EA580C" />
          <span>{st.cur}°</span><small>Current</small>
        </div>
        <div className="dc-stat">
          <HubIco d={HP_PATHS.bolt} size={15} color="#16A34A" />
          <span>Eco</span><small>Energy</small>
        </div>
      </div>
    </>
  );
}

// ─── Lock control ─────────────────────────────────────────────
const LOCK_HISTORY: Array<{ who: string; act: string; t: string }> = [
  { who: "You",       act: "Unlocked", t: "8:12 AM" },
  { who: "Auto-lock", act: "Locked",   t: "8:32 AM" },
  { who: "Goose",     act: "Locked",   t: "Last night 11:00 PM" },
];

function LockControl({ device }: { device: DeviceData }) {
  const [st, , control] = useDeviceState(device.id);
  const locked = st.locked;
  return (
    <>
      <div
        className="lock-hero"
        style={{
          background: locked
            ? "linear-gradient(150deg,#3B82F6,#2563EB)"
            : "linear-gradient(150deg,#F59E0B,#D97706)",
        }}
      >
        <HubIco d={locked ? lockEl : unlockEl} size={46} color="#fff" sw={1.7} />
        <span className="lock-hero__state">{locked ? "Locked" : "Unlocked"}</span>
      </div>
      <button
        className="lock-action"
        data-locked={locked}
        onClick={() => control({ locked: !locked })}
        type="button"
      >
        <HubIco d={locked ? unlockEl : lockEl} size={18} color="#fff" />{" "}
        {locked ? "Unlock door" : "Lock door"}
      </button>
      <div className="dc-block">
        <div className="dc-block__label">Recent access</div>
        <div className="lock-hist">
          {LOCK_HISTORY.map((h, i) => (
            <div key={i} className="lock-histrow">
              <span className="lock-histrow__icon">
                <HubIco
                  d={h.who === "Goose" ? HP_PATHS.goose : HP_PATHS.person}
                  size={14}
                  color="#7C3AED"
                />
              </span>
              <span className="lock-histrow__who">
                <strong>{h.who}</strong> · {h.act}
              </span>
              <span className="lock-histrow__t">{h.t}</span>
            </div>
          ))}
        </div>
      </div>
    </>
  );
}

// ─── Plug control ─────────────────────────────────────────────
function PlugControl({ device }: { device: DeviceData }) {
  const [st] = useDeviceState(device.id);
  const bars = [30, 42, 38, 55, 48, 60, 42];
  return (
    <>
      <div
        className="dc-hero"
        style={{ background: st.on ? "linear-gradient(150deg,#2DD4BF,#0D9488)" : "var(--hub-bg)" }}
      >
        <HubIco
          d={HP_PATHS.plug}
          size={40}
          color={st.on ? "#fff" : "#CBD5E1"}
          sw={1.8}
        />
        <span className="dc-hero__val" style={{ color: st.on ? "#fff" : "var(--mut)" }}>
          {st.on ? `${st.watts} W` : "Off"}
        </span>
      </div>

      <div className="dc-stats">
        <div className="dc-stat"><span>{st.on ? st.watts : 0} W</span><small>Now</small></div>
        <div className="dc-stat"><span>0.8 kWh</span><small>Today</small></div>
        <div className="dc-stat"><span>$0.11</span><small>Cost today</small></div>
      </div>

      <div className="dc-block">
        <div className="dc-block__label">Last 7 hours</div>
        <div className="plug-chart">
          {bars.map((b, i) => (
            <span key={i} style={{ height: `${st.on ? b : 4}%` }} />
          ))}
        </div>
      </div>
    </>
  );
}

// ─── Device control modal ─────────────────────────────────────
const KIND_TINT: Record<DeviceKind, string>   = { light: "#F59E0B", lock: "#2563EB", thermo: "#EA580C", plug: "#0D9488", other: "#71717A" };
const KIND_TINTBG: Record<DeviceKind, string> = { light: "#FEF3C7", lock: "#DBEAFE", thermo: "#FFEDD5", plug: "#CCFBF1", other: "#F4F4F5" };
const KIND_ICON: Record<DeviceKind, string>   = { light: HP_PATHS.bulb, lock: "", thermo: HP_PATHS.flame, plug: HP_PATHS.plug, other: HP_PATHS.sliders };

interface DeviceControlProps {
  deviceId: string;
  onClose: () => void;
}

function DeviceControl({ deviceId, onClose }: DeviceControlProps) {
  const home = useHomeData();
  const device = home.devices.find((d) => d.id === deviceId);
  const [st, , control] = useDeviceState(deviceId);
  if (!device) return null;
  const k = device.kind;
  const on = k === "lock" ? st.locked : (k === "thermo" ? true : st.on);
  const showSwitch = k === "light" || k === "plug";
  const toggle = () => control(k === "lock" ? { locked: !st.locked } : { on: !st.on });
  const tint   = KIND_TINT[k];
  const tintBg = KIND_TINTBG[k];

  return (
    <Modal label={`${device.name} controls`} onClose={onClose}>
      <div className="dc-head">
        <span className="dc-head__icon" style={{ background: tintBg, color: tint }}>
          {k === "lock"
            ? <HubIco d={lockEl} size={20} color={tint} />
            : <HubIco d={(k === "other" && device.subtype && SUBTYPE_ICON[device.subtype]) || KIND_ICON[k]} size={20} color={tint} />}
        </span>
        <div className="dc-head__text">
          <span className="dc-head__name">{device.name}</span>
          <span className="dc-head__room">{device.room}</span>
        </div>
        {showSwitch && (
          <Switch
            checked={on}
            onChange={() => toggle()}
            style={{ marginRight: 36 }}
            label={`Toggle ${device.name}`}
          />
        )}
      </div>
      <div className="dc-body">
        {k === "light"  && <LightControl       device={device} />}
        {k === "thermo" && <ThermostatControl  device={device} />}
        {k === "lock"   && <LockControl        device={device} />}
        {k === "plug"   && <PlugControl        device={device} />}
      </div>
    </Modal>
  );
}

// ─── Camera modal ─────────────────────────────────────────────
interface CameraModalProps {
  camId: string;
  onClose: () => void;
}

function CameraModal({ camId, onClose }: CameraModalProps) {
  const home = useHomeData();
  const [cur, setCur] = useState(camId);
  const [muted, setMuted] = useState(false);
  const cam = home.cameras.find((c) => c.id === cur) ?? home.cameras[0];
  if (!cam) return null;
  const events = [
    { t: "8:48 AM", label: "Motion", icon: HP_PATHS.person },
    { t: "8:12 AM", label: "Person", icon: HP_PATHS.person },
    { t: "7:30 AM", label: "Motion", icon: HP_PATHS.bolt },
  ];
  return (
    <Modal label={`${cam.name} camera`} onClose={onClose} wide>
      <div className="camm">
        <div className="camm__feed">
          <CameraFeed cam={cam} interactive={false} />
        </div>
        <div className="camm__side">
          <div className="camm__title">{cam.name}</div>
          <div className="camm__sub">Live · on-device recording</div>
          <div className="camm__ctrls">
            <button className="camm__btn" type="button">
              <HubIco d={HP_PATHS.cam} size={18} color="#7C3AED" /><span>Snapshot</span>
            </button>
            <button className="camm__btn" type="button">
              <HubIco d={micEl} size={18} color="#7C3AED" /><span>Talk</span>
            </button>
            <button className="camm__btn" type="button" onClick={() => setMuted((m) => !m)}>
              <HubIco d={muted ? HP_PATHS.micOff : HP_PATHS.speaker} size={18} color="#7C3AED" />
              <span>{muted ? "Muted" : "Sound"}</span>
            </button>
            <button className="camm__btn" type="button">
              <HubIco d={HP_PATHS.max} size={18} color="#7C3AED" /><span>Full</span>
            </button>
          </div>
          <div className="camm__events-label">Recent events</div>
          <div className="camm__events">
            {events.map((e, i) => (
              <div key={i} className="camm__event">
                <span className="camm__event-ic">
                  <HubIco d={e.icon} size={13} color="#7C3AED" />
                </span>
                <span className="camm__event-label">{e.label}</span>
                <span className="camm__event-t">{e.t}</span>
              </div>
            ))}
          </div>
          <div className="camm__switch">
            {home.cameras.map((c) => (
              <button
                key={c.id}
                className="camm__thumb"
                data-active={c.id === cur}
                onClick={() => setCur(c.id)}
                type="button"
                style={{
                  background: `linear-gradient(${c.hue + 20}deg, hsl(${c.hue},35%,42%), hsl(${c.hue + 30},40%,28%))`,
                }}
              >
                <span>{c.name}</span>
              </button>
            ))}
          </div>
        </div>
      </div>
    </Modal>
  );
}

// ─── Category device row ──────────────────────────────────────
const CAT_ROW_TINT: Record<DeviceKind, string> = KIND_TINT;

function catIconFor(kind: DeviceKind, on: boolean): string | React.ReactNode {
  if (kind === "light")  return on ? HP_PATHS.bulb : HP_PATHS.bulbOff;
  if (kind === "lock")   return on ? lockEl : unlockEl;
  if (kind === "thermo") return HP_PATHS.flame;
  return HP_PATHS.plug;
}

function CatRow({ device }: { device: DeviceData }) {
  const [st, , control] = useDeviceState(device.id);
  const k = device.kind;
  const on = k === "lock" ? st.locked : st.on;
  const tint = CAT_ROW_TINT[k];
  const icon = catIconFor(k, on);
  const status =
    k === "lock"   ? (on ? "Locked" : "Unlocked") :
    k === "thermo" ? `${st.mode} ${st.target}°` :
                     (on ? "On" : "Off");
  return (
    <div className="catrow">
      <span className="catrow__icon" style={{ background: on ? tint : "var(--hub-bg)" }}>
        <HubIco d={icon} size={16} color={on ? "#fff" : "var(--mut)"} />
      </span>
      <div className="catrow__text">
        <span className="catrow__name">{device.name}</span>
        <span className="catrow__sub">{device.room} · {status}</span>
      </div>
      {k === "thermo" ? (
        <div className="catrow__therm">
          <button
            type="button"
            onClick={() => control({ target: Math.max(60, (st.target ?? 70) - 1) })}
            aria-label="Decrease"
          >
            <HubIco d={HP_PATHS.minus} size={15} color="var(--ink)" />
          </button>
          <span>{st.target ?? 70}°</span>
          <button
            type="button"
            onClick={() => control({ target: Math.min(80, (st.target ?? 70) + 1) })}
            aria-label="Increase"
          >
            <HubIco d={HP_PATHS.plus} size={15} color="var(--ink)" />
          </button>
        </div>
      ) : (
        <Switch
          checked={on}
          label={`Toggle ${device.name}`}
          onChange={() => control(k === "lock" ? { locked: !st.locked } : { on: !st.on })}
        />
      )}
    </div>
  );
}

// ─── Category sheet ───────────────────────────────────────────
interface CategorySheetProps {
  catId: string;
  onClose: () => void;
}

const CAT_KIND: Record<string, DeviceKind | undefined> = {
  lights: "light",
  locks:  "lock",
  climate: "thermo",
  plugs:  "plug",
};

function CategorySheet({ catId, onClose }: CategorySheetProps) {
  const home = useHomeData();
  const cat = home.categories.find((c) => c.id === catId);
  const [armed, setArmed] = useState(false);
  if (!cat) return null;

  const kindFor = CAT_KIND[catId];
  const devices = kindFor ? home.devices.filter((d) => d.kind === kindFor) : [];

  function allOn(val: boolean) {
    devices.forEach((d) => {
      void hubControlDevice(d.id, kindFor === "lock" ? { locked: val } : { on: val });
    });
  }

  const iconPath = HP_PATHS[cat.icon as keyof typeof HP_PATHS];

  return (
    <Modal label={cat.label} onClose={onClose}>
      <div className="dc-head">
        <span className="dc-head__icon" style={{ background: cat.bg, color: cat.color }}>
          {iconPath && <HubIco d={iconPath} size={20} color={cat.color} />}
        </span>
        <div className="dc-head__text">
          <span className="dc-head__name">{cat.label}</span>
          <span className="dc-head__room">{cat.status}</span>
        </div>
      </div>

      <div className="dc-body">
        {catId === "security" && (
          <>
            <div
              className="lock-hero"
              style={{
                background: armed
                  ? "linear-gradient(150deg,#16A34A,#15803D)"
                  : "var(--hub-bg)",
              }}
            >
              <HubIco
                d={HP_PATHS.shieldCheck}
                size={44}
                color={armed ? "#fff" : "#CBD5E1"}
                sw={1.7}
              />
              <span
                className="lock-hero__state"
                style={{ color: armed ? "#fff" : "var(--mut)" }}
              >
                {armed ? "Armed · Home" : "Disarmed"}
              </span>
            </div>
            <button
              className="lock-action"
              data-locked={!armed}
              onClick={() => setArmed((a) => !a)}
              type="button"
              style={{ background: armed ? "#64748B" : "#16A34A" }}
            >
              <HubIco d={HP_PATHS.shieldCheck} size={18} color="#fff" />{" "}
              {armed ? "Disarm system" : "Arm system"}
            </button>
            <div className="dc-block">
              <div className="dc-block__label">Sensors</div>
              <div className="catlist">
                {[
                  ["Front door",    "Closed"],
                  ["Garage",        "Closed"],
                  ["Motion · Hall", "Clear"],
                  ["Leak · Kitchen","Dry"],
                ].map(([n, s]) => (
                  <div key={n} className="catrow">
                    <span className="catrow__icon" style={{ background: "#DCFCE7" }}>
                      <HubIco d={HP_PATHS.check} size={15} color="#16A34A" sw={3} />
                    </span>
                    <div className="catrow__text">
                      <span className="catrow__name">{n}</span>
                    </div>
                    <span className="catrow__ok">{s}</span>
                  </div>
                ))}
              </div>
            </div>
          </>
        )}

        {catId === "cameras" && (
          <div className="cat-camgrid">
            {home.cameras.map((c) => (
              <div
                key={c.id}
                onClick={() =>
                  window.dispatchEvent(new CustomEvent("hub:camera", { detail: c.id }))
                }
                style={{ cursor: "pointer" }}
              >
                <CameraFeed cam={c} interactive={false} />
              </div>
            ))}
          </div>
        )}

        {kindFor && (
          <>
            {(catId === "lights" || catId === "locks") && (
              <div className="cat-master">
                <button type="button" onClick={() => allOn(true)}>
                  {catId === "locks" ? "Lock all" : "All on"}
                </button>
                {catId === "lights" && (
                  <button type="button" onClick={() => allOn(false)}>All off</button>
                )}
              </div>
            )}
            <div className="catlist">
              {devices.map((d) => <CatRow key={d.id} device={d} />)}
            </div>
          </>
        )}
      </div>
    </Modal>
  );
}

// ─── Overlay host ─────────────────────────────────────────────
type OverlayState =
  | { type: "device";   id: string }
  | { type: "camera";   id: string }
  | { type: "category"; id: string }
  | null;

export function HubOverlay() {
  const [ov, setOv] = useState<OverlayState>(null);

  useEffect(() => {
    const onDevice   = (e: Event) => setOv({ type: "device",   id: (e as CustomEvent<string>).detail });
    const onCamera   = (e: Event) => setOv({ type: "camera",   id: (e as CustomEvent<string>).detail });
    const onCategory = (e: Event) => setOv({ type: "category", id: (e as CustomEvent<string>).detail });
    window.addEventListener("hub:device",   onDevice);
    window.addEventListener("hub:camera",   onCamera);
    window.addEventListener("hub:category", onCategory);
    return () => {
      window.removeEventListener("hub:device",   onDevice);
      window.removeEventListener("hub:camera",   onCamera);
      window.removeEventListener("hub:category", onCategory);
    };
  }, []);

  if (!ov) return null;
  const close = () => setOv(null);
  if (ov.type === "device")   return <DeviceControl deviceId={ov.id} onClose={close} />;
  if (ov.type === "camera")   return <CameraModal   camId={ov.id}    onClose={close} />;
  if (ov.type === "category") return <CategorySheet catId={ov.id}    onClose={close} />;
  return null;
}
