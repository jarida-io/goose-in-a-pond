import React from "react";
import { HubIco, lockEl, unlockEl, dotsEl } from "./HubIco";
import { HP_PATHS } from "./icons";
import { useDeviceState } from "../state/hubStore";
import type { DeviceData } from "../data/mockHome";

interface DeviceTileProps {
  device: DeviceData;
  size?: "md" | "lg";
}

/** Icons for non-smart-home devices, keyed by the Devices section's register types. */
export const SUBTYPE_ICON: Record<string, string> = {
  host: HP_PATHS.cpu,
  sensor: HP_PATHS.pulse,
  gotg: HP_PATHS.phone,
  smart_speaker: HP_PATHS.speaker,
  pond: HP_PATHS.goose,
  edge: HP_PATHS.chip,
};

const SUBTYPE_LABEL: Record<string, string> = {
  host: "Host / PC",
  sensor: "Sensor",
  gotg: "Mobile (GOTG)",
  smart_speaker: "Smart speaker",
  pond: "Pond instance",
  edge: "Edge device",
};

export function DeviceTile({ device, size = "md" }: DeviceTileProps) {
  const [st, , control] = useDeviceState(device.id);
  const k = device.kind;
  const { on, locked, target = 70 } = st;

  // One look for every kind; state shows via `data-active` (see hub.css) and the accent icon.
  let active = false;
  let bg = "var(--tile-bg,#fff)";
  let fg = "var(--tile-fg,#18181B)";
  let sub = "var(--tile-sub,#566178)";
  let iconEl: string | React.ReactNode;
  let statusText = "";
  let accentIcon = "var(--tile-icon,#CBD5E1)";
  const border = active ? undefined : "1px solid var(--tile-border,#ECECF1)";

  if (k === "light") {
    active = on;
    if (on) {
      accentIcon = "var(--pp)";
    }
    iconEl = on ? HP_PATHS.bulb : HP_PATHS.bulbOff;
    statusText = on ? `On · ${st.brightness}%` : "Off";
  } else if (k === "lock") {
    active = locked;
    if (locked) {
      accentIcon = "var(--pp)";
    }
    iconEl = locked ? lockEl : unlockEl;
    statusText = locked ? "Locked" : "Unlocked";
  } else if (k === "thermo") {
    active = true;
    accentIcon = "var(--pp)";
    iconEl = HP_PATHS.flame;
    statusText = `${st.mode} to ${target}°`;
  } else if (k === "plug") {
    active = on;
    if (on) {
      accentIcon = "var(--pp)";
    }
    iconEl = HP_PATHS.plug;
    statusText = on ? `On · ${st.watts}W` : "Off";
  } else if (k === "other") {
    iconEl = (device.subtype && SUBTYPE_ICON[device.subtype]) || HP_PATHS.sliders;
    statusText = (device.subtype && SUBTYPE_LABEL[device.subtype]) || "Device";
  }

  function handle() {
    if (k === "light" || k === "plug") void control({ on: !on });
    else if (k === "lock") void control({ locked: !locked });
    else if (k === "thermo") void control({ target: target >= 74 ? 66 : target + 1 });
  }

  function openCtrl() {
    window.dispatchEvent(new CustomEvent("hub:device", { detail: device.id }));
  }

  return (
    // The "..." button can't nest in the tile button (a11y); .dtile-wrap overlays it as a sibling.
    <div className="dtile-wrap">
      <button
        className="dtile"
        onClick={handle}
        data-active={active}
        style={{
          background: bg,
          color: fg,
          ...(active ? {} : { border }),
        }}
      >
        <div className="dtile__top">
          <span className="dtile__name" style={{ color: fg }}>{device.name}</span>
        </div>
        {k === "thermo" ? (
          <div className="dtile__temp">
            {st.cur ?? device.value}<span>°</span>
          </div>
        ) : <div style={{ flex: 1 }} />}
        <div className="dtile__bottom">
          <span
            className="dtile__icon"
            style={{ background: active ? "rgba(255,255,255,.22)" : "var(--tile-iconbg,#F4F4F7)" }}
          >
            <HubIco d={iconEl ?? ""} size={size === "lg" ? 20 : 18} color={accentIcon} sw={2} />
          </span>
          <span className="dtile__status" style={{ color: sub }}>{statusText}</span>
        </div>
      </button>
      <button
        className="dtile__dots"
        onClick={openCtrl}
        aria-label={`${device.name} controls`}
        style={{ color: active ? "rgba(255,255,255,.7)" : "var(--tile-icon,#566178)" }}
      >
        <HubIco d={dotsEl} size={16} />
      </button>
    </div>
  );
}
