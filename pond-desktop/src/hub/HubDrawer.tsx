// ────────────────────────────────────────────────────────────
// The drawer that replaced the sidebar.
//
// Both shells used to carry their own navigation, and neither was small: the
// classic shell a 240px column of fourteen items in three labelled groups, the
// hub an 86px icon rail of five. The redesign replaces both with one drawer
// behind a hamburger, and the reason is the panel it targets. At 1024x600 a
// persistent column is a fifth of the screen spent saying where you are not.
//
// Three things the shape buys, none of them cosmetic:
//
//   ONE NAV    The twelve sections the classic sidebar kept under MANAGE and
//              CONFIGURE became one destination, "Manage". The design's own
//              note is the authority: "the two groups became one destination".
//   REACHABLE  Nothing was dropped. Every section the fat sidebar listed is
//              still one tap from every screen, which a five-item rail could
//              not say -- it left nine of them with no route at all.
//   TOUCH      Every row is at least 44px and every chip 34px, because the
//              finger is the pointer this design assumes.
//
// Deliberately NOT here: the rest of the Home redesign -- the paged widgets,
// the suggestion queue, the clock and weather cluster. This is the navigation
// and nothing else.
// ────────────────────────────────────────────────────────────

import { useEffect, useState } from "react";
import { Logo } from "../components/Logo";
import { HubIco, micEl } from "./primitives/HubIco";
import { HP_PATHS } from "./primitives/icons";
import { useHomeData, useRoutines } from "./state/hubDataStore";
import type { GuiSection } from "../desktopState";
import "./hubDrawer.css";

/**
 * Where a drawer row sends you.
 *
 * Voice is a mode, not a section, and the two shells reach it differently --
 * so it is its own case rather than a magic section name that one shell would
 * silently fail to recognise.
 */
export type DrawerNav =
  | { kind: "section"; section: GuiSection }
  | { kind: "voice" };

interface NavRow {
  label: string;
  icon: string;
  section: GuiSection;
}

/**
 * "Pond" -- the two ways to talk to it.
 *
 * Voice takes `micEl`, not `HP_PATHS.mic`: that entry is a sentinel string for
 * a compound icon, so rendering it as a path draws nothing at all. It fails
 * silently, which is why it is worth naming here.
 */
const POND_ROWS: Array<{ label: string; icon: string | React.ReactNode; nav: DrawerNav }> = [
  { label: "Chat",  icon: HP_PATHS.chat, nav: { kind: "section", section: "chat" } },
  { label: "Voice", icon: micEl,         nav: { kind: "voice" } },
];

/**
 * "Manage" -- the twelve, in the design's order.
 *
 * The order is the design's and not alphabetical: it is roughly what the
 * household touches most first. Keep it in step with the design file rather
 * than tidying it.
 */
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
  // Pond starts open and Manage starts closed, per the design. Manage is twelve
  // chips; opening it by default would push Rooms and the routines below the
  // fold on the panel this targets.
  const [pondOpen, setPondOpen]     = useState(true);
  const [manageOpen, setManageOpen] = useState(false);

  // `devicesAreReal` is the store's own answer to "did this come off the wire",
  // and rooms are derived from devices, so it is the flag for both. Without it
  // the drawer listed a household's rooms back to them before a single request
  // had settled -- Living Room, Kitchen, Bedroom, Office, Outdoor -- in a house
  // that may have none of them, and the list then silently collapsed to one.
  const { rooms, devicesAreReal } = useHomeData();
  // Routines need no such flag: they are the household's recipes or they are
  // nothing. The store no longer substitutes a fixture for an empty list.
  const routines  = useRoutines();

  // Escape closes it. A panel that covers the screen and can only be dismissed
  // by finding the right pixel is a trap for anyone on a keyboard.
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
