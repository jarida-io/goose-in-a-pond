// Step 6, All Set (Extensions + Done).

import { useMemo, Fragment } from "react";
import { motion } from "framer-motion";
import { CloudSun, Brain, Home, CalendarDays } from "lucide-react";
import { useOnboarding } from "../OnboardingContext";
import { ToggleRow } from "../primitives/ToggleRow";
import { PROMPT_STYLES } from "../onboarding.constants";
import { describeVoice } from "../../../voice/voiceCatalogue";

const ICON_PROPS = { size: 18, strokeWidth: 1.8 } as const;

interface Props {
  onFinish: () => void;
}

export function StepComplete({ onFinish }: Props) {
  const { draft, patch } = useOnboarding();

  const rows = useMemo(
    () => [
      { k: "You",        v: `${draft.avatar} ${draft.preferredName || draft.userName || "friend"}` },
      { k: "Locale",     v: `${draft.language.toUpperCase()} \u00b7 ${draft.timezone}` },
      { k: "Style",      v: PROMPT_STYLES.find((p) => p.value === draft.promptStyle)?.label ?? "Balanced" },
      { k: "Assistant",  v: `${draft.assistantName} \u00b7 ${describeVoice(draft.ttsVoice).name}` },
      { k: "Wake word",  v: draft.wakeWord === "custom" ? `"${draft.wakeWordCustom}"` : `"${draft.wakeWord}"` },
      { k: "Model",      v: "Auto (downloads on first run)" },
    ],
    [draft],
  );

  return (
    <div className="ob-complete">
      {/* Extensions section */}
      <div className="ob-complete__extensions">
        <div className="ob-complete__extensions-header">Extensions</div>
        <p className="ob-lead">
          Enable built-in extensions. Connect more from Settings &rarr; Extensions later.
        </p>
        <div className="ob-toggle-stack">
          <ToggleRow
            icon={<CloudSun {...ICON_PROPS} />}
            title="Weather"
            desc={
              draft.enableWeather
                ? `Open-Meteo live forecast${draft.locationName ? ` for ${draft.locationName}` : ""}. Auto-detected from your network.`
                : "Enable on the Location step to use this. Location detected from your network."
            }
            checked={draft.enableWeather}
            onChange={(v) => patch({ enableWeather: v })}
          />
          <ToggleRow
            icon={<Brain {...ICON_PROPS} />}
            title="Memory"
            desc="Goose remembers facts across conversations using a local flat-file store. Nothing leaves your device."
            checked={draft.enableMcpMemory}
            onChange={(v) => patch({ enableMcpMemory: v })}
          />
          <ToggleRow
            icon={<Home {...ICON_PROPS} />}
            title="Home Assistant"
            desc="Control lights, sensors, and scenes via your local Home Assistant instance."
            checked={draft.enableHomeAssistant}
            onChange={(v) => patch({ enableHomeAssistant: v })}
            badge="Beta"
          />
          <ToggleRow
            icon={<CalendarDays {...ICON_PROPS} />}
            title="Calendar"
            desc="Read today's events from your local calendar app."
            checked={draft.enableCalendar}
            onChange={(v) => patch({ enableCalendar: v })}
            badge="Beta"
          />
        </div>
        <p className="ob-complete__extensions-hint ob-field-hint">
          Custom MCP servers and more can be added in Settings &rarr; Extensions.
        </p>
      </div>

      {/* Animated checkmark */}
      <motion.div
        className="ob-complete__check"
        initial={{ opacity: 0, scale: 0.5 }}
        animate={{ opacity: 1, scale: 1 }}
        transition={{ type: "spring", stiffness: 260, damping: 20, delay: 0.1 }}
      >
        <svg width="34" height="34" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
          <path d="M5 12l5 5 9-11" />
        </svg>
      </motion.div>

      <h1 className="ob-complete__title">You're all set!</h1>
      <p className="ob-complete__body">
        Goose is ready on your network. Try saying{" "}
        <code className="ob-complete__wake-code">
          "{draft.wakeWord === "custom" ? draft.wakeWordCustom || "your phrase" : draft.wakeWord}"
        </code>{" "}
        or click below.
      </p>

      {/* Summary card */}
      <div className="ob-complete__summary">
        <div className="ob-complete__summary-label">Your setup</div>
        <dl className="ob-complete__summary-grid">
          {rows.map((r) => (
            <Fragment key={r.k}>
              <dt className="ob-complete__summary-key">{r.k}</dt>
              <dd className="ob-complete__summary-val">{r.v}</dd>
            </Fragment>
          ))}
        </dl>
      </div>

      <div className="ob-complete__actions">
        <button type="button" onClick={onFinish} className="ob-complete__btn-primary">
          Open dashboard
        </button>
        <button type="button" onClick={onFinish} className="ob-complete__btn-secondary">
          Start chatting
        </button>
      </div>

      <p className="ob-complete__footer">
        All settings can be changed from Settings at any time.
      </p>
    </div>
  );
}
