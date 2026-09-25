// Step 0, Welcome: system info from GET /api/v1/health and /system/info.

import { Lock, Mic, Shield } from "lucide-react";
import { Logo } from "../../Logo";
import { useSystemInfo } from "../hooks/useSystemInfo";

interface Props {
  onNext: () => void;
}

const ICON_PROPS = { size: 14, strokeWidth: 1.8 } as const;

export function StepWelcome({ onNext }: Props) {
  const { health, system, loading } = useSystemInfo();

  const sysRows = [
    {
      label: "Device",
      value: loading
        ? "Detecting\u2026"
        : `${system?.hostname || "pond.local"} \u00b7 ${system?.arch || "unknown"}`,
    },
    {
      label: "System",
      value: loading
        ? "Detecting\u2026"
        : `${system?.platform || "unknown"} \u00b7 ${system?.arch || "unknown"}`,
    },
    {
      label: "Server",
      value: loading
        ? "Connecting\u2026"
        : `pond-server ${health?.version || "v0.1.0"} \u00b7 :4000`,
    },
  ];

  return (
    <div className="ob-welcome">
      <Logo className="ob-welcome__logo" size={96} />

      <h1 className="ob-welcome__title">Welcome to Goose In A Pond</h1>
      <p className="ob-welcome__subtitle">
        Your private AI assistant &mdash; on your home network.
      </p>

      <p className="ob-welcome__body">
        Goose runs entirely on your own hardware. It thinks locally, speaks
        locally, and never sends your data anywhere. Once set up, just say the
        wake word and Goose is ready.
      </p>

      <div className="ob-welcome__pills">
        <span className="ob-welcome__pill"><Lock {...ICON_PROPS} /> Fully offline</span>
        <span className="ob-welcome__pill"><Mic {...ICON_PROPS} /> Voice-first</span>
        <span className="ob-welcome__pill"><Shield {...ICON_PROPS} /> Privacy by design</span>
      </div>

      <button type="button" onClick={onNext} className="ob-welcome__cta">
        Get started &rarr;
      </button>
      <span className="ob-welcome__hint">Takes about 2 minutes</span>

      <div className="ob-welcome__sys-table">
        {sysRows.map((r) => (
          <div key={r.label} className="ob-welcome__sys-row">
            <span className="ob-welcome__sys-label">{r.label}</span>
            <code className="ob-welcome__sys-value">{r.value}</code>
          </div>
        ))}
      </div>
    </div>
  );
}
