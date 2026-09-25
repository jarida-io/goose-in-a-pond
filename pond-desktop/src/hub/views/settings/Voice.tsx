import { useState, useEffect, useCallback, useRef } from "react";
import { Play, RefreshCw, Loader2, RotateCcw, Square, AlertCircle } from "lucide-react";
import { DetailShell } from "./DetailShell";
import { Card, Row, Toggle, Slider } from "./controls";
import { api } from "../../../api/PondApiClient";
import type { Settings, ModelEntry } from "../../../api/types";
import { useVoicePreview } from "../../../voice/useVoicePreview";
import { VoicePicker } from "./VoicePicker";
import "./voice-picker.css";
import {
  groupVoices,
  describeVoice,
  paceLabel,
  clampPace,
  describeQuality,
  VOICE_QUALITY_TIERS,
  DEFAULT_VOICE,
  DEFAULT_PACE,
  DEFAULT_QUALITY,
  MIN_PACE,
  MAX_PACE,
} from "../../../voice/voiceCatalogue";

// ─── Icon path strings ───────────────────────────────────────
const SICN = {
  mic:     "M12 2a3 3 0 0 1 3 3v6a3 3 0 0 1-6 0V5a3 3 0 0 1 3-3zM19 10a7 7 0 0 1-14 0M12 19v3M8 22h8",
  speaker: "M11 5L6 9H2v6h4l5 4zM19 5a10 10 0 0 1 0 14M15.5 8.5a5 5 0 0 1 0 7",
  text:    "M4 6h16M4 10h16M4 14h10",
} as const;

// ─── STT model options ────────────────────────────────────────
interface SttOption {
  label: string;
  value: string;
}

const STT_OPTIONS: SttOption[] = [
  { label: "Whisper base",          value: "ggml-base.bin"         },
  { label: "Whisper small",         value: "ggml-small.bin"        },
  { label: "Whisper large-v3-turbo", value: "ggml-large-v3-turbo.bin" },
];

// Installed voices come from the model catalogue; names derive from ids (`af_heart` → "Heart").
const TTS_PROVIDERS = ["tts", "tts_piper", "tts_kokoro", "tts_http"];

// ─── Mock fallback ────────────────────────────────────────────
const MOCK_VOICE_SETTINGS: Partial<Settings> = {
  voice_wake_word: "goose",
  active_whisper_model: "ggml-base.bin",
  voice_tts_voice: DEFAULT_VOICE,
  voice_tts_speed: DEFAULT_PACE,
  voice_tts_quality: DEFAULT_QUALITY,
};

/** Slider works in whole percent; the setting is a multiplier the engine takes. */
const paceToPct = (pace: number) => Math.round(clampPace(pace) * 100);
const pctToPace = (pct: number) => clampPace(pct / 100);

interface VoiceDetailProps {
  go: (route: string) => void;
}

export function VoiceDetail({ go }: VoiceDetailProps) {
  // ── State ──────────────────────────────────────────────────
  const [settings, setSettings]       = useState<Partial<Settings>>(MOCK_VOICE_SETTINGS);
  const [loading, setLoading]         = useState(true);
  const [error, setError]             = useState<string | null>(null);
  const [flash, setFlash]             = useState<{ text: string; ok: boolean } | null>(null);

  // Installed voices, from the model catalogue.
  const [voiceIds, setVoiceIds]       = useState<string[]>([]);

  const preview = useVoicePreview();

  // Reset calibration state
  const [resetting, setResetting]     = useState(false);

  // Debounce timer for slider
  const sliderTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const flashTimer  = useRef<ReturnType<typeof setTimeout> | null>(null);

  // ── Flash helper ───────────────────────────────────────────
  function showFlash(text: string, ok = true) {
    if (flashTimer.current) clearTimeout(flashTimer.current);
    setFlash({ text, ok });
    flashTimer.current = setTimeout(() => setFlash(null), 3000);
  }

  // ── Load settings on mount ─────────────────────────────────
  const loadSettings = useCallback(async () => {
    setLoading(true);
    setError(null);
    try {
      const s = await api.getSettings();
      // A 404 or empty body resolves to `undefined`, which would make every `settings.x` read throw.
      if (s && typeof s === "object") {
        setSettings(s);
      } else {
        throw new Error("settings response was empty");
      }
    } catch (e) {
      console.warn("[VoiceDetail] API offline — using mock fallback:", e);
      setError("Could not reach the server. Showing offline view.");
    } finally {
      setLoading(false);
    }
  }, []);

  // Separate, non-blocking read: without the catalogue the panel still works, minus alternatives.
  const loadVoices = useCallback(async () => {
    try {
      const models: ModelEntry[] = await api.listModels();
      setVoiceIds(
        models
          .filter((m) => TTS_PROVIDERS.includes(m.provider) && m.downloaded !== false)
          .map((m) => m.name),
      );
    } catch (e) {
      console.warn("[VoiceDetail] could not list voices:", e);
    }
  }, []);

  useEffect(() => {
    loadSettings();
    loadVoices();
    return () => {
      if (sliderTimer.current) clearTimeout(sliderTimer.current);
      if (flashTimer.current) clearTimeout(flashTimer.current);
    };
  }, [loadSettings, loadVoices]);

  // ── Hands-free toggle ──────────────────────────────────────
  // TODO: add `voice_hands_free` to Settings and wire updateSettings; local-only until then.
  const [handsFreePending, setHandsFreePending] = useState(false);
  const handsFree = false; // placeholder — replace with settings.voice_hands_free ?? false

  async function handleHandsFreeChange(on: boolean) {
    if (handsFreePending) return;
    setHandsFreePending(true);
    try {
      // TODO: persist with api.updateSettings({ voice_hands_free: on }); log and flash until then.
      console.info("[VoiceDetail] hands-free toggled:", on, "(not persisted — field pending in backend)");
      showFlash(on ? "Hands-free enabled (coming in wave 4)" : "Hands-free disabled (coming in wave 4)", true);
    } finally {
      setHandsFreePending(false);
    }
  }

  // ── STT model select ───────────────────────────────────────
  async function handleSttChange(value: string) {
    setSettings((prev) => ({ ...prev, active_whisper_model: value }));
    try {
      await api.updateSettings({ active_whisper_model: value });
      showFlash("Speech recognition model updated.");
    } catch (e) {
      // Revert
      setSettings((prev) => ({ ...prev, active_whisper_model: settings.active_whisper_model }));
      showFlash(`Failed to update STT model: ${String(e)}`, false);
    }
  }

  // ── TTS voice select ───────────────────────────────────────
  async function handleTtsVoiceChange(value: string) {
    const previous = settings.voice_tts_voice;
    setSettings((prev) => ({ ...prev, voice_tts_voice: value }));
    try {
      await api.updateSettings({ voice_tts_voice: value });
      // Play a sample at once; it's cheap (a 522 KB style-table swap, no model reload).
      void preview.play();
    } catch (e) {
      setSettings((prev) => ({ ...prev, voice_tts_voice: previous }));
      showFlash(`Failed to update voice: ${String(e)}`, false);
    }
  }

  // ── Quality tier ───────────────────────────────────────────
  async function handleQualityChange(value: string) {
    const previous = settings.voice_tts_quality;
    setSettings((prev) => ({ ...prev, voice_tts_quality: value }));
    try {
      await api.updateSettings({ voice_tts_quality: value });
      showFlash(`Voice quality set to ${describeQuality(value).label.toLowerCase()}.`);
    } catch (e) {
      setSettings((prev) => ({ ...prev, voice_tts_quality: previous }));
      showFlash(`Failed to update voice quality: ${String(e)}`, false);
    }
  }

  // ── Thinking tone toggle ───────────────────────────────────
  // Defaults ON, so an absent key reads as ON — `!== false`, not `?? false`.
  const thinkingTone = settings.voice_thinking_tone_enabled !== false;

  async function handleThinkingToneChange(on: boolean) {
    setSettings((prev) => ({ ...prev, voice_thinking_tone_enabled: on }));
    try {
      await api.updateSettings({ voice_thinking_tone_enabled: on });
      showFlash(on ? "Thinking sound on." : "Thinking sound off.");
    } catch (e) {
      setSettings((prev) => ({ ...prev, voice_thinking_tone_enabled: !on }));
      showFlash(`Failed to update thinking sound: ${String(e)}`, false);
    }
  }

  // ── Speaking rate slider (debounced) ───────────────────────
  // A drag emits a value per pixel; each would be a settings write and a synthesis.
  function handlePaceChange(next: number) {
    const pace = clampPace(next);
    setSettings((prev) => ({ ...prev, voice_tts_speed: pace }));
    if (sliderTimer.current) clearTimeout(sliderTimer.current);
    sliderTimer.current = setTimeout(async () => {
      try {
        await api.updateSettings({ voice_tts_speed: pace });
        // Pace is the one setting you cannot judge by reading it.
        void preview.play();
      } catch (e) {
        showFlash(`Failed to update speaking pace: ${String(e)}`, false);
      }
    }, 500);
  }

  // ── Reset wake-word calibration ────────────────────────────
  async function handleResetCalibration() {
    setResetting(true);
    try {
      await api.resetWakeWordCalibration();
      setSettings((prev) => ({ ...prev, voice_wake_word_transcriptions: [] }));
      showFlash("Wake word calibration cleared.");
    } catch (e) {
      showFlash(`Failed to reset calibration: ${String(e)}`, false);
    } finally {
      setResetting(false);
    }
  }

  // ── Derived helpers ────────────────────────────────────────
  const currentStt    = settings.active_whisper_model ?? "ggml-base.bin";
  const currentVoice  = settings.voice_tts_voice ?? DEFAULT_VOICE;
  const currentPace   = settings.voice_tts_speed ?? DEFAULT_PACE;
  const currentQuality = settings.voice_tts_quality ?? DEFAULT_QUALITY;
  const wakeWord      = settings.voice_wake_word ?? "";
  const transcriptions = settings.voice_wake_word_transcriptions ?? [];
  const isCalibrated  = transcriptions.length > 0;

  const sttLabel  = STT_OPTIONS.find((o) => o.value === currentStt)?.label ?? currentStt;

  // Always offer the saved voice, or the select renders blank and reassigns it on the next change.
  const offeredVoices = voiceIds.includes(currentVoice) ? voiceIds : [currentVoice, ...voiceIds];
  const voiceGroups = groupVoices(offeredVoices);
  const voiceInfo = describeVoice(currentVoice);
  const voiceLabel = voiceInfo.language
    ? `${voiceInfo.name} — ${voiceInfo.language}`
    : voiceInfo.name;
  const qualityTier = describeQuality(currentQuality);

  return (
    <DetailShell
      title="Voice"
      subtitle="Wake word, speech recognition and Goose's voice."
      accent="#DB2777"
      onBack={() => go("settings")}
      headRight={
        <button
          className="mrow__btn"
          type="button"
          onClick={loadSettings}
          aria-label="Refresh voice settings"
          style={{ minWidth: 32, display: "flex", alignItems: "center", justifyContent: "center" }}
        >
          <RefreshCw size={13} strokeWidth={2} style={{ opacity: loading ? 0.4 : 1 }} />
        </button>
      }
    >
      {/* Flash feedback */}
      {flash && (
        <div
          style={{
            padding: "8px 12px",
            borderRadius: 6,
            fontSize: 13,
            background: flash.ok ? "#f0fdf4" : "#fef2f2",
            color:      flash.ok ? "#16a34a" : "#dc2626",
            border:     `1px solid ${flash.ok ? "#bbf7d0" : "#fecaca"}`,
          }}
          role="status"
          aria-live="polite"
        >
          {flash.text}
        </div>
      )}

      {/* Offline error banner */}
      {error && (
        <div
          style={{
            padding: "8px 12px",
            borderRadius: 6,
            fontSize: 13,
            background: "#fffbeb",
            color: "#92400e",
            border: "1px solid #fde68a",
          }}
        >
          {error}
        </div>
      )}

      {/* ── Listening card ──────────────────────────────────── */}
      <Card title="Listening">
        <Row
          label="Hands-free mode"
          sub="Always listening for the wake word"
          control={
            <Toggle
              on={handsFree}
              onChange={handleHandsFreeChange}
            />
          }
        />

        {/* Wake word + calibration status */}
        <div className="srow" style={{ cursor: "default" }}>
          <span className="srow__text">
            <span className="srow__label">Wake word</span>
            <span className="srow__sub">
              {loading ? (
                <span style={{ display: "inline-block", height: 10, width: 80, background: "#e2e8f0", borderRadius: 4, verticalAlign: "middle" }} />
              ) : wakeWord ? (
                <>
                  <span style={{ fontFamily: "var(--font-mono)", fontSize: 12, color: "#7C3AED", fontWeight: 600 }}>
                    &ldquo;{wakeWord}&rdquo;
                  </span>
                  {" "}
                  <span
                    style={{
                      display: "inline-block",
                      width: 6,
                      height: 6,
                      borderRadius: "50%",
                      background: isCalibrated ? "#16a34a" : "#f59e0b",
                      verticalAlign: "middle",
                      marginLeft: 4,
                    }}
                    title={isCalibrated ? `${transcriptions.length} calibration variant${transcriptions.length !== 1 ? "s" : ""}` : "Not calibrated"}
                  />
                  {" "}
                  <span style={{ fontSize: 11, color: "var(--color-text-tertiary)" }}>
                    {isCalibrated ? `${transcriptions.length} variant${transcriptions.length !== 1 ? "s" : ""}` : "not calibrated"}
                  </span>
                </>
              ) : (
                <span style={{ color: "var(--color-text-tertiary)", fontStyle: "italic" }}>not set</span>
              )}
            </span>
          </span>
          <span className="srow__control" onClick={(e) => e.stopPropagation()} onKeyDown={(e) => e.stopPropagation()}>
            {isCalibrated && (
              <button
                className="mrow__btn"
                type="button"
                disabled={resetting}
                onClick={handleResetCalibration}
                aria-label="Reset wake word calibration"
                title="Clear all calibration data — detector reverts to raw phrase matching"
                style={{ display: "flex", alignItems: "center", gap: 4, fontSize: 12 }}
              >
                {resetting
                  ? <Loader2 size={12} style={{ animation: "spin 1s linear infinite" }} />
                  : <RotateCcw size={12} strokeWidth={2} />
                }
                Reset
              </button>
            )}
          </span>
        </div>

        <Row
          label="Push-to-talk shortcut"
          sub="Cmd+Shift+V from anywhere"
          control={<Toggle on={true} />}
        />
      </Card>

      {/* ── Speech-to-text card ─────────────────────────────── */}
      <Card title="Speech-to-text">
        <div className="srow" style={{ cursor: "default" }}>
          <span className="srow__text">
            <span className="srow__label">Recognition model</span>
            <span className="srow__sub">
              {loading ? (
                <span style={{ display: "inline-block", height: 10, width: 120, background: "#e2e8f0", borderRadius: 4, verticalAlign: "middle" }} />
              ) : (
                sttLabel
              )}
            </span>
          </span>
          <span className="srow__control" onClick={(e) => e.stopPropagation()} onKeyDown={(e) => e.stopPropagation()}>
            <select
              aria-label="STT model"
              value={currentStt}
              onChange={(e) => handleSttChange(e.target.value)}
              disabled={loading}
              style={{
                height: 32,
                border: "1px solid var(--line, #e2e8f0)",
                borderRadius: 8,
                padding: "0 28px 0 10px",
                fontSize: 13,
                fontFamily: "var(--font-body, inherit)",
                background: "var(--tile-bg, #fff)",
                color: "var(--ink, #1a1a2e)",
                cursor: loading ? "not-allowed" : "pointer",
                appearance: "none",
                backgroundImage: "url(\"data:image/svg+xml,%3Csvg width='10' height='6' viewBox='0 0 10 6' fill='none' xmlns='http://www.w3.org/2000/svg'%3E%3Cpath d='M1 1l4 4 4-4' stroke='%238A8A8A' stroke-width='1.5' stroke-linecap='round' stroke-linejoin='round'/%3E%3C/svg%3E\")",
                backgroundRepeat: "no-repeat",
                backgroundPosition: "right 10px center",
                minWidth: 180,
              }}
            >
              {STT_OPTIONS.map((o) => (
                <option key={o.value} value={o.value}>{o.label}</option>
              ))}
            </select>
          </span>
        </div>

        <Row
          label="Language"
          control={
            <select
              aria-label="Recognition language"
              defaultValue="auto"
              style={{
                height: 32,
                border: "1px solid var(--line, #e2e8f0)",
                borderRadius: 8,
                padding: "0 28px 0 10px",
                fontSize: 13,
                fontFamily: "var(--font-body, inherit)",
                background: "var(--tile-bg, #fff)",
                color: "var(--ink, #1a1a2e)",
                cursor: "pointer",
                appearance: "none",
                backgroundImage: "url(\"data:image/svg+xml,%3Csvg width='10' height='6' viewBox='0 0 10 6' fill='none' xmlns='http://www.w3.org/2000/svg'%3E%3Cpath d='M1 1l4 4 4-4' stroke='%238A8A8A' stroke-width='1.5' stroke-linecap='round' stroke-linejoin='round'/%3E%3C/svg%3E\")",
                backgroundRepeat: "no-repeat",
                backgroundPosition: "right 10px center",
                minWidth: 120,
              }}
            >
              <option value="auto">Auto</option>
              <option value="en">English</option>
            </select>
          }
        />
      </Card>

      {/* ── Goose's voice ──────────────────────────────────── */}
      <Card title="Goose's voice">
        <VoicePicker
          voices={offeredVoices}
          selected={currentVoice}
          onSelect={handleTtsVoiceChange}
          pace={currentPace}
          onPaceChange={handlePaceChange}
          preview={preview}
          loading={loading}
        />
      </Card>

      {/* Quality is a different kind of decision from voice and pace: it picks
          the engine's precision and its download size, not how the pond
          sounds to you. It sits apart so it does not read as a third voice
          control. */}
      <Card title="Engine">
        <div className="srow" style={{ cursor: "default" }}>
          <span className="srow__text">
            <span className="srow__label">Voice quality</span>
            <span className="srow__sub">
              {loading ? (
                <span style={{ display: "inline-block", height: 10, width: 150, background: "#e2e8f0", borderRadius: 4, verticalAlign: "middle" }} />
              ) : (
                qualityTier.detail
              )}
            </span>
          </span>
          <span className="srow__control" onClick={(e) => e.stopPropagation()} onKeyDown={(e) => e.stopPropagation()}>
            <select
              aria-label="Voice quality"
              value={currentQuality}
              onChange={(e) => handleQualityChange(e.target.value)}
              disabled={loading}
              style={{
                height: 32,
                border: "1px solid var(--line, #e2e8f0)",
                borderRadius: 8,
                padding: "0 28px 0 10px",
                fontSize: 13,
                fontFamily: "var(--font-body, inherit)",
                background: "var(--tile-bg, #fff)",
                color: "var(--ink, #1a1a2e)",
                cursor: loading ? "not-allowed" : "pointer",
                appearance: "none",
                backgroundImage: "url(\"data:image/svg+xml,%3Csvg width='10' height='6' viewBox='0 0 10 6' fill='none' xmlns='http://www.w3.org/2000/svg'%3E%3Cpath d='M1 1l4 4 4-4' stroke='%238A8A8A' stroke-width='1.5' stroke-linecap='round' stroke-linejoin='round'/%3E%3C/svg%3E\")",
                backgroundRepeat: "no-repeat",
                backgroundPosition: "right 10px center",
                minWidth: 180,
              }}
            >
              {VOICE_QUALITY_TIERS.map((t) => (
                <option key={t.value} value={t.value}>{t.label} ({t.sizeMb} MB)</option>
              ))}
            </select>
          </span>
        </div>

        <Row
          label="Sound while it thinks"
          sub="A soft pulse between your question and the answer"
          control={
            // `key` forces a remount: Toggle reads `on` only once, and settings arrive after mount.
            <Toggle
              key={`thinking-tone-${thinkingTone}`}
              on={thinkingTone}
              onChange={handleThinkingToneChange}
            />
          }
        />
      </Card>

      {/* ── HubIco usage (invisible — keeps import alive for linter) ── */}
      {/* Using SICN paths inline on SVGs keeps the bundle clean */}
      <svg width={0} height={0} aria-hidden style={{ position: "absolute" }}>
        <symbol id="v-mic"     viewBox="0 0 24 24"><path d={SICN.mic}     fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" /></symbol>
        <symbol id="v-speaker" viewBox="0 0 24 24"><path d={SICN.speaker} fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" /></symbol>
        <symbol id="v-text"    viewBox="0 0 24 24"><path d={SICN.text}    fill="none" stroke="currentColor" strokeWidth={2} strokeLinecap="round" strokeLinejoin="round" /></symbol>
      </svg>
    </DetailShell>
  );
}
