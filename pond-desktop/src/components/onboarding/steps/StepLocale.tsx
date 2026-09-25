// Step 2, Language & Location; timezone is pre-filled from Intl and must be confirmed.

import { useState } from "react";
import { MapPin, CloudSun } from "lucide-react";
import { useOnboarding } from "../OnboardingContext";
import { FormLabel } from "../primitives/FormLabel";
import { ToggleRow } from "../primitives/ToggleRow";
import { Lead } from "../primitives/StepShell";
import { LANGUAGES } from "../onboarding.constants";
import { detectPlace } from "../../../lib/place";
import { ZonePicker } from "../../ZonePicker";

export function StepLocale() {
  const { draft, patch } = useOnboarding();
  const [detecting, setDetecting] = useState(false);
  const [note, setNote] = useState<string | null>(null);

  /** Runs the server's place-detection cascade, the same one Settings uses. */
  async function detect() {
    setDetecting(true);
    setNote(null);
    try {
      const at = await detectPlace(draft.locationName);
      patch({
        locationName: at.name || draft.locationName,
        timezone: at.timezone,
        latitude: at.latitude,
        longitude: at.longitude,
        // Enable weather only with coordinates or a name, or it stays on but unconfigurable.
        enableWeather: at.has_coordinates || Boolean(at.name),
      });
      setNote(
        at.note
          ? at.note
          : at.certain
            ? `Found ${at.name}.`
            : `Guessed ${at.name} from your time zone.`,
      );
    } catch {
      setNote("Could not work that out. Type the nearest town instead.");
    } finally {
      setDetecting(false);
    }
  }

  return (
    <div className="ob-step-content">
      <Lead>Used for voice responses, weather, and time-aware answers.</Lead>

      <div className="ob-field-grid">
        <div>
          <FormLabel>Language</FormLabel>
          <select
            value={draft.language}
            onChange={(e) => patch({ language: e.target.value })}
            className="ob-select"
          >
            {LANGUAGES.map((l) => (
              <option key={l.key} value={l.key}>
                {l.label}
              </option>
            ))}
          </select>
        </div>
        <div>
          <FormLabel>Timezone</FormLabel>
          <ZonePicker
            value={draft.timezone}
            onChange={(timezone) => patch({ timezone })}
            className="ob-select"
            aria-label="Timezone"
          />
        </div>
      </div>

      <div className="ob-field">
        <FormLabel optional>City / location name</FormLabel>
        <div className="ob-input-row">
          <input
            className="ob-input"
            placeholder="e.g. Nairobi, London, New York"
            value={draft.locationName}
            onChange={(e) => patch({ locationName: e.target.value })}
          />
          <button
            type="button"
            onClick={detect}
            disabled={detecting}
            className="ob-btn-secondary"
          >
            <MapPin size={14} strokeWidth={1.8} />
            {detecting ? "Detecting\u2026" : "Auto-detect"}
          </button>
        </div>
        {note && <p className="ob-field-hint">{note}</p>}
      </div>

      <ToggleRow
        icon={<CloudSun size={18} strokeWidth={1.8} />}
        title="Enable live weather"
        desc="Pulls forecast from Open-Meteo. Approximate network location only — no GPS needed."
        checked={draft.enableWeather}
        onChange={(v) => patch({ enableWeather: v })}
      />
    </div>
  );
}
