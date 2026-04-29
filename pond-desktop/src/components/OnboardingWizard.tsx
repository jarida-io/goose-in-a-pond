import React, { useState, useEffect, useRef, useMemo, Fragment } from "react";
import { Switch, Input } from "@heroui/react";
import logoSrc from "../assets/logo.png";

// ─── Design tokens (inline — immune to Tailwind reset) ───────────────────────
const C = {
  purple: "#7C3AED",
  purpleMid: "#8C4BFF",
  purpleLight: "#A878FF",
  purpleBg: "#F4ECFF",
  purpleBorder: "rgba(140,75,255,0.25)",
  grey50: "#FAFAFA",
  grey100: "#F5F5F5",
  grey200: "#EDEDED",
  grey300: "#D4D4D4",
  grey400: "#A3A3A3",
  grey500: "#A3A3A3",
  grey600: "#737373",
  grey700: "#525252",
  grey800: "#262626",
  grey900: "#111111",
  white: "#FFFFFF",
  green: "#16A34A",
  red: "#DC2626",
} as const;

const FONT_HEAD = '"Syne", "DM Serif Display", Georgia, serif';
const FONT_BODY = 'Inter, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif';
const FONT_MONO = '"JetBrains Mono", "Fira Code", "SF Mono", Menlo, monospace';

// ─── Step metadata ────────────────────────────────────────────────────────────
interface StepMeta {
  id: string;
  label: string;
  caption: string;
}

const STEPS: StepMeta[] = [
  { id: "welcome",       label: "Welcome",          caption: "Say hi to Goose" },
  { id: "basics",        label: "About you",        caption: "Name and avatar" },
  { id: "location",      label: "Language & place",  caption: "Locale, weather" },
  { id: "accessibility", label: "Accessibility",     caption: "Speech, motion" },
  { id: "personality",   label: "Personality",       caption: "How Goose talks" },
  { id: "identity",      label: "Goose's identity", caption: "Name and voice" },
  { id: "wake",          label: "Wake word",         caption: "How to summon" },
  { id: "model",         label: "AI model",          caption: "The brain" },
  { id: "extensions",    label: "Extensions",        caption: "Built-ins" },
  { id: "done",          label: "All set",           caption: "Hello, world" },
];

const AVATARS = ["🦆", "🐧", "🦅", "🦜", "🐸", "🦉", "🐻", "🦊", "🐱", "🐶"];

interface LangOption {
  key: string;
  label: string;
}

const LANGUAGES: LangOption[] = [
  { key: "en", label: "English" },
  { key: "fr", label: "Francais" },
  { key: "es", label: "Espanol" },
  { key: "de", label: "Deutsch" },
  { key: "sw", label: "Kiswahili" },
  { key: "pt", label: "Portugues" },
  { key: "ja", label: "日本語" },
  { key: "zh", label: "中文" },
];

const TIMEZONES = [
  "Africa/Nairobi", "Europe/London", "Europe/Berlin", "Europe/Paris",
  "America/New_York", "America/Los_Angeles", "America/Chicago",
  "Asia/Tokyo", "Asia/Singapore", "Asia/Dubai",
  "Australia/Sydney", "Pacific/Auckland", "UTC",
];

interface PromptStyle {
  value: string;
  label: string;
  icon: string;
  desc: string;
}

const PROMPT_STYLES: PromptStyle[] = [
  { value: "balanced",  label: "Balanced",  icon: "\u2696\ufe0f", desc: "Warm and practical. Just enough detail." },
  { value: "concise",   label: "Concise",   icon: "\u26a1",       desc: "Short, action-first. Skips the small talk." },
  { value: "technical", label: "Technical",  icon: "\ud83d\udd27", desc: "Step-by-step. Detailed narration for tinkerers." },
  { value: "warm",      label: "Warm",       icon: "\u2600\ufe0f", desc: "Conversational, like a helpful neighbour." },
];

interface TtsVoice {
  value: string;
  label: string;
  accent: string;
  file: string;
}

const TTS_VOICES: TtsVoice[] = [
  { value: "amy",      label: "Amy",      accent: "British \u00b7 Female",  file: "piper-amy.onnx" },
  { value: "ryan",     label: "Ryan",     accent: "American \u00b7 Male",   file: "piper-ryan.onnx" },
  { value: "kathleen", label: "Kathleen", accent: "Irish \u00b7 Female",    file: "piper-kathleen.onnx" },
  { value: "libritts", label: "LibriTTS", accent: "Neutral \u00b7 Mixed",   file: "libritts-r.onnx" },
];

interface WakePreset {
  value: string;
  label: string;
  desc: string;
}

const WAKE_PRESETS: WakePreset[] = [
  { value: "goose",       label: '"Goose"',       desc: "Short and memorable." },
  { value: "hey goose",   label: '"Hey Goose"',   desc: "Natural call-and-response." },
  { value: "ok computer", label: '"OK Computer"', desc: "Classic command style." },
  { value: "custom",      label: "Custom phrase",  desc: "Say anything you like." },
];

interface ProviderOption {
  key: string;
  label: string;
  icon: string;
  desc: string;
  recommended?: boolean;
}

const PROVIDERS: ProviderOption[] = [
  { key: "llamafile", label: "Llamafile",  icon: "\ud83e\udd99", desc: "Self-contained. Starts automatically.", recommended: true },
  { key: "ollama",    label: "Ollama",     icon: "\ud83d\udc11", desc: "Use models you've already set up." },
  { key: "local",     label: "GGUF file",  icon: "\ud83d\udce6", desc: "Load a GGUF directly from disk." },
];

interface ModelOption {
  name: string;
  size: string;
  tag: string;
  downloaded: boolean;
}

const MODELS: Record<string, ModelOption[]> = {
  llamafile: [
    { name: "llama-3.2-3b",        size: "2.0 GB", tag: "Balanced", downloaded: true },
    { name: "gemma-2b",            size: "1.6 GB", tag: "Fast",     downloaded: true },
    { name: "qwen2.5-7b-instruct", size: "4.4 GB", tag: "Capable",  downloaded: false },
  ],
  ollama: [
    { name: "llama3.2",  size: "installed", tag: "Balanced", downloaded: true },
    { name: "mistral",   size: "installed", tag: "Fast",     downloaded: true },
    { name: "qwen2.5",   size: "available", tag: "Capable",  downloaded: false },
  ],
  local: [
    { name: "phi-3-mini.gguf",    size: "2.2 GB", tag: "Fast",     downloaded: true },
    { name: "mistral-7b-q4.gguf", size: "4.1 GB", tag: "Balanced", downloaded: false },
  ],
};

// ─── Draft state ──────────────────────────────────────────────────────────────
interface OnboardingDraft {
  userName: string;
  preferredName: string;
  birthday: string;
  avatar: string;
  language: string;
  timezone: string;
  locationName: string;
  enableWeather: boolean;
  atypicalSpeech: boolean;
  slowSpeech: boolean;
  highContrast: boolean;
  reduceMotion: boolean;
  promptStyle: string;
  personality: string;
  assistantName: string;
  ttsVoice: string;
  ttsRate: number;
  wakeWord: string;
  wakeWordCustom: string;
  llmProvider: string;
  llmModel: string;
  enableMcpMemory: boolean;
  enableHomeAssistant: boolean;
  enableCalendar: boolean;
}

const DEFAULT_DRAFT: OnboardingDraft = {
  userName: "",
  preferredName: "",
  birthday: "",
  avatar: "\ud83e\udd86",
  language: "en",
  timezone: Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC",
  locationName: "",
  enableWeather: false,
  atypicalSpeech: false,
  slowSpeech: false,
  highContrast: false,
  reduceMotion: false,
  promptStyle: "balanced",
  personality: "friendly and helpful",
  assistantName: "Goose",
  ttsVoice: "amy",
  ttsRate: 50,
  wakeWord: "goose",
  wakeWordCustom: "",
  llmProvider: "llamafile",
  llmModel: "llama-3.2-3b",
  enableMcpMemory: true,
  enableHomeAssistant: false,
  enableCalendar: false,
};

// ─── Props ────────────────────────────────────────────────────────────────────
interface OnboardingWizardProps {
  onComplete: () => void;
}

// ─── Primitives ───────────────────────────────────────────────────────────────

interface RadioCardProps {
  selected: boolean;
  onClick: () => void;
  children: React.ReactNode;
  style?: React.CSSProperties;
}

function RadioCard({ selected, onClick, children, style = {} }: RadioCardProps) {
  return (
    <div
      onClick={onClick}
      style={{
        display: "flex", alignItems: "flex-start", gap: 12,
        padding: "12px 14px",
        background: selected ? C.purpleBg : C.white,
        border: `1.5px solid ${selected ? C.purpleMid : C.grey200}`,
        borderRadius: 10, cursor: "pointer",
        transition: "border-color 0.12s, background 0.12s",
        boxShadow: selected ? "0 0 0 3px rgba(140,75,255,0.12)" : "none",
        ...style,
      }}
    >
      {/* Custom radio dot */}
      <div style={{
        flexShrink: 0, marginTop: 3,
        width: 16, height: 16, borderRadius: "50%",
        border: `2px solid ${selected ? C.purpleMid : C.grey300}`,
        background: selected ? C.purpleMid : C.white,
        display: "flex", alignItems: "center", justifyContent: "center",
        boxShadow: selected ? `inset 0 0 0 3px ${C.white}` : "none",
        transition: "border-color 0.12s, background 0.12s",
      }} />
      {children}
    </div>
  );
}

interface ToggleRowProps {
  icon?: string;
  title: string;
  desc: string;
  checked: boolean;
  onChange: (val: boolean) => void;
  badge?: string;
}

function ToggleRow({ icon, title, desc, checked, onChange, badge }: ToggleRowProps) {
  return (
    <div style={{
      display: "flex", alignItems: "center", gap: 14,
      padding: "14px 16px",
      background: C.white,
      border: `1px solid ${C.grey200}`,
      borderRadius: 12,
    }}>
      {icon && <span style={{ fontSize: 22, flexShrink: 0 }}>{icon}</span>}
      <div style={{ flex: 1, minWidth: 0 }}>
        <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 3 }}>
          <strong style={{ fontSize: 13.5, color: C.grey900, fontWeight: 600 }}>{title}</strong>
          {badge && (
            <span style={{
              fontSize: 11, color: C.purple, background: C.purpleBg,
              border: `1px solid ${C.purpleBorder}`, padding: "1px 7px",
              borderRadius: 999, fontWeight: 500,
            }}>{badge}</span>
          )}
        </div>
        <p style={{ margin: 0, fontSize: 12.5, color: C.grey600, lineHeight: 1.5 }}>{desc}</p>
      </div>
      <Switch isSelected={checked} onChange={onChange}>
        <Switch.Control><Switch.Thumb /></Switch.Control>
      </Switch>
    </div>
  );
}

interface FormLabelProps {
  children: React.ReactNode;
  optional?: boolean;
  sub?: string;
}

function FormLabel({ children, optional, sub }: FormLabelProps) {
  return (
    <div style={{ display: "flex", alignItems: "baseline", gap: 8, marginBottom: 6 }}>
      <span style={{ fontSize: 12, fontWeight: 600, color: C.grey700, textTransform: "uppercase", letterSpacing: "0.05em" }}>{children}</span>
      {optional && <span style={{ fontSize: 11.5, color: C.grey500, textTransform: "none", letterSpacing: 0, fontWeight: 400 }}>(optional)</span>}
      {sub && <span style={{ fontSize: 11.5, color: C.grey500, textTransform: "none", letterSpacing: 0 }}>{sub}</span>}
    </div>
  );
}

function Lead({ children }: { children: React.ReactNode }) {
  return <p style={{ margin: "0 0 20px", fontSize: 14, color: C.grey600, lineHeight: 1.6, maxWidth: "56ch" }}>{children}</p>;
}

// ─── Side rail ────────────────────────────────────────────────────────────────
interface StepRailProps {
  stepIndex: number;
  onJump: (i: number) => void;
}

function StepRail({ stepIndex, onJump }: StepRailProps) {
  return (
    <aside style={{
      background: C.white,
      borderRight: `1px solid ${C.grey200}`,
      display: "flex", flexDirection: "column",
      padding: "20px 16px 16px",
      overflowY: "auto", gap: 4,
    }}>
      {/* Brand */}
      <div style={{
        display: "flex", alignItems: "center", gap: 10,
        paddingBottom: 16, marginBottom: 8,
        borderBottom: `1px solid ${C.grey100}`,
      }}>
        <img src={logoSrc} alt="Goose In A Pond" style={{ height: 32, width: 32, objectFit: "contain" }} />
        <div>
          <div style={{ fontWeight: 700, fontSize: 13, color: C.grey900, fontFamily: FONT_BODY }}>Goose In A Pond</div>
          <div style={{ fontSize: 11, color: C.grey500 }}>First-time setup</div>
        </div>
      </div>

      {/* Steps */}
      <ol style={{ listStyle: "none", padding: 0, margin: 0, display: "flex", flexDirection: "column", gap: 2, flex: 1 }}>
        {STEPS.map((s, i) => {
          const done = i < stepIndex;
          const active = i === stepIndex;
          return (
            <li key={s.id}>
              <button
                type="button"
                onClick={() => done && onJump(i)}
                disabled={!done && !active}
                style={{
                  display: "flex", alignItems: "center", gap: 10,
                  width: "100%", padding: "7px 8px", borderRadius: 8,
                  background: active ? "rgba(140,75,255,0.07)" : "transparent",
                  border: "none", cursor: done ? "pointer" : "default",
                  textAlign: "left",
                }}
              >
                {/* Dot */}
                <span style={{
                  flexShrink: 0, width: 22, height: 22, borderRadius: "50%",
                  display: "flex", alignItems: "center", justifyContent: "center",
                  fontSize: 10.5, fontWeight: 700,
                  background: done || active ? C.purpleMid : C.grey100,
                  color: done || active ? C.white : C.grey500,
                  border: `1.5px solid ${done || active ? C.purpleMid : C.grey200}`,
                  boxShadow: active ? "0 0 0 4px rgba(140,75,255,0.15)" : "none",
                }}>
                  {done
                    ? (
                      <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3.5" strokeLinecap="round" strokeLinejoin="round">
                        <path d="M5 12l5 5 9-11" />
                      </svg>
                    )
                    : i + 1}
                </span>
                {/* Text */}
                <span style={{ display: "flex", flexDirection: "column", gap: 1 }}>
                  <span style={{ fontSize: 12.5, fontWeight: active ? 700 : 500, color: active ? C.grey900 : done ? C.grey700 : C.grey500 }}>{s.label}</span>
                  <span style={{ fontSize: 11, color: C.grey400 }}>{s.caption}</span>
                </span>
              </button>
            </li>
          );
        })}
      </ol>

      {/* Footer */}
      <div style={{ paddingTop: 12, borderTop: `1px solid ${C.grey100}` }}>
        <span style={{
          display: "inline-flex", alignItems: "center", gap: 6,
          fontSize: 11.5, color: C.green,
          background: "rgba(22,163,74,0.08)",
          border: "1px solid rgba(22,163,74,0.2)",
          padding: "4px 10px", borderRadius: 999,
        }}>
          <span style={{ width: 6, height: 6, borderRadius: "50%", background: C.green, display: "inline-block" }} />
          Connected &middot; pond.local
        </span>
      </div>
    </aside>
  );
}

// ─── Step header ──────────────────────────────────────────────────────────────
interface StepHeaderProps {
  stepIndex: number;
}

function StepHeader({ stepIndex }: StepHeaderProps) {
  const total = STEPS.length - 2;
  const num = stepIndex;
  if (stepIndex === 0 || stepIndex === STEPS.length - 1) return null;
  const meta = STEPS[stepIndex];
  const pct = Math.round((num / total) * 100);
  return (
    <div style={{ marginBottom: 28 }}>
      <div style={{ fontSize: 11, fontWeight: 700, color: C.purpleMid, textTransform: "uppercase", letterSpacing: "0.08em", marginBottom: 8 }}>
        Step {num} of {total}
      </div>
      <h1 style={{ margin: "0 0 16px", fontFamily: FONT_HEAD, fontWeight: 700, fontSize: 30, letterSpacing: "-0.02em", color: C.grey900, lineHeight: 1.15 }}>
        {meta.label}
      </h1>
      <div style={{ height: 3, borderRadius: 999, background: C.grey100, maxWidth: 260, overflow: "hidden" }}>
        <div style={{ height: "100%", width: `${pct}%`, background: C.purpleMid, borderRadius: 999, transition: "width 0.4s ease" }} />
      </div>
    </div>
  );
}

// ─── Actions bar ─────────────────────────────────────────────────────────────
interface ActionsProps {
  onBack: () => void;
  onSkip?: () => void;
  onNext: () => void;
  nextLabel?: string;
  disabled?: boolean;
}

function Actions({ onBack, onSkip, onNext, nextLabel = "Continue", disabled = false }: ActionsProps) {
  return (
    <div style={{
      display: "flex", alignItems: "center", gap: 10,
      marginTop: 36, paddingTop: 20,
      borderTop: `1px solid ${C.grey100}`,
    }}>
      <button
        type="button"
        onClick={onBack}
        style={{
          padding: "8px 16px", borderRadius: 8, border: `1px solid ${C.grey200}`,
          background: C.white, color: C.grey600, fontSize: 13.5, fontWeight: 500,
          cursor: "pointer", fontFamily: FONT_BODY,
        }}
      >&#8592; Back</button>
      <div style={{ flex: 1 }} />
      {onSkip && (
        <button
          type="button"
          onClick={onSkip}
          style={{
            padding: "8px 14px", border: "none", background: "transparent",
            color: C.grey500, fontSize: 13, fontWeight: 500, cursor: "pointer", fontFamily: FONT_BODY,
          }}
        >Skip for now</button>
      )}
      <button
        type="button"
        onClick={onNext}
        disabled={disabled}
        style={{
          padding: "9px 22px", borderRadius: 8, border: "none",
          background: disabled ? C.grey200 : C.purpleMid,
          color: disabled ? C.grey500 : C.white, fontSize: 13.5, fontWeight: 600,
          cursor: disabled ? "not-allowed" : "pointer", fontFamily: FONT_BODY,
          boxShadow: disabled ? "none" : "0 2px 8px rgba(124,58,237,0.3)",
          transition: "opacity 0.15s",
        }}
      >{nextLabel} &#8594;</button>
    </div>
  );
}

// ─── Step 0 — Welcome ─────────────────────────────────────────────────────────
interface StepWelcomeProps {
  onNext: () => void;
}

function StepWelcome({ onNext }: StepWelcomeProps) {
  const sysRows = [
    { label: "Device",  value: "pond.local \u00b7 192.168.1.42" },
    { label: "System",  value: "macOS 14.5 \u00b7 Apple Silicon" },
    { label: "Server",  value: "pond-server v0.1.0 \u00b7 :4000" },
  ];
  return (
    <div style={{ display: "flex", flexDirection: "column", alignItems: "center", textAlign: "center", padding: "28px 16px 24px", maxWidth: 500, margin: "0 auto" }}>
      <img src={logoSrc} alt="Goose In A Pond" style={{ height: 80, width: 80, objectFit: "contain", marginBottom: 20 }} />
      <h1 style={{ margin: "0 0 8px", fontFamily: FONT_HEAD, fontWeight: 700, fontSize: 32, letterSpacing: "-0.02em", color: C.grey900, lineHeight: 1.1 }}>
        Welcome to Goose In A Pond
      </h1>
      <p style={{ margin: "0 0 22px", fontSize: 15, color: C.grey600 }}>Your private AI assistant — on your home network.</p>

      <p style={{ margin: "0 0 22px", fontSize: 14, color: C.grey700, lineHeight: 1.65, textAlign: "left", maxWidth: "44ch" }}>
        Goose runs entirely on your own hardware. It thinks locally, speaks locally,
        and never sends your data anywhere. Once set up, just say the wake word and Goose is ready.
      </p>

      <div style={{ display: "flex", gap: 8, justifyContent: "center", marginBottom: 28 }}>
        {([
          ["\ud83d\udd12", "Fully offline"],
          ["\ud83c\udf99\ufe0f", "Voice-first"],
          ["\ud83c\udfe0", "Privacy by design"],
        ] as const).map(([icon, text]) => (
          <span key={text} style={{
            display: "inline-flex", alignItems: "center", gap: 5, whiteSpace: "nowrap",
            padding: "5px 12px", borderRadius: 999, fontSize: 12.5, fontWeight: 500,
            color: C.purple, background: C.purpleBg, border: `1px solid ${C.purpleBorder}`,
          }}>{icon} {text}</span>
        ))}
      </div>

      <div style={{ display: "flex", flexDirection: "column", alignItems: "center", gap: 10, marginBottom: 32 }}>
        <button
          type="button"
          onClick={onNext}
          style={{
            padding: "12px 32px", borderRadius: 10, border: "none",
            background: C.purpleMid,
            color: C.white, fontSize: 15, fontWeight: 700,
            cursor: "pointer", fontFamily: FONT_BODY,
          }}
        >Get started &#8594;</button>
        <span style={{ fontSize: 12, color: C.grey500 }}>Takes about 3 minutes</span>
      </div>

      <div style={{ width: "100%", borderTop: `1px solid ${C.grey200}` }}>
        {sysRows.map((r, i) => (
          <div key={r.label} style={{
            display: "flex", justifyContent: "space-between", alignItems: "center",
            padding: "10px 4px",
            borderBottom: i < sysRows.length - 1 ? `1px solid ${C.grey100}` : "none",
            fontSize: 12.5,
          }}>
            <span style={{ color: C.grey500, fontWeight: 500 }}>{r.label}</span>
            <code style={{ fontSize: 12, color: C.grey800, background: C.grey100, padding: "2px 8px", borderRadius: 5, fontFamily: FONT_MONO }}>{r.value}</code>
          </div>
        ))}
      </div>
    </div>
  );
}

// ─── Step 1 — Basics ──────────────────────────────────────────────────────────
interface StepDraftProps {
  draft: OnboardingDraft;
  patch: (p: Partial<OnboardingDraft>) => void;
}

function StepBasics({ draft, patch }: StepDraftProps) {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 20 }}>
      <Lead>Goose uses this to greet you by name and personalise responses.</Lead>

      <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 14 }}>
        <Input label="Your name" placeholder="e.g. Jack Smith" variant="bordered" value={draft.userName} onValueChange={(v: string) => patch({ userName: v })} />
        <Input label="Preferred name" placeholder="What should Goose call you?" variant="bordered" value={draft.preferredName} onValueChange={(v: string) => patch({ preferredName: v })} />
      </div>

      <div>
        <FormLabel optional>Avatar</FormLabel>
        <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
          {AVATARS.map((em) => (
            <button key={em} type="button" onClick={() => patch({ avatar: em })} style={{
              width: 44, height: 44, borderRadius: 10,
              border: `2px solid ${draft.avatar === em ? C.purpleMid : C.grey200}`,
              background: draft.avatar === em ? C.purpleBg : C.white,
              fontSize: 22, cursor: "pointer", display: "flex", alignItems: "center", justifyContent: "center",
              transition: "border-color 0.12s, background 0.12s, transform 0.1s",
              transform: draft.avatar === em ? "scale(1.1)" : "scale(1)",
              boxShadow: draft.avatar === em ? "0 0 0 3px rgba(140,75,255,0.18)" : "none",
            }}>{em}</button>
          ))}
        </div>
      </div>

      <div style={{ maxWidth: 280 }}>
        <Input label="Birthday" type="date" variant="bordered" value={draft.birthday} onValueChange={(v: string) => patch({ birthday: v })} />
        <p style={{ margin: "4px 0 0", fontSize: 12, color: C.grey500 }}>Optional — Goose will wish you well on the day</p>
      </div>
    </div>
  );
}

// ─── Step 2 — Location ────────────────────────────────────────────────────────
function StepLocation({ draft, patch }: StepDraftProps) {
  const [detecting, setDetecting] = useState(false);
  function detect() {
    setDetecting(true);
    setTimeout(() => {
      patch({ locationName: "Nairobi", timezone: "Africa/Nairobi", enableWeather: true });
      setDetecting(false);
    }, 900);
  }

  const selectStyle: React.CSSProperties = {
    width: "100%", height: 40, padding: "0 12px",
    border: `1.5px solid ${C.grey200}`, borderRadius: 8,
    background: C.white, color: C.grey800,
    fontSize: 14, fontFamily: FONT_BODY,
    outline: "none", cursor: "pointer",
    appearance: "none",
    backgroundImage: `url("data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' width='12' height='12' viewBox='0 0 12 12'%3E%3Cpath d='M3 5l3 3 3-3' fill='none' stroke='%23737373' stroke-width='1.5' stroke-linecap='round'/%3E%3C/svg%3E")`,
    backgroundRepeat: "no-repeat",
    backgroundPosition: "right 12px center",
  };

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 20 }}>
      <Lead>Used for voice responses, weather, and time-aware answers.</Lead>

      <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 14 }}>
        <div>
          <FormLabel>Language</FormLabel>
          <select
            value={draft.language}
            onChange={(e) => patch({ language: e.target.value })}
            style={selectStyle}
          >
            {LANGUAGES.map((l) => (
              <option key={l.key} value={l.key}>{l.label}</option>
            ))}
          </select>
        </div>
        <div>
          <FormLabel>Timezone</FormLabel>
          <select
            value={draft.timezone}
            onChange={(e) => patch({ timezone: e.target.value })}
            style={selectStyle}
          >
            {TIMEZONES.map((t) => (
              <option key={t} value={t}>{t}</option>
            ))}
          </select>
        </div>
      </div>

      <div>
        <FormLabel optional>City / location name</FormLabel>
        <div style={{ display: "flex", gap: 10 }}>
          <div style={{ flex: 1 }}>
            <Input placeholder="e.g. Nairobi, London, New York" variant="bordered" value={draft.locationName} onValueChange={(v: string) => patch({ locationName: v })} />
          </div>
          <button type="button" onClick={detect} disabled={detecting} style={{
            flexShrink: 0, padding: "0 16px", height: 40, borderRadius: 8,
            border: `1px solid ${C.grey200}`,
            background: detecting ? C.grey100 : C.white, color: C.purple,
            fontSize: 13, fontWeight: 600,
            cursor: detecting ? "wait" : "pointer", fontFamily: FONT_BODY, whiteSpace: "nowrap",
          }}>{detecting ? "\ud83d\udccd Detecting\u2026" : "\ud83d\udccd Auto-detect"}</button>
        </div>
      </div>

      <ToggleRow
        icon="\ud83c\udf24\ufe0f"
        title="Enable live weather"
        desc="Pulls forecast from Open-Meteo. Approximate network location only — no GPS needed."
        checked={draft.enableWeather}
        onChange={(v) => patch({ enableWeather: v })}
      />
    </div>
  );
}

// ─── Step 3 — Accessibility ───────────────────────────────────────────────────
function StepAccessibility({ draft, patch }: StepDraftProps) {
  const items = [
    { key: "atypicalSpeech" as const, icon: "\ud83d\udcac", title: "Atypical speech support", desc: "Goose waits longer before responding — helpful for stutter, pauses, or non-standard speech patterns." },
    { key: "slowSpeech" as const,     icon: "\ud83d\udc22", title: "Slow speech mode",        desc: "Goose speaks at a reduced pace so responses are easier to follow." },
    { key: "highContrast" as const,   icon: "\ud83d\udd32", title: "High contrast",           desc: "Increases visual contrast across the dashboard interface." },
    { key: "reduceMotion" as const,   icon: "\ud83d\uded1", title: "Reduce motion",           desc: "Disables animations and transitions throughout the interface." },
  ];
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
      <Lead>All options can be changed at any time in Settings &rarr; Accessibility.</Lead>
      {items.map((it) => (
        <ToggleRow
          key={it.key}
          icon={it.icon}
          title={it.title}
          desc={it.desc}
          checked={draft[it.key]}
          onChange={(v) => patch({ [it.key]: v })}
        />
      ))}
    </div>
  );
}

// ─── Step 4 — Personality ─────────────────────────────────────────────────────
function StepPersonality({ draft, patch }: StepDraftProps) {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 20 }}>
      <Lead>Choose how Goose talks to you. You can change this in Settings at any time.</Lead>

      <div>
        <FormLabel>Conversation style</FormLabel>
        <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 8 }}>
          {PROMPT_STYLES.map((s) => (
            <RadioCard key={s.value} selected={draft.promptStyle === s.value} onClick={() => patch({ promptStyle: s.value })}>
              <div style={{ display: "flex", flexDirection: "column", gap: 3 }}>
                <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
                  <span style={{ fontSize: 16 }}>{s.icon}</span>
                  <strong style={{ fontSize: 13.5, color: C.grey900, fontWeight: 600 }}>{s.label}</strong>
                </div>
                <span style={{ fontSize: 12, color: C.grey600, lineHeight: 1.45 }}>{s.desc}</span>
              </div>
            </RadioCard>
          ))}
        </div>
      </div>

      <div>
        <FormLabel optional>Personality hint</FormLabel>
        <p style={{ margin: "0 0 8px", fontSize: 12.5, color: C.grey500 }}>A few words to shape Goose's character, e.g. "curious and warm".</p>
        <textarea
          value={draft.personality}
          onChange={(e) => patch({ personality: e.target.value })}
          placeholder="friendly and helpful"
          rows={2}
          style={{
            width: "100%", boxSizing: "border-box",
            padding: "10px 12px", border: `1.5px solid ${C.grey200}`, borderRadius: 8,
            fontSize: 14, color: C.grey800, background: C.white,
            fontFamily: FONT_BODY, resize: "vertical", outline: "none",
          }}
        />
      </div>
    </div>
  );
}

// ─── Step 5 — Identity ────────────────────────────────────────────────────────
function StepIdentity({ draft, patch }: StepDraftProps) {
  const [playing, setPlaying] = useState<string | null>(null);
  function preview(v: string) {
    setPlaying(v);
    setTimeout(() => setPlaying(null), 1400);
  }

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 22 }}>
      <Lead>Give your assistant a name and pick a voice. More household members can be added later.</Lead>

      <Input label="Assistant name" placeholder="Goose" variant="bordered" value={draft.assistantName} onValueChange={(v: string) => patch({ assistantName: v })} />

      <div>
        <FormLabel>Voice</FormLabel>
        <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
          {TTS_VOICES.map((v) => {
            const sel = draft.ttsVoice === v.value;
            return (
              <div key={v.value} onClick={() => patch({ ttsVoice: v.value })} style={{
                display: "flex", alignItems: "center", gap: 12,
                padding: "11px 14px",
                background: sel ? C.purpleBg : C.white,
                border: `1.5px solid ${sel ? C.purpleMid : C.grey200}`,
                borderRadius: 10, cursor: "pointer",
                boxShadow: sel ? "0 0 0 3px rgba(140,75,255,0.1)" : "none",
              }}>
                {/* Radio dot */}
                <div style={{
                  flexShrink: 0, width: 16, height: 16, borderRadius: "50%",
                  border: `2px solid ${sel ? C.purpleMid : C.grey300}`,
                  background: sel ? C.purpleMid : C.white,
                  boxShadow: sel ? `inset 0 0 0 3px ${C.white}` : "none",
                }} />
                <div style={{ flex: 1 }}>
                  <div style={{ fontSize: 13.5, fontWeight: 600, color: C.grey900, marginBottom: 2 }}>{v.label}</div>
                  <div style={{ fontSize: 12, color: C.grey500 }}>{v.accent} &middot; <code style={{ fontFamily: FONT_MONO, fontSize: 11 }}>{v.file}</code></div>
                </div>
                <button type="button" onClick={(e) => { e.stopPropagation(); preview(v.value); }} style={{
                  width: 32, height: 32, borderRadius: "50%",
                  border: `1px solid ${C.grey200}`,
                  background: playing === v.value ? C.purpleMid : C.white,
                  color: playing === v.value ? C.white : C.grey600,
                  cursor: "pointer", fontSize: 12, display: "flex", alignItems: "center", justifyContent: "center", flexShrink: 0,
                }}>
                  {playing === v.value ? "\u25a0" : "\u25b6"}
                </button>
              </div>
            );
          })}
        </div>
      </div>

      <div>
        <FormLabel sub={draft.ttsRate < 40 ? "\u00b7 Slower" : draft.ttsRate > 65 ? "\u00b7 Faster" : "\u00b7 Normal"}>Speaking rate</FormLabel>
        <input
          type="range"
          min={0}
          max={100}
          step={5}
          value={draft.ttsRate}
          onChange={(e) => patch({ ttsRate: Number(e.target.value) })}
          style={{
            width: "100%", height: 6, appearance: "none",
            background: `linear-gradient(to right, ${C.purpleMid} ${draft.ttsRate}%, ${C.grey200} ${draft.ttsRate}%)`,
            borderRadius: 999, outline: "none", cursor: "pointer",
          }}
        />
      </div>
    </div>
  );
}

// ─── Step 6 — Wake Word ───────────────────────────────────────────────────────
function StepWakeWord({ draft, patch }: StepDraftProps) {
  const [calibrating, setCalibrating] = useState(false);
  const [level, setLevel] = useState(0);
  const [samples, setSamples] = useState(0);
  const tick = useRef<ReturnType<typeof setInterval> | null>(null);
  useEffect(() => () => { if (tick.current) clearInterval(tick.current); }, []);

  function startCalib() {
    setCalibrating(true);
    setLevel(0);
    setSamples(0);
    tick.current = setInterval(() => {
      setLevel(0.15 + Math.random() * 0.85);
      setSamples((s) => {
        const n = s + 1;
        if (n >= 12) {
          if (tick.current) clearInterval(tick.current);
          setCalibrating(false);
          setLevel(0);
        }
        return n;
      });
    }, 220);
  }

  const isCustom = draft.wakeWord === "custom";
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 20 }}>
      <Lead>Say this phrase to activate Goose when it's listening in voice mode.</Lead>

      <div>
        <FormLabel>Wake phrase</FormLabel>
        <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 8 }}>
          {WAKE_PRESETS.map((p) => (
            <RadioCard key={p.value} selected={draft.wakeWord === p.value} onClick={() => patch({ wakeWord: p.value })}>
              <div>
                <strong style={{ display: "block", fontSize: 13.5, color: C.grey900, fontWeight: 600, marginBottom: 2 }}>{p.label}</strong>
                <span style={{ fontSize: 12, color: C.grey600 }}>{p.desc}</span>
              </div>
            </RadioCard>
          ))}
        </div>
      </div>

      {isCustom && (
        <Input label="Your custom phrase" placeholder="e.g. hey duck, morning pond" variant="bordered" value={draft.wakeWordCustom} onValueChange={(v: string) => patch({ wakeWordCustom: v })} />
      )}

      {/* Calibration */}
      <div style={{ background: C.white, border: `1px solid ${C.grey200}`, borderRadius: 12, padding: 16, display: "flex", flexDirection: "column", gap: 14 }}>
        <div style={{ display: "flex", justifyContent: "space-between", alignItems: "flex-start", gap: 16 }}>
          <div>
            <div style={{ fontSize: 13.5, fontWeight: 600, color: C.grey900, marginBottom: 4 }}>{"\ud83c\udf99\ufe0f"} Calibrate microphone</div>
            <p style={{ margin: 0, fontSize: 12.5, color: C.grey600, lineHeight: 1.5, maxWidth: "36ch" }}>Say your wake word a few times so Goose learns your voice pattern.</p>
          </div>
          <button type="button" onClick={startCalib} disabled={calibrating} style={{
            flexShrink: 0, padding: "8px 16px", borderRadius: 8, border: "none",
            background: calibrating ? C.grey100 : C.purpleMid,
            color: calibrating ? C.grey500 : C.white,
            fontSize: 13, fontWeight: 600, cursor: calibrating ? "wait" : "pointer", fontFamily: FONT_BODY,
            boxShadow: calibrating ? "none" : "0 2px 8px rgba(124,58,237,0.25)",
          }}>{calibrating ? "Listening\u2026" : samples >= 12 ? "Re-calibrate" : "Start"}</button>
        </div>

        {/* Level meter */}
        <div style={{ display: "flex", gap: 3, alignItems: "flex-end", height: 40, padding: "0 2px" }}>
          {Array.from({ length: 20 }).map((_, i) => {
            const t = i / 19;
            const on = calibrating && level > t;
            return (
              <div key={i} style={{
                flex: 1, borderRadius: 2,
                height: on ? `${40 + level * 60}%` : "25%",
                background: on ? `hsl(${270 - t * 40}, 80%, 60%)` : C.grey200,
                transition: "height 0.15s, background 0.15s",
              }} />
            );
          })}
        </div>

        {/* Progress */}
        <div style={{ display: "flex", alignItems: "center", gap: 12 }}>
          <span style={{ fontSize: 12, color: C.grey600, flexShrink: 0 }}>Samples</span>
          <div style={{ flex: 1, height: 4, borderRadius: 999, background: C.grey100, overflow: "hidden" }}>
            <div style={{ height: "100%", width: `${(samples / 12) * 100}%`, background: C.purpleMid, borderRadius: 999, transition: "width 0.25s" }} />
          </div>
          <span style={{ fontSize: 12, color: C.grey600, flexShrink: 0, fontFamily: FONT_MONO }}>{samples}/12</span>
        </div>
      </div>
    </div>
  );
}

// ─── Step 7 — Model ───────────────────────────────────────────────────────────
function StepModel({ draft, patch }: StepDraftProps) {
  const list = MODELS[draft.llmProvider] || [];
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 22 }}>
      <Lead>This is the brain behind Goose's responses. Not sure? <strong>Llamafile</strong> handles everything automatically.</Lead>

      <div>
        <FormLabel>AI provider</FormLabel>
        <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
          {PROVIDERS.map((p) => (
            <RadioCard key={p.key} selected={draft.llmProvider === p.key} onClick={() => {
              const first = MODELS[p.key]?.[0]?.name || "";
              patch({ llmProvider: p.key, llmModel: first });
            }}>
              <div style={{ flex: 1 }}>
                <div style={{ display: "flex", alignItems: "center", gap: 8, marginBottom: 3 }}>
                  <span style={{ fontSize: 18 }}>{p.icon}</span>
                  <strong style={{ fontSize: 14, color: C.grey900, fontWeight: 600 }}>{p.label}</strong>
                  {p.recommended && (
                    <span style={{
                      fontSize: 11, color: C.purple, background: C.purpleBg,
                      border: `1px solid ${C.purpleBorder}`, padding: "1px 8px",
                      borderRadius: 999, fontWeight: 600,
                    }}>Recommended</span>
                  )}
                </div>
                <span style={{ fontSize: 12.5, color: C.grey600 }}>{p.desc}</span>
              </div>
            </RadioCard>
          ))}
        </div>
      </div>

      <div>
        <FormLabel>Model</FormLabel>
        <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
          {list.map((m) => {
            const sel = draft.llmModel === m.name;
            return (
              <div key={m.name} onClick={() => patch({ llmModel: m.name })} style={{
                display: "flex", alignItems: "center", gap: 12,
                padding: "12px 14px",
                background: sel ? C.purpleBg : C.white,
                border: `1.5px solid ${sel ? C.purpleMid : C.grey200}`,
                borderRadius: 10, cursor: "pointer",
              }}>
                <div style={{
                  flexShrink: 0, width: 16, height: 16, borderRadius: "50%",
                  border: `2px solid ${sel ? C.purpleMid : C.grey300}`,
                  background: sel ? C.purpleMid : C.white,
                  boxShadow: sel ? `inset 0 0 0 3px ${C.white}` : "none",
                }} />
                <div style={{ flex: 1 }}>
                  <code style={{ fontSize: 13, color: C.grey900, fontWeight: 600, fontFamily: FONT_MONO }}>{m.name}</code>
                  <span style={{ marginLeft: 10, fontSize: 12, color: C.grey500 }}>{m.size}</span>
                </div>
                <div style={{ display: "flex", gap: 6 }}>
                  <span style={{ fontSize: 11.5, padding: "2px 8px", borderRadius: 999, background: C.grey100, color: C.grey700, fontWeight: 500 }}>{m.tag}</span>
                  <span style={{
                    fontSize: 11.5, padding: "2px 8px", borderRadius: 999,
                    background: m.downloaded ? "rgba(22,163,74,0.1)" : C.grey100,
                    color: m.downloaded ? C.green : C.grey500, fontWeight: 500,
                    border: m.downloaded ? "1px solid rgba(22,163,74,0.2)" : "none",
                  }}>
                    {m.downloaded ? "\u2713 Ready" : "Download"}
                  </span>
                </div>
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}

// ─── Step 8 — Extensions ─────────────────────────────────────────────────────
function StepExtensions({ draft, patch }: StepDraftProps) {
  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
      <Lead>Enable built-in extensions. Connect more from Settings &rarr; Extensions later.</Lead>
      <ToggleRow
        icon="\ud83c\udf24\ufe0f"
        title="Weather"
        desc={draft.enableWeather
          ? `Open-Meteo live forecast${draft.locationName ? ` for ${draft.locationName}` : ""}. Auto-detected from your network.`
          : "Enable on the Location step to use this. Location detected from your network."
        }
        checked={draft.enableWeather}
        onChange={(v) => patch({ enableWeather: v })}
      />
      <ToggleRow
        icon="\ud83e\udde0"
        title="Memory"
        desc="Goose remembers facts across conversations using a local flat-file store. Nothing leaves your device."
        checked={draft.enableMcpMemory}
        onChange={(v) => patch({ enableMcpMemory: v })}
      />
      <ToggleRow
        icon="\ud83c\udfe0"
        title="Home Assistant"
        desc="Control lights, sensors, and scenes via your local Home Assistant instance."
        checked={draft.enableHomeAssistant}
        onChange={(v) => patch({ enableHomeAssistant: v })}
        badge="Beta"
      />
      <ToggleRow
        icon="\ud83d\udcc5"
        title="Calendar"
        desc="Read today's events from your local calendar app."
        checked={draft.enableCalendar}
        onChange={(v) => patch({ enableCalendar: v })}
        badge="Beta"
      />
      <p style={{ margin: "8px 0 0", fontSize: 12.5, color: C.grey500, fontStyle: "italic" }}>
        Custom MCP servers and more can be added in Settings &rarr; Extensions.
      </p>
    </div>
  );
}

// ─── Step 9 — Done ────────────────────────────────────────────────────────────
interface StepDoneProps {
  draft: OnboardingDraft;
  onFinish: () => void;
}

function StepDone({ draft, onFinish }: StepDoneProps) {
  const rows = useMemo(() => [
    { k: "You",       v: `${draft.avatar} ${draft.preferredName || draft.userName || "friend"}` },
    { k: "Locale",    v: `${draft.language.toUpperCase()} \u00b7 ${draft.timezone}` },
    { k: "Style",     v: PROMPT_STYLES.find((p) => p.value === draft.promptStyle)?.label ?? "Balanced" },
    { k: "Assistant", v: `${draft.assistantName} \u00b7 ${TTS_VOICES.find((v) => v.value === draft.ttsVoice)?.label ?? "Amy"}` },
    { k: "Wake word", v: draft.wakeWord === "custom" ? `"${draft.wakeWordCustom}"` : `"${draft.wakeWord}"` },
    { k: "Model",     v: `${draft.llmProvider} / ${draft.llmModel}` },
  ], [draft]);

  return (
    <div style={{ display: "flex", flexDirection: "column", alignItems: "center", textAlign: "center", padding: "20px 16px" }}>
      {/* Animated checkmark */}
      <div style={{
        width: 72, height: 72, borderRadius: "50%",
        background: C.purpleMid,
        display: "flex", alignItems: "center", justifyContent: "center",
        color: C.white, marginBottom: 20,
        boxShadow: "0 8px 28px rgba(124,58,237,0.35)",
        animation: "ob-pop 0.4s cubic-bezier(0.34, 1.56, 0.64, 1) both",
      }}>
        <svg width="34" height="34" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
          <path d="M5 12l5 5 9-11" />
        </svg>
      </div>

      <h1 style={{ margin: "0 0 10px", fontFamily: FONT_HEAD, fontWeight: 700, fontSize: 30, letterSpacing: "-0.02em", color: C.grey900 }}>
        You're all set!
      </h1>
      <p style={{ margin: "0 0 26px", fontSize: 14, color: C.grey600, lineHeight: 1.6, maxWidth: "42ch" }}>
        Goose is ready on your network. Try saying{" "}
        <code style={{ fontFamily: FONT_MONO, fontSize: 13, color: C.purple, background: C.purpleBg, padding: "1px 7px", borderRadius: 5 }}>
          "{draft.wakeWord === "custom" ? draft.wakeWordCustom || "your phrase" : draft.wakeWord}"
        </code>{" "}
        or click below.
      </p>

      {/* Summary card */}
      <div style={{
        width: "100%", maxWidth: 420,
        background: C.white, border: `1px solid ${C.grey200}`, borderRadius: 12,
        padding: "16px 20px", marginBottom: 28, textAlign: "left",
      }}>
        <div style={{ fontSize: 11, fontWeight: 700, color: C.grey500, textTransform: "uppercase", letterSpacing: "0.06em", marginBottom: 14 }}>Your setup</div>
        <dl style={{ display: "grid", gridTemplateColumns: "auto 1fr", gap: "10px 20px", margin: 0 }}>
          {rows.map((r) => (
            <Fragment key={r.k}>
              <dt style={{ fontSize: 12.5, color: C.grey500, fontWeight: 500, whiteSpace: "nowrap" }}>{r.k}</dt>
              <dd style={{ fontSize: 13, color: C.grey900, fontWeight: 500, margin: 0 }}>{r.v}</dd>
            </Fragment>
          ))}
        </dl>
      </div>

      <div style={{ display: "flex", gap: 12 }}>
        <button type="button" onClick={onFinish} style={{
          padding: "11px 28px", borderRadius: 9, border: "none",
          background: C.purpleMid,
          color: C.white, fontSize: 14, fontWeight: 700, cursor: "pointer", fontFamily: FONT_BODY,
        }}>Open dashboard</button>
        <button type="button" onClick={onFinish} style={{
          padding: "11px 24px", borderRadius: 9, border: `1.5px solid ${C.grey200}`,
          background: C.white, color: C.grey700, fontSize: 14, fontWeight: 600, cursor: "pointer", fontFamily: FONT_BODY,
        }}>Start chatting</button>
      </div>

      <p style={{ margin: "18px 0 0", fontSize: 12, color: C.grey400 }}>All settings can be changed from Settings at any time.</p>
    </div>
  );
}

// ─── Orchestrator ─────────────────────────────────────────────────────────────
export function OnboardingWizard({ onComplete }: OnboardingWizardProps) {
  const [stepIndex, setStepIndex] = useState(0);
  const [draft, setDraft] = useState<OnboardingDraft>(DEFAULT_DRAFT);

  function patch(p: Partial<OnboardingDraft>) {
    setDraft((d) => ({ ...d, ...p }));
  }
  function next() { setStepIndex((i) => Math.min(i + 1, STEPS.length - 1)); }
  function back() { setStepIndex((i) => Math.max(i - 1, 0)); }
  function finish() { onComplete(); }

  const isWelcome = stepIndex === 0;
  const isDone = stepIndex === STEPS.length - 1;

  const canProceed = (() => {
    switch (STEPS[stepIndex].id) {
      case "basics":   return !!draft.userName.trim();
      case "identity": return !!draft.assistantName.trim();
      case "wake":     return draft.wakeWord !== "custom" || !!draft.wakeWordCustom.trim();
      case "model":    return !!draft.llmModel;
      default:         return true;
    }
  })();

  let body: React.ReactNode;
  switch (STEPS[stepIndex].id) {
    case "welcome":       body = <StepWelcome onNext={next} />; break;
    case "basics":        body = <StepBasics draft={draft} patch={patch} />; break;
    case "location":      body = <StepLocation draft={draft} patch={patch} />; break;
    case "accessibility": body = <StepAccessibility draft={draft} patch={patch} />; break;
    case "personality":   body = <StepPersonality draft={draft} patch={patch} />; break;
    case "identity":      body = <StepIdentity draft={draft} patch={patch} />; break;
    case "wake":          body = <StepWakeWord draft={draft} patch={patch} />; break;
    case "model":         body = <StepModel draft={draft} patch={patch} />; break;
    case "extensions":    body = <StepExtensions draft={draft} patch={patch} />; break;
    case "done":          body = <StepDone draft={draft} onFinish={finish} />; break;
    default:              body = null;
  }

  return (
    <div style={{ display: "grid", gridTemplateColumns: "260px 1fr", width: "100%", height: "100%", minHeight: 0, background: C.grey50 }}>
      <StepRail stepIndex={stepIndex} onJump={setStepIndex} />

      <main style={{
        overflowY: "auto",
        padding: isWelcome || isDone ? "32px 48px" : "36px 56px 56px",
        display: "flex", flexDirection: "column",
        background: C.grey50,
      }}>
        <div
          style={{ width: "100%", maxWidth: 640, margin: "0 auto", animation: "ob-fade 0.22s ease both" }}
          key={stepIndex}
        >
          <style>{`
            @keyframes ob-fade {
              from { opacity: 0; transform: translateY(6px); }
              to   { opacity: 1; transform: translateY(0); }
            }
            @keyframes ob-pop {
              from { opacity: 0; transform: scale(0.5); }
              to   { opacity: 1; transform: scale(1); }
            }
          `}</style>
          <StepHeader stepIndex={stepIndex} />
          {body}
          {!isWelcome && !isDone && (
            <Actions
              onBack={back}
              onNext={next}
              onSkip={next}
              nextLabel={stepIndex === STEPS.length - 2 ? "Finish" : "Continue"}
              disabled={!canProceed}
            />
          )}
        </div>
      </main>
    </div>
  );
}
