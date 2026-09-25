import { Sun, Moon, Sunset, Check } from "lucide-react";
import {
  useTheme,
  ACCENT_PALETTES,
  type ThemeChoice,
  type AccentName,
  type DensityChoice,
} from "../../state/themeStore";
import { DeviceTile } from "../../primitives/DeviceTile";
import type { DeviceData } from "../../data/mockHome";
import { hubSetDevice } from "../../state/hubStore";

// ─── A sample device shown as a live accent preview ──────────────────────────

const PREVIEW_DEVICE: DeviceData = {
  id: "appearance-preview-light",
  name: "Accent Preview",
  kind: "light",
  room: "General",
  on: true,
};

// Seed hubStore so the preview tile renders on; an unknown id defaults to off/locked.
hubSetDevice(PREVIEW_DEVICE.id, { on: true, brightness: 80 });

// ─── Segmented control ────────────────────────────────────────────────────────

interface SegmentedProps<T extends string> {
  options: T[];
  value: T;
  onChange: (v: T) => void;
  renderLabel?: (v: T) => React.ReactNode;
}

function Segmented<T extends string>({
  options,
  value,
  onChange,
  renderLabel,
}: SegmentedProps<T>) {
  return (
    <div className="hseg" role="group">
      {options.map((opt) => (
        <button
          key={opt}
          className="hseg__opt"
          data-active={opt === value}
          onClick={() => onChange(opt)}
          type="button"
          aria-pressed={opt === value}
        >
          {renderLabel ? renderLabel(opt) : opt}
        </button>
      ))}
    </div>
  );
}

// ─── Theme icons ─────────────────────────────────────────────────────────────

function ThemeIcon({ choice }: { choice: ThemeChoice }) {
  if (choice === "Light")  return <Sun   size={14} style={{ marginRight: 5 }} />;
  if (choice === "Dark")   return <Moon  size={14} style={{ marginRight: 5 }} />;
  return                          <Sunset size={14} style={{ marginRight: 5 }} />;
}

// ─── Accent swatch ────────────────────────────────────────────────────────────

function AccentSwatch({
  name,
  active,
  onSelect,
}: {
  name: AccentName;
  active: boolean;
  onSelect: () => void;
}) {
  const [pp] = ACCENT_PALETTES[name];
  return (
    <button
      className="swatch"
      type="button"
      data-active={active}
      onClick={onSelect}
      aria-label={`${name} accent${active ? " (selected)" : ""}`}
      style={{ color: pp }}
    >
      <span
        className="swatch__dot"
        style={{ background: pp }}
      >
        {active && (
          <Check size={20} color="#fff" strokeWidth={3} />
        )}
      </span>
      <span className="swatch__name">{name}</span>
    </button>
  );
}

// ─── Section wrapper used in design ──────────────────────────────────────────

function SetCard({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div className="setcard">
      <div className="setcard__head">
        <span className="setcard__title">{title}</span>
      </div>
      {children}
    </div>
  );
}

// ─── Main view ───────────────────────────────────────────────────────────────

export function AppearanceView() {
  const { theme, accent, density, setTheme, setAccent, setDensity } = useTheme();

  const themeOptions: ThemeChoice[]   = ["Light", "Dark", "Auto"];
  const accentOptions: AccentName[]   = ["Purple", "Blue", "Teal", "Coral", "Magenta"];
  const densityOptions: DensityChoice[] = ["Comfortable", "Compact"];

  return (
    <div className="setd">
      {/* Header */}
      <div className="setd__titlerow">
        <div>
          <h1 className="view-title">Appearance</h1>
          <p className="view-sub">Theme, accent and day / night</p>
        </div>
      </div>

      <div className="setd__body">

        {/* Theme */}
        <SetCard title="Theme">
          <div className="srow" style={{ cursor: "default" }}>
            <div className="srow__text">
              <span className="srow__label">Color theme</span>
              <span className="srow__sub">
                Auto switches to dark between 19:00 and 06:00
              </span>
            </div>
            <div className="srow__control">
              <Segmented
                options={themeOptions}
                value={theme}
                onChange={setTheme}
                renderLabel={(opt) => (
                  <span style={{ display: "flex", alignItems: "center" }}>
                    <ThemeIcon choice={opt} />
                    {opt}
                  </span>
                )}
              />
            </div>
          </div>
        </SetCard>

        {/* Accent */}
        <SetCard title="Accent color">
          <div className="swatches">
            {accentOptions.map((name) => (
              <AccentSwatch
                key={name}
                name={name}
                active={accent === name}
                onSelect={() => setAccent(name)}
              />
            ))}
          </div>
        </SetCard>

        {/* Density */}
        <SetCard title="Density">
          <div className="srow" style={{ cursor: "default" }}>
            <div className="srow__text">
              <span className="srow__label">Layout density</span>
              <span className="srow__sub">
                Compact reduces spacing for smaller screens
              </span>
            </div>
            <div className="srow__control">
              <Segmented
                options={densityOptions}
                value={density}
                onChange={setDensity}
              />
            </div>
          </div>
        </SetCard>

        {/* Live preview */}
        <SetCard title="Preview">
          <div style={{ padding: "10px 10px 14px" }}>
            <p
              style={{
                fontSize: 12,
                fontWeight: 600,
                color: "var(--mut)",
                margin: "0 0 10px",
              }}
            >
              The tile below updates as you change accent and theme.
            </p>
            <div style={{ maxWidth: 180 }}>
              <DeviceTile device={PREVIEW_DEVICE} />
            </div>
          </div>
        </SetCard>

      </div>
    </div>
  );
}
