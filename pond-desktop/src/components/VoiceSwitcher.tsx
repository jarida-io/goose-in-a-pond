import { useEffect, useRef, useState } from "react";
import { Check, Lock } from "lucide-react";
import { api } from "../api/PondApiClient";
import type { ModelEntry } from "../api/types";

interface Props {
  isOpen: boolean;
  onClose: () => void;
}

// Piper's file-naming convention: <locale>-<voice>-<quality>.onnx.
const LOCALE_LABELS: Record<string, string> = {
  en_US: "US English",
  en_GB: "British English",
  fr_FR: "French",
  de_DE: "German",
  sw_CD: "Swahili",
};

function describeVoice(entry: ModelEntry): { label: string; sub: string } {
  const filename = entry.filename ?? entry.name;
  const match = /^([a-z]{2}_[A-Z]{2})-([a-z_]+)-(medium|high)/.exec(filename);
  if (!match) return { label: entry.display_name ?? entry.name, sub: "" };
  const [, locale, voiceSlug, quality] = match;
  const label = voiceSlug
    .split("_")
    .map((w) => w.charAt(0).toUpperCase() + w.slice(1))
    .join(" ");
  return { label, sub: `${LOCALE_LABELS[locale] ?? locale} · ${quality}` };
}

/** Voice-mode header popover: pick a downloaded TTS voice to activate it (as Models.tsx does). */
export function VoiceSwitcher({ isOpen, onClose }: Props) {
  const ref = useRef<HTMLDivElement>(null);
  const [voices, setVoices] = useState<ModelEntry[] | null>(null);
  const [activeFilename, setActiveFilename] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [switching, setSwitching] = useState<string | null>(null);

  useEffect(() => {
    if (!isOpen) return;
    function handleClick(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose();
    }
    document.addEventListener("mousedown", handleClick);
    return () => document.removeEventListener("mousedown", handleClick);
  }, [isOpen, onClose]);

  useEffect(() => {
    if (!isOpen) return;
    setError(null);
    Promise.all([api.listModels(), api.getSettings()])
      .then(([models, settings]) => {
        setVoices(
          models.filter(
            (m) =>
              m.provider === "tts" ||
              m.provider === "tts_piper" ||
              m.provider === "tts_kokoro" ||
              m.provider === "tts_http",
          ),
        );
        setActiveFilename(settings.voice_tts_voice ?? null);
      })
      .catch((e) => setError(e instanceof Error ? e.message : String(e)));
  }, [isOpen]);

  async function selectVoice(entry: ModelEntry) {
    if (entry.downloaded === false || switching !== null) return;
    setSwitching(entry.name);
    setError(null);
    try {
      // `category`, not `provider` — see the note in sections/Models.tsx.
      await api.activateModel(entry.category ?? entry.provider, entry.name, "tts");
      setActiveFilename(entry.filename ?? null);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setSwitching(null);
    }
  }

  if (!isOpen) return null;

  return (
    <div ref={ref} className="voice-switcher">
      <div className="voice-switcher__header">Voice</div>

      {voices === null && !error && (
        <div className="voice-switcher__status">Loading…</div>
      )}
      {error && (
        <div className="voice-switcher__status voice-switcher__status--error">{error}</div>
      )}

      {voices?.map((v) => {
        const { label, sub } = describeVoice(v);
        const isActive = v.filename !== undefined && v.filename === activeFilename;
        const isDownloaded = v.downloaded !== false;
        return (
          <button
            key={v.name}
            type="button"
            className={`voice-switcher__row${isActive ? " is-active" : ""}`}
            onClick={() => selectVoice(v)}
            disabled={!isDownloaded || switching !== null}
          >
            {isActive
              ? <Check size={13} className="voice-switcher__check" />
              : <span className="voice-switcher__check-spacer" />}
            <span className="voice-switcher__text">
              <span className="voice-switcher__label">{label}</span>
              <span className="voice-switcher__sub">
                {isDownloaded
                  ? sub
                  : (
                    <span className="voice-switcher__hint">
                      <Lock size={10} /> Download in Settings → Models
                    </span>
                  )}
              </span>
            </span>
          </button>
        );
      })}
    </div>
  );
}
