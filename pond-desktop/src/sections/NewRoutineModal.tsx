import { useState, useId } from "react";
import { Button } from "@heroui/react";
import { X, Check } from "lucide-react";
import { api } from "../api/PondApiClient";
import { refreshHomeData } from "../hub/state/hubDataStore";
import { useDialogFocusTrap } from "../components/shared";

// ── Colour / icon templates ───────────────────────────────────────────────────

export interface RoutineTemplate {
  id: string;
  label: string;
  color: string;
  bg: string;
  iconPath: string;
}

export const ROUTINE_TEMPLATES: RoutineTemplate[] = [
  { id: "morning",  label: "Morning",    color: "#F59E0B", bg: "linear-gradient(150deg,#FCD34D,#F59E0B)", iconPath: "M12 3v2M12 19v2M4.22 4.22l1.42 1.42M18.36 18.36l1.42 1.42M3 12H1M23 12h-2M4.22 19.78l1.42-1.42M18.36 5.64l1.42-1.42M12 7a5 5 0 1 0 0 10A5 5 0 0 0 12 7z" },
  { id: "night",    label: "Night",      color: "#6366F1", bg: "linear-gradient(150deg,#818CF8,#4F46E5)", iconPath: "M21 12.8A9 9 0 1 1 11.2 3a7 7 0 0 0 9.8 9.8z" },
  { id: "movie",    label: "Movie",      color: "#7C3AED", bg: "linear-gradient(150deg,#A78BFA,#7C3AED)", iconPath: "M15 10l4.55-2.73A1 1 0 0 1 21 8.2v7.6a1 1 0 0 1-1.45.94L15 14M3 8a2 2 0 0 1 2-2h8a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" },
  { id: "away",     label: "Away",       color: "#0D9488", bg: "linear-gradient(150deg,#2DD4BF,#0D9488)", iconPath: "M3 12l2-2m0 0l7-7 7 7M5 10v10a1 1 0 0 0 1 1h3m10-11l2 2m-2-2v10a1 1 0 0 0-1 1h-3m-6 0h6" },
  { id: "focus",    label: "Focus",      color: "#EC4899", bg: "linear-gradient(150deg,#F472B6,#DB2777)", iconPath: "M13 4l-2 5h4l-2 5M5 21l3-7M19 21l-3-7M9 14h6" },
  { id: "home",     label: "Home",       color: "#F97316", bg: "linear-gradient(150deg,#FB923C,#EA580C)", iconPath: "M3 9l9-7 9 7v11a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2z" },
  { id: "music",    label: "Music",      color: "#16A34A", bg: "linear-gradient(150deg,#4ADE80,#16A34A)", iconPath: "M9 18V5l12-2v13M9 18a3 3 0 1 1-6 0 3 3 0 0 1 6 0zM21 16a3 3 0 1 1-6 0 3 3 0 0 1 6 0z" },
  { id: "security", label: "Security",   color: "#EF4444", bg: "linear-gradient(150deg,#F87171,#DC2626)", iconPath: "M12 22s8-4 8-10V5l-8-3-8 3v7c0 6 8 10 8 10z" },
  { id: "sleep",    label: "Sleep",      color: "#475569", bg: "linear-gradient(150deg,#94A3B8,#475569)", iconPath: "M18 8h1a4 4 0 0 1 0 8h-1M5 8H2v8h3M8 8v8M2 12h3M8 12h13" },
  { id: "exercise", label: "Exercise",   color: "#65A30D", bg: "linear-gradient(150deg,#A3E635,#65A30D)", iconPath: "M18 20V10M12 20V4M6 20v-6" },
  { id: "coffee",   label: "Coffee",     color: "#92400E", bg: "linear-gradient(150deg,#D97706,#92400E)", iconPath: "M18 8h1a4 4 0 0 1 0 8h-1M2 8h16v9a4 4 0 0 1-4 4H6a4 4 0 0 1-4-4V8zM6 1v3M10 1v3M14 1v3" },
  { id: "sky",      label: "Chill",      color: "#0EA5E9", bg: "linear-gradient(150deg,#38BDF8,#0284C7)", iconPath: "M18 10h-1.26A8 8 0 1 0 9 20h9a5 5 0 0 0 0-10z" },
];

// ── Predefined actions by category ───────────────────────────────────────────

interface ActionCategory {
  label: string;
  actions: string[];
}

const ACTION_CATEGORIES: ActionCategory[] = [
  {
    label: "Lighting",
    actions: ["Lights on", "Lights off", "Dim to 20%", "Dim to 60%", "Desk lamp on", "Desk lamp off", "Bright mode"],
  },
  {
    label: "Climate",
    actions: ["Heat to 66°F", "Heat to 68°F", "Heat to 70°F", "Heat to 72°F", "Cool to 72°F", "Eco mode", "Fan on", "Fan off"],
  },
  {
    label: "Security",
    actions: ["Lock all doors", "Unlock front door", "Arm security", "Disarm security", "Arm cameras", "Motion alerts on", "Motion alerts off"],
  },
  {
    label: "Entertainment",
    actions: ["TV on", "TV off", "Play lo-fi playlist", "Play focus playlist", "Play relaxing music", "Mute audio", "Start podcast"],
  },
  {
    label: "Productivity",
    actions: ["Do not disturb", "Clear do not disturb", "Start focus timer", "Mute notifications", "Set Slack to Away", "Set Slack to Active"],
  },
  {
    label: "Comfort",
    actions: ["Close blinds", "Open blinds", "Brew coffee", "Start diffuser", "Warm lighting", "Read news briefing"],
  },
];

// ── Component ─────────────────────────────────────────────────────────────────

interface Props {
  onClose: () => void;
  onCreated: () => void;
}

export function NewRoutineModal({ onClose, onCreated }: Props) {
  const [name, setName]             = useState("");
  const [selectedActions, setSelectedActions] = useState<string[]>([]);
  const [template, setTemplate]     = useState<RoutineTemplate>(ROUTINE_TEMPLATES[0]);
  const [timeLabel, setTimeLabel]   = useState("On demand");
  const [saving, setSaving]         = useState(false);
  const [error, setError]           = useState<string | null>(null);
  const titleId = useId();
  const dialogRef = useDialogFocusTrap<HTMLDivElement>(true, onClose);

  function toggleAction(action: string) {
    setSelectedActions((prev) =>
      prev.includes(action)
        ? prev.filter((a) => a !== action)
        : prev.length < 6 ? [...prev, action] : prev,
    );
  }

  function buildPrompt(): string {
    if (selectedActions.length === 0) return `Run the ${name} routine.`;
    const list = selectedActions
      .map((a) => a.toLowerCase())
      .join(", ");
    return `Run the ${name} routine: ${list}.`;
  }

  /** Escape for a YAML double-quoted scalar. Backslashes first, so the quote escapes are not doubled. */
  function yamlDoubleQuoted(s: string): string {
    return s.replace(/\\/g, "\\\\").replace(/"/g, '\\"');
  }

  async function handleSave() {
    if (!name.trim()) { setError("Name is required"); return; }
    if (selectedActions.length === 0) { setError("Add at least one action"); return; }
    setSaving(true);
    setError(null);
    try {
      await api.createRecipe({
        name: name.trim(),
        description: selectedActions.join(", "),
        yaml: `prompt: "${yamlDoubleQuoted(buildPrompt())}"`,
      });
      await refreshHomeData();
      onCreated();
    } catch (e) {
      setError(String(e));
      setSaving(false);
    }
  }

  return (
    <div className="sched-modal__overlay" onClick={onClose}>
      <div
        ref={dialogRef}
        className="sched-modal__dialog nr-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div className="sched-modal__header">
          <h2 id={titleId} className="sched-modal__title">New Routine</h2>
          <button className="sched-modal__close" onClick={onClose} aria-label="Close">
            <X size={16} />
          </button>
        </div>

        <div className="sched-modal__body">
          {/* Name */}
          <div className="sched-modal__field">
            <label className="sched-modal__label">Name</label>
            <input
              className="sched-modal__input"
              placeholder="e.g. Wind Down"
              value={name}
              onChange={(e) => setName(e.target.value)}
              maxLength={40}
            />
          </div>

          {/* Colour / icon */}
          <div className="sched-modal__field">
            <label className="sched-modal__label">Style</label>
            <div className="nr-template-grid">
              {ROUTINE_TEMPLATES.map((t) => (
                <button
                  key={t.id}
                  className={`nr-template-btn${template.id === t.id ? " is-active" : ""}`}
                  style={{ background: t.bg }}
                  onClick={() => setTemplate(t)}
                  title={t.label}
                  type="button"
                >
                  <svg width="18" height="18" viewBox="0 0 24 24" fill="none"
                    stroke="#fff" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                    <path d={t.iconPath} />
                  </svg>
                  {template.id === t.id && (
                    <span className="nr-template-check"><Check size={10} /></span>
                  )}
                </button>
              ))}
            </div>
          </div>

          {/* Actions */}
          <div className="sched-modal__field">
            <label className="sched-modal__label--row">
              Actions
              <span className="nr-label-hint">{selectedActions.length}/6 selected</span>
            </label>
            <div className="nr-actions-scroll">
              {ACTION_CATEGORIES.map((cat) => (
                <div key={cat.label} className="nr-cat">
                  <div className="nr-cat__label">{cat.label}</div>
                  <div className="nr-cat__chips">
                    {cat.actions.map((action) => {
                      const active = selectedActions.includes(action);
                      const disabled = !active && selectedActions.length >= 6;
                      return (
                        <button
                          key={action}
                          type="button"
                          className={`nr-chip${active ? " is-active" : ""}${disabled ? " is-disabled" : ""}`}
                          onClick={() => !disabled && toggleAction(action)}
                        >
                          {active && <Check size={10} />}
                          {action}
                        </button>
                      );
                    })}
                  </div>
                </div>
              ))}
            </div>
          </div>

          {/* Selected actions preview */}
          {selectedActions.length > 0 && (
            <div className="nr-preview">
              {selectedActions.map((a) => (
                <span key={a} className="nr-preview-chip">
                  {a}
                  <button onClick={() => toggleAction(a)} type="button" aria-label="Remove">
                    <X size={9} />
                  </button>
                </span>
              ))}
            </div>
          )}

          {/* Time label */}
          <div className="sched-modal__field">
            <label className="sched-modal__label--row">
              When
              <span className="nr-label-hint">display only</span>
            </label>
            <input
              className="sched-modal__input"
              placeholder="e.g. 9:00 AM · weekdays"
              value={timeLabel}
              onChange={(e) => setTimeLabel(e.target.value)}
              maxLength={40}
            />
          </div>

          {error && <p className="nr-error">{error}</p>}
        </div>

        {/* Footer */}
        <div className="sched-modal__footer">
          <Button variant="ghost" size="sm" onPress={onClose} isDisabled={saving}>
            Cancel
          </Button>
          <Button
            variant="primary"
            size="sm"
            onPress={handleSave}
            isDisabled={saving || !name.trim() || selectedActions.length === 0}
          >
            {saving ? "Creating…" : "Create Routine"}
          </Button>
        </div>
      </div>
    </div>
  );
}
