// The navigation drawer both shells share. Every section stays one tap from every screen, and
// rows are at least 44px, chips 34px: the finger is the assumed pointer.

import { useEffect, useState } from "react";
import { Logo } from "../components/Logo";
import { HubIco } from "./primitives/HubIco";
import { HP_PATHS, micEl } from "./primitives/icons";
import { useHomeData, useRoutines } from "./state/hubDataStore";
import type { GuiSection } from "../desktopState";
import "./hubDrawer.css";

/** A drawer row's target; voice is a mode each shell reaches its own way, so it's its own case. */
export type DrawerNav =
  | { kind: "section"; section: GuiSection }
  | { kind: "voice" };

interface NavRow {
  label: string;
  icon: string;
  section: GuiSection;
}

/** Voice takes `micEl`: `HP_PATHS.mic` is a compound-icon sentinel and draws nothing as a path. */
const POND_ROWS: Array<{ label: string; icon: string | React.ReactNode; nav: DrawerNav }> = [
  { label: "Chat",  icon: HP_PATHS.chat, nav: { kind: "section", section: "chat" } },
  { label: "Voice", icon: micEl,         nav: { kind: "voice" } },
];

/** The design's order (most-touched first), not alphabetical; keep it in step with the design. */
const MANAGE_ROWS: Array<{ label: string; section: GuiSection }> = [
  { label: "Devices",    section: "devices" },
  { label: "Mesh",       section: "mesh" },
  { label: "Pairing",    section: "pairing" },
  { label: "Schedules",  section: "schedules" },
  { label: "Context",    section: "context" },
  { label: "Skills",     section: "skills" },
  { label: "Recipes",    section: "recipes" },
  { label: "Logs",       section: "logs" },
  { label: "Models",     section: "models" },
  { label: "Prompts",    section: "prompts" },
  { label: "Extensions", section: "extensions" },
  { label: "Household",  section: "household" },
  { label: "Faces",      section: "faces" },
];

const HOME_ROW: NavRow     = { label: "Home",     icon: HP_PATHS.home,         section: "dashboard" };
const SETTINGS_ROW: NavRow = { label: "Settings", icon: HP_PATHS.railSettings, section: "settings" };

/** The design's Manage glyph: three rules with a knob on the last. */
const manageEl = (
  <>
    <path d="M4 6h16M4 12h16M4 18h10" />
    <circle cx="17" cy="18" r="2.4" />
  </>
);

export interface HubDrawerProps {
  open: boolean;
  onClose: () => void;
  /** The section the shell is showing, so the drawer can mark it. */
  active: GuiSection;
  onNavigate: (nav: DrawerNav) => void;
}

export function HubDrawer({ open, onClose, active, onNavigate }: HubDrawerProps) {
  // Manage (twelve chips) starts closed so Rooms and routines stay above the fold.
  const [pondOpen, setPondOpen]     = useState(true);
  const [manageOpen, setManageOpen] = useState(false);

  // Rooms derive from devices, so `devicesAreReal` gates them too.
  const { rooms, devicesAreReal } = useHomeData();
  // Routines need no flag: they are the household's recipes or nothing.
  const routines  = useRoutines();

  useEffect(() => {
    if (!open) return;
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") onClose();
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [open, onClose]);

  if (!open) return null;

  function go(nav: DrawerNav) {
    onNavigate(nav);
    onClose();
  }

  const manageHoldsActive = MANAGE_ROWS.some((r) => r.section === active);

  return (
    <>
      {/* Decorative: the close button and Escape are the accessible ways out,
          so a second "Close menu" control in the tree would only be a duplicate
          name for the same action. */}
      <div className="hdrawer__scrim" onClick={onClose} aria-hidden="true" />
      <div className="hdrawer" role="dialog" aria-modal="true" aria-label="Menu">
        <div className="hdrawer__head">
          <span className="hdrawer__brand">
            <Logo size={48} alt="" />
            <span className="hdrawer__brand-name">Goose</span>
          </span>
          <button
            type="button"
            className="hdrawer__close"
            onClick={onClose}
            aria-label="Close menu"
          >
            <HubIco d={HP_PATHS.x} size={14} color="var(--color-text)" sw={2.4} />
          </button>
        </div>

        <div className="hdrawer__body">
          <p className="hdrawer__label" id="hdrawer-goto">Go to</p>
          <nav className="hdrawer__list" aria-labelledby="hdrawer-goto">
            <DrawerRow
              row={HOME_ROW}
              active={active === HOME_ROW.section}
              onClick={() => go({ kind: "section", section: HOME_ROW.section })}
            />

            <DrawerGroup
              label="Pond"
              icon={HP_PATHS.goose}
              open={pondOpen}
              holdsActive={active === "chat"}
              onToggle={() => setPondOpen((v) => !v)}
            >
              <div className="hdrawer__sub">
                {POND_ROWS.map((r) => (
                  <button
                    key={r.label}
                    type="button"
                    className="hdrawer__subitem"
                    onClick={() => go(r.nav)}
                  >
                    <HubIco d={r.icon} size={17} color="var(--color-text-tertiary)" sw={2} />
                    {r.label}
                  </button>
                ))}
              </div>
            </DrawerGroup>

            <DrawerGroup
              label="Manage"
              icon={manageEl}
              open={manageOpen}
              holdsActive={manageHoldsActive}
              onToggle={() => setManageOpen((v) => !v)}
            >
              <div className="hdrawer__chips hdrawer__chips--indent">
                {MANAGE_ROWS.map((r) => (
                  <button
                    key={r.section}
                    type="button"
                    className="hdrawer__chip"
                    data-active={active === r.section}
                    onClick={() => go({ kind: "section", section: r.section })}
                  >
                    {r.label}
                  </button>
                ))}
              </div>
            </DrawerGroup>

            <DrawerRow
              row={SETTINGS_ROW}
              active={active === SETTINGS_ROW.section}
              onClick={() => go({ kind: "section", section: SETTINGS_ROW.section })}
            />
          </nav>

          {devicesAreReal && rooms.length > 0 && (
            <>
              <p className="hdrawer__label hdrawer__label--spaced" id="hdrawer-rooms">Rooms</p>
              <nav className="hdrawer__chips" aria-labelledby="hdrawer-rooms">
                {rooms.map((r) => (
                  <button
                    key={r.id}
                    type="button"
                    className="hdrawer__room"
                    onClick={() => go({ kind: "section", section: "dashboard" })}
                  >
                    {r.name}
                  </button>
                ))}
              </nav>
            </>
          )}

          {routines.length > 0 && (
            <>
              <p className="hdrawer__label hdrawer__label--spaced" id="hdrawer-routines">Quick routines</p>
              <nav className="hdrawer__routines" aria-labelledby="hdrawer-routines">
                {routines.map((r) => (
                  <button
                    key={r.id}
                    type="button"
                    className="hdrawer__routine"
                    onClick={() => go({ kind: "section", section: "schedules" })}
                  >
                    <HubIco d={r.iconPath} size={18} color="var(--color-text)" sw={1.9} />
                    <span className="hdrawer__routine-name">{r.name}</span>
                    <span className="hdrawer__routine-run">Open</span>
                  </button>
                ))}
              </nav>
            </>
          )}
        </div>
      </div>
    </>
  );
}

function DrawerRow({
  row,
  active,
  onClick,
}: {
  row: NavRow;
  active: boolean;
  onClick: () => void;
}) {
  return (
    <button
      type="button"
      className="hdrawer__item"
      data-active={active}
      aria-current={active ? "page" : undefined}
      onClick={onClick}
    >
      <HubIco
        d={row.icon}
        size={20}
        color={active ? "var(--pp)" : "var(--color-text-tertiary)"}
        sw={2}
      />
      {row.label}
    </button>
  );
}

function DrawerGroup({
  label,
  icon,
  open,
  holdsActive,
  onToggle,
  children,
}: {
  label: string;
  icon: string | React.ReactNode;
  open: boolean;
  /** True when the section being shown lives inside this group. */
  holdsActive: boolean;
  onToggle: () => void;
  children: React.ReactNode;
}) {
  return (
    <>
      <button
        type="button"
        className="hdrawer__item hdrawer__item--group"
        onClick={onToggle}
        aria-expanded={open}
      >
        <HubIco
          d={icon}
          size={20}
          color={holdsActive ? "var(--pp)" : "var(--color-text-tertiary)"}
          sw={2}
        />
        <span className="hdrawer__group-label">{label}</span>
        <HubIco
          d={HP_PATHS.chevD}
          size={14}
          color="var(--color-text-tertiary)"
          sw={2.4}
          className={`hdrawer__caret${open ? " is-open" : ""}`}
        />
      </button>
      {open && children}
    </>
  );
}
