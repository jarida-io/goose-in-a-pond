import { useState, useEffect, useRef } from 'react'
import { api, isPreviewMode, DEV_MOCK_TOKEN } from '../api'
import logo from '../assets/logo.png'
import '../onboarding.css'

// ── Types ────────────────────────────────────────────────────────────────────

// 0=Welcome 1=Basics 2=Location 3=Accessibility 4=Personality
// 5=GooseIdentity 6=WakeWord 7=Model 8=Extensions 9=Face 10=Done
type Step = 0 | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10

interface Draft {
  token:              string
  clientId:           string
  primaryProfileId:   string | null
  // Step 1 – Basics
  userName:           string
  preferredName:      string
  birthday:           string
  avatarEmoji:        string
  // Step 2 – Location
  language:           string
  timezone:           string
  locationName:       string
  latitude:           string
  longitude:          string
  enableWeather:      boolean
  // Step 3 – Accessibility
  atypicalSpeech:     boolean
  slowSpeech:         boolean
  highContrast:       boolean
  reduceMotion:       boolean
  // Step 4 – Personality
  promptStyle:        string
  personality:        string
  // Step 5 – Goose's Identity
  assistantName:      string
  ttsVoice:           string
  // Step 6 – Wake Word
  wakeWord:           string
  wakeWordCustom:     string
  // Step 7 – Model
  llmProvider:        string
  llmModel:           string
  // Step 8 – Extensions
  enableMcpMemory:    boolean
}

const DEFAULT_DRAFT: Draft = {
  token:            '',
  clientId:         '',
  primaryProfileId: null,
  userName:         '',
  preferredName:    '',
  birthday:         '',
  avatarEmoji:      '🦆',
  language:         'en',
  timezone:         Intl.DateTimeFormat().resolvedOptions().timeZone || 'UTC',
  locationName:     '',
  latitude:         '',
  longitude:        '',
  enableWeather:    false,
  atypicalSpeech:   false,
  slowSpeech:       false,
  highContrast:     false,
  reduceMotion:     false,
  promptStyle:      'balanced',
  personality:      'friendly and helpful',
  assistantName:    'Goose',
  ttsVoice:         '',
  wakeWord:         'goose',
  wakeWordCustom:   '',
  llmProvider:      '',
  llmModel:         '',
  enableMcpMemory:  false,
}

// ── Step meta ────────────────────────────────────────────────────────────────

const STEP_META = [
  { label: 'Welcome',     icon: '🦆' },
  { label: 'About You',   icon: '👤' },
  { label: 'Location',    icon: '🌍' },
  { label: 'Accessibility', icon: '♿' },
  { label: 'Personality', icon: '💬' },
  { label: 'Goose',       icon: '🎙️' },
  { label: 'Wake Word',   icon: '🔊' },
  { label: 'AI Model',    icon: '🧠' },
  { label: 'Extensions',  icon: '🔌' },
  { label: 'Face',        icon: '🙂' },
]

// ── Progress bar ─────────────────────────────────────────────────────────────

function ProgressBar({ step }: { step: Step }) {
  if (step === 0 || step === 10) return null
  const total = 9
  return (
    <div className="ob-progress" aria-label={`Step ${step} of ${total}`}>
      {Array.from({ length: total }, (_, i) => (
        <div
          key={i}
          className={`ob-progress-seg ${i + 1 < step ? 'done' : i + 1 === step ? 'active' : ''}`}
        />
      ))}
    </div>
  )
}

// ── Step header ──────────────────────────────────────────────────────────────

function StepHeader({ step }: { step: Step }) {
  if (step === 0 || step === 10) return null
  const meta = STEP_META[step]
  return (
    <div className="ob-step-header">
      <span className="ob-step-icon">{meta.icon}</span>
      <span className="ob-step-label">Step {step} of 9 · {meta.label}</span>
    </div>
  )
}

// ── Toggle ───────────────────────────────────────────────────────────────────

function Toggle({ id, checked, onChange }: { id: string; checked: boolean; onChange: (v: boolean) => void }) {
  return (
    <label className="ob-toggle" htmlFor={id}>
      <input
        id={id}
        type="checkbox"
        role="switch"
        checked={checked}
        onChange={e => onChange(e.target.checked)}
      />
      <span className="ob-toggle-track">
        <span className="ob-toggle-thumb" />
      </span>
    </label>
  )
}

// ── Step 0 — Welcome ─────────────────────────────────────────────────────────

function StepWelcome({ onNext, loading, error }: {
  onNext: () => void
  loading: boolean
  error: string | null
}) {
  return (
    <div className="ob-body ob-welcome">
      <div className="ob-welcome-hero">
        <img src={logo} alt="Goose In A Pond" className="ob-logo-large" />
        <h1 className="ob-welcome-title">Goose In A Pond</h1>
        <p className="ob-welcome-tagline">Your private AI assistant — on your home network.</p>
      </div>

      <p className="ob-welcome-desc">
        GIAP is a voice-first smart home assistant that runs entirely on your own
        hardware. It thinks locally, speaks locally, and never sends your data
        anywhere. Once set up, just say the wake word and Goose is ready.
      </p>

      <div className="ob-feature-pills">
        <span className="ob-pill">🔒 Fully Offline</span>
        <span className="ob-pill">🎙️ Voice-First</span>
        <span className="ob-pill">🏠 Privacy by Design</span>
      </div>

      <p className="ob-hint ob-muted">Takes about 3 minutes to set up.</p>

      {error && <p className="ob-error">{error}</p>}

      <div className="ob-actions ob-actions-center">
        <button className="ob-btn ob-btn-primary ob-btn-lg" onClick={onNext} disabled={loading}>
          {loading ? 'Connecting…' : 'Get Started'}
        </button>
      </div>
    </div>
  )
}

// ── Step 1 — Basics ──────────────────────────────────────────────────────────

const AVATARS = ['🦆', '🐧', '🦅', '🦜', '🐸']

function StepBasics({ draft, onChange, onNext, onBack, loading, error }: {
  draft: Draft
  onChange: (p: Partial<Draft>) => void
  onNext: () => void
  onBack: () => void
  loading: boolean
  error: string | null
}) {
  const [showBirthday, setShowBirthday] = useState(!!draft.birthday)

  return (
    <div className="ob-body ob-form">
      <h2>Tell us about you</h2>
      <p className="ob-hint">Goose uses this to greet you by name and personalise responses.</p>

      <label htmlFor="userName">Your name <span className="ob-required">*</span></label>
      <input
        id="userName"
        type="text"
        value={draft.userName}
        onChange={e => onChange({ userName: e.target.value })}
        placeholder="e.g. Jack Smith"
        maxLength={50}
        autoFocus
      />

      <label htmlFor="preferredName">
        Preferred name <span className="ob-optional">(optional)</span>
      </label>
      <p className="ob-hint">What should Goose call you in conversation?</p>
      <input
        id="preferredName"
        type="text"
        value={draft.preferredName}
        onChange={e => onChange({ preferredName: e.target.value })}
        placeholder="e.g. Jack, Captain, Mum…"
        maxLength={50}
      />

      <div className="ob-label-row">
        <label>Avatar</label>
      </div>
      <div className="ob-emoji-picker">
        {AVATARS.map(e => (
          <button
            key={e}
            type="button"
            className={`ob-emoji-btn ${draft.avatarEmoji === e ? 'selected' : ''}`}
            onClick={() => onChange({ avatarEmoji: e })}
            aria-label={e}
          >
            {e}
          </button>
        ))}
      </div>

      {!showBirthday ? (
        <button type="button" className="ob-link" onClick={() => setShowBirthday(true)}>
          + Add birthday (optional)
        </button>
      ) : (
        <>
          <label htmlFor="birthday">Birthday <span className="ob-optional">(optional)</span></label>
          <p className="ob-hint">Goose will wish you well on the day.</p>
          <input
            id="birthday"
            type="date"
            value={draft.birthday}
            onChange={e => onChange({ birthday: e.target.value })}
          />
        </>
      )}

      {error && <p className="ob-error">{error}</p>}
      <div className="ob-actions">
        <button className="ob-btn ob-btn-ghost" onClick={onBack}>Back</button>
        <button
          className="ob-btn ob-btn-primary"
          onClick={onNext}
          disabled={loading || !draft.userName.trim()}
        >
          {loading ? 'Saving…' : 'Next'}
        </button>
      </div>
    </div>
  )
}

// ── Step 2 — Location ────────────────────────────────────────────────────────

const LANGUAGES = [
  { value: 'en',    label: 'English' },
  { value: 'fr',    label: 'French' },
  { value: 'es',    label: 'Spanish' },
  { value: 'de',    label: 'German' },
  { value: 'sw',    label: 'Swahili' },
  { value: 'other', label: 'Other' },
]

async function detectLocationFromIP(): Promise<{ city: string; latitude: number; longitude: number; timezone: string } | null> {
  try {
    const res = await fetch('https://ipapi.co/json/')
    if (!res.ok) return null
    const data = await res.json()
    if (!data.latitude || !data.longitude) return null
    return {
      city:      data.city || data.region || '',
      latitude:  data.latitude,
      longitude: data.longitude,
      timezone:  data.timezone || '',
    }
  } catch {
    return null
  }
}

function StepLocation({ draft, onChange, onNext, onBack, loading, error }: {
  draft: Draft
  onChange: (p: Partial<Draft>) => void
  onNext: () => void
  onBack: () => void
  loading: boolean
  error: string | null
}) {
  const [detecting, setDetecting] = useState(false)
  const [detected, setDetected]   = useState(false)

  async function handleWeatherToggle(enabled: boolean) {
    onChange({ enableWeather: enabled })
    if (!enabled || draft.latitude) return   // already have coords

    setDetecting(true)
    const loc = await detectLocationFromIP()
    setDetecting(false)

    if (loc) {
      setDetected(true)
      onChange({
        enableWeather: true,
        latitude:     String(loc.latitude),
        longitude:    String(loc.longitude),
        locationName: draft.locationName || loc.city,
        timezone:     draft.timezone !== 'UTC' && draft.timezone
          ? draft.timezone
          : loc.timezone || draft.timezone,
      })
    }
  }

  return (
    <div className="ob-body ob-form">
      <h2>Language &amp; Location</h2>
      <p className="ob-hint">Used for voice responses, weather, and time-aware answers.</p>

      <label htmlFor="language">Language</label>
      <select
        id="language"
        value={draft.language}
        onChange={e => onChange({ language: e.target.value })}
      >
        {LANGUAGES.map(l => (
          <option key={l.value} value={l.value}>{l.label}</option>
        ))}
      </select>

      <label htmlFor="timezone">Timezone</label>
      <input
        id="timezone"
        type="text"
        value={draft.timezone}
        onChange={e => onChange({ timezone: e.target.value })}
        placeholder="e.g. Africa/Nairobi"
      />

      <label htmlFor="locationName">City / Location name <span className="ob-optional">(optional)</span></label>
      <input
        id="locationName"
        type="text"
        value={draft.locationName}
        onChange={e => onChange({ locationName: e.target.value })}
        placeholder="e.g. Nairobi, London, New York"
        maxLength={100}
      />

      <div className="ob-toggle-row">
        <div className="ob-toggle-text">
          <strong>Enable live weather</strong>
          <p className="ob-hint">
            {detecting
              ? '📍 Detecting your location…'
              : detected && draft.latitude
              ? `📍 Location detected${draft.locationName ? ` · ${draft.locationName}` : ''}`
              : 'Uses your approximate network location — no precise GPS needed.'}
          </p>
        </div>
        <Toggle id="weather" checked={draft.enableWeather} onChange={handleWeatherToggle} />
      </div>

      {error && <p className="ob-error">{error}</p>}
      <div className="ob-actions">
        <button className="ob-btn ob-btn-ghost" onClick={onBack}>Back</button>
        <button className="ob-btn ob-btn-primary" onClick={onNext} disabled={loading || detecting}>
          {loading ? 'Saving…' : detecting ? 'Detecting…' : 'Next'}
        </button>
      </div>
    </div>
  )
}

// ── Step 3 — Accessibility ───────────────────────────────────────────────────

function StepAccessibility({ draft, onChange, onNext, onBack, loading, error }: {
  draft: Draft
  onChange: (p: Partial<Draft>) => void
  onNext: () => void
  onBack: () => void
  loading: boolean
  error: string | null
}) {
  const items = [
    {
      id:    'atypicalSpeech',
      key:   'atypicalSpeech' as const,
      title: 'Atypical speech support',
      desc:  'Goose waits a little longer before responding — helpful if you have a stutter, pause frequently, or use non-standard speech patterns.',
    },
    {
      id:    'slowSpeech',
      key:   'slowSpeech' as const,
      title: 'Slow speech',
      desc:  'Goose speaks at a reduced pace so responses are easier to follow.',
    },
    {
      id:    'highContrast',
      key:   'highContrast' as const,
      title: 'High contrast',
      desc:  'Increases visual contrast across the dashboard.',
    },
    {
      id:    'reduceMotion',
      key:   'reduceMotion' as const,
      title: 'Reduce motion',
      desc:  'Disables animations and transitions throughout the interface.',
    },
  ]

  return (
    <div className="ob-body ob-form">
      <h2>Accessibility</h2>
      <p className="ob-hint">All of these can be changed at any time in Settings.</p>

      <div className="ob-toggle-list">
        {items.map(item => (
          <div key={item.id} className="ob-toggle-row">
            <div className="ob-toggle-text">
              <strong>{item.title}</strong>
              <p className="ob-hint">{item.desc}</p>
            </div>
            <Toggle
              id={item.id}
              checked={draft[item.key]}
              onChange={v => onChange({ [item.key]: v })}
            />
          </div>
        ))}
      </div>

      {error && <p className="ob-error">{error}</p>}
      <div className="ob-actions">
        <button className="ob-btn ob-btn-ghost" onClick={onBack}>Back</button>
        <button className="ob-btn ob-btn-primary" onClick={onNext} disabled={loading}>
          {loading ? 'Saving…' : 'Next'}
        </button>
      </div>
    </div>
  )
}

// ── Step 4 — Personality ─────────────────────────────────────────────────────

const PROMPT_STYLES = [
  { value: 'balanced',  label: 'Balanced',  desc: 'Warm and practical. Clear answers with just enough detail.' },
  { value: 'concise',   label: 'Concise',   desc: 'Short, action-first replies. Skips the small talk.' },
  { value: 'technical', label: 'Technical', desc: 'Detailed step-by-step explanations with narration. For tinkerers.' },
  { value: 'warm',      label: 'Warm',      desc: 'Conversational and friendly, like a helpful neighbour.' },
]

function StepPersonality({ draft, onChange, onNext, onBack, loading, error }: {
  draft: Draft
  onChange: (p: Partial<Draft>) => void
  onNext: () => void
  onBack: () => void
  loading: boolean
  error: string | null
}) {
  return (
    <div className="ob-body ob-form">
      <h2>Response style</h2>
      <p className="ob-hint">Choose how Goose talks to you. You can always change this in Settings.</p>

      <fieldset className="ob-fieldset">
        <legend>Conversation style</legend>
        <div className="ob-radio-group">
          {PROMPT_STYLES.map(s => (
            <label
              key={s.value}
              className={`ob-radio-card ${draft.promptStyle === s.value ? 'selected' : ''}`}
            >
              <input
                type="radio"
                name="promptStyle"
                value={s.value}
                checked={draft.promptStyle === s.value}
                onChange={() => onChange({ promptStyle: s.value })}
              />
              <div>
                <strong>{s.label}</strong>
                <span>{s.desc}</span>
              </div>
            </label>
          ))}
        </div>
      </fieldset>

      <label htmlFor="personality">Personality hint <span className="ob-optional">(optional)</span></label>
      <p className="ob-hint">A few words to shape Goose's character. e.g. "curious and warm", "direct and no-nonsense".</p>
      <textarea
        id="personality"
        value={draft.personality}
        onChange={e => onChange({ personality: e.target.value })}
        placeholder="friendly and helpful"
        rows={2}
        maxLength={200}
      />

      {error && <p className="ob-error">{error}</p>}
      <div className="ob-actions">
        <button className="ob-btn ob-btn-ghost" onClick={onBack}>Back</button>
        <button className="ob-btn ob-btn-primary" onClick={onNext} disabled={loading}>
          {loading ? 'Saving…' : 'Next'}
        </button>
      </div>
    </div>
  )
}

// ── Step 5 — Goose's Identity ─────────────────────────────────────────────────

interface TtsVoiceOption { value: string; label: string; desc: string }

function StepGooseIdentity({ draft, onChange, onNext, onBack, loading, error }: {
  draft: Draft
  onChange: (p: Partial<Draft>) => void
  onNext: () => void
  onBack: () => void
  loading: boolean
  error: string | null
}) {
  const [ttsVoices, setTtsVoices] = useState<TtsVoiceOption[]>([])

  useEffect(() => {
    api.listModels(draft.token)
      .then(resp => {
        const voices: TtsVoiceOption[] = resp.tts
          .filter(m => m.filename && m.filename.endsWith('.onnx'))
          .map(m => ({
            value: m.filename!,
            label: m.name,
            desc:  m.description || m.filename!,
          }))
        setTtsVoices([
          ...voices,
          { value: 'custom', label: 'Custom', desc: 'Enter an ONNX model filename in Settings.' },
        ])
      })
      .catch(() => {
        setTtsVoices([
          { value: 'custom', label: 'Custom', desc: 'Enter an ONNX model filename in Settings.' },
        ])
      })
  }, [draft.token])

  return (
    <div className="ob-body ob-form">
      <h2>Goose's identity</h2>
      <p className="ob-hint">
        Give your assistant a name and choose a voice. You can add more
        household members from Settings later.
      </p>

      <label htmlFor="assistantName">Assistant name</label>
      <input
        id="assistantName"
        type="text"
        value={draft.assistantName}
        onChange={e => onChange({ assistantName: e.target.value })}
        placeholder="Goose"
        maxLength={50}
      />

      <fieldset className="ob-fieldset">
        <legend>Voice</legend>
        {ttsVoices.length === 0 ? (
          <p className="ob-hint">Loading available voices…</p>
        ) : (
          <div className="ob-radio-group">
            {ttsVoices.map(v => (
              <label
                key={v.value}
                className={`ob-radio-card ${draft.ttsVoice === v.value ? 'selected' : ''}`}
              >
                <input
                  type="radio"
                  name="ttsVoice"
                  value={v.value}
                  checked={draft.ttsVoice === v.value}
                  onChange={() => onChange({ ttsVoice: v.value })}
                />
                <div>
                  <strong>{v.label}</strong>
                  <span>{v.desc}</span>
                </div>
              </label>
            ))}
          </div>
        )}
      </fieldset>

      {error && <p className="ob-error">{error}</p>}
      <div className="ob-actions">
        <button className="ob-btn ob-btn-ghost" onClick={onBack}>Back</button>
        <button
          className="ob-btn ob-btn-primary"
          onClick={onNext}
          disabled={loading || !draft.assistantName.trim()}
        >
          {loading ? 'Saving…' : 'Next'}
        </button>
      </div>
    </div>
  )
}

// ── Step 6 — Wake Word ───────────────────────────────────────────────────────

const WAKE_PRESETS = [
  { value: 'goose',       label: '"Goose"',       desc: 'Short and memorable.' },
  { value: 'hey goose',   label: '"Hey Goose"',   desc: 'Natural call-and-response.' },
  { value: 'ok computer', label: '"OK Computer"', desc: 'Classic command style.' },
  { value: 'custom',      label: 'Custom phrase', desc: 'Say anything you like.' },
]

function StepWakeWord({ draft, onChange, onNext, onBack, loading, error }: {
  draft: Draft
  onChange: (p: Partial<Draft>) => void
  onNext: () => void
  onBack: () => void
  loading: boolean
  error: string | null
}) {
  const isCustom = draft.wakeWord === 'custom'
  const resolvedWord = isCustom ? draft.wakeWordCustom.trim() : draft.wakeWord

  return (
    <div className="ob-body ob-form">
      <h2>Wake word</h2>
      <p className="ob-hint">
        Say this phrase to activate Goose when it's listening in voice mode.
      </p>

      <div className="ob-radio-group ob-radio-group-2col">
        {WAKE_PRESETS.map(p => (
          <label
            key={p.value}
            className={`ob-radio-card ${draft.wakeWord === p.value ? 'selected' : ''}`}
          >
            <input
              type="radio"
              name="wakeWord"
              value={p.value}
              checked={draft.wakeWord === p.value}
              onChange={() => onChange({ wakeWord: p.value })}
            />
            <div>
              <strong>{p.label}</strong>
              <span>{p.desc}</span>
            </div>
          </label>
        ))}
      </div>

      {isCustom && (
        <>
          <label htmlFor="wakeWordCustom">Your custom phrase</label>
          <input
            id="wakeWordCustom"
            type="text"
            value={draft.wakeWordCustom}
            onChange={e => onChange({ wakeWordCustom: e.target.value })}
            placeholder="e.g. hey duck, morning pond…"
            maxLength={60}
            autoFocus
          />
        </>
      )}

      {error && <p className="ob-error">{error}</p>}
      <div className="ob-actions">
        <button className="ob-btn ob-btn-ghost" onClick={onBack}>Back</button>
        <button
          className="ob-btn ob-btn-primary"
          onClick={onNext}
          disabled={loading || (isCustom && !draft.wakeWordCustom.trim()) || !resolvedWord}
        >
          {loading ? 'Saving…' : 'Next'}
        </button>
      </div>
    </div>
  )
}

// ── Step 7 — AI Model ────────────────────────────────────────────────────────

const LLM_PROVIDERS = [
  { value: 'llamafile', label: 'Llamafile',        desc: 'Self-contained AI model that starts automatically. Best for most users.' },
  { value: 'ollama',    label: 'Ollama',            desc: 'Use a model you\'ve already set up with Ollama.' },
  { value: 'local',     label: 'GGUF file',         desc: 'Load a GGUF model directly. Best for custom or large models.' },
]

const MODEL_HINTS: Record<string, string> = {
  llamafile: 'Name from the Models page, e.g. gemma-2b',
  ollama:    'Run `ollama list` to see installed models, e.g. llama3.2',
  local:     'GGUF model name from the Models page',
}

function StepModel({ draft, onChange, onNext, onBack, loading, error }: {
  draft: Draft
  onChange: (p: Partial<Draft>) => void
  onNext: () => void
  onBack: () => void
  loading: boolean
  error: string | null
}) {
  return (
    <div className="ob-body ob-form">
      <h2>Choose your AI model</h2>
      <p className="ob-hint">
        This is the brain behind Goose's responses. Not sure? Start with{' '}
        <strong>Llamafile</strong> — it handles everything automatically.
      </p>

      <fieldset className="ob-fieldset">
        <legend>AI provider</legend>
        <div className="ob-radio-group">
          {LLM_PROVIDERS.map(p => (
            <label
              key={p.value}
              className={`ob-radio-card ${draft.llmProvider === p.value ? 'selected' : ''}`}
            >
              <input
                type="radio"
                name="llmProvider"
                value={p.value}
                checked={draft.llmProvider === p.value}
                onChange={() => onChange({ llmProvider: p.value })}
              />
              <div>
                <strong>{p.label}</strong>
                <span>{p.desc}</span>
              </div>
            </label>
          ))}
        </div>
      </fieldset>

      <label htmlFor="llmModel">Model name</label>
      <p className="ob-hint">{MODEL_HINTS[draft.llmProvider]}</p>
      <input
        id="llmModel"
        type="text"
        value={draft.llmModel}
        onChange={e => onChange({ llmModel: e.target.value })}
        placeholder={draft.llmProvider === 'ollama' ? 'llama3.2' : 'gemma-2b'}
        maxLength={100}
      />

      {error && <p className="ob-error">{error}</p>}
      <div className="ob-actions">
        <button className="ob-btn ob-btn-ghost" onClick={onBack}>Back</button>
        <button
          className="ob-btn ob-btn-primary"
          onClick={onNext}
          disabled={loading || !draft.llmModel.trim()}
        >
          {loading ? 'Saving…' : 'Next'}
        </button>
      </div>
    </div>
  )
}

// ── Step 8 — Extensions ──────────────────────────────────────────────────────

function StepExtensions({ draft, onChange, onNext, onBack, loading, error }: {
  draft: Draft
  onChange: (p: Partial<Draft>) => void
  onNext: () => void
  onBack: () => void
  loading: boolean
  error: string | null
}) {
  return (
    <div className="ob-body ob-form">
      <h2>Extensions</h2>
      <p className="ob-hint">
        Enable built-in extensions for Goose. You can connect more from the
        Settings page after setup.
      </p>

      <div className="ob-extension-list">
        <div className="ob-extension-card">
          <div className="ob-extension-info">
            <span className="ob-extension-icon">🌤️</span>
            <div>
              <strong>Weather</strong>
              <p className="ob-hint">
                {draft.latitude
                  ? `Live weather from Open-Meteo${draft.locationName ? ` for ${draft.locationName}` : ''} · location auto-detected.`
                  : 'Enable weather on the Location step to use this. Location is auto-detected from your network.'}
              </p>
            </div>
          </div>
          <Toggle
            id="ext-weather"
            checked={draft.enableWeather}
            onChange={v => onChange({ enableWeather: v })}
          />
        </div>

        <div className="ob-extension-card">
          <div className="ob-extension-info">
            <span className="ob-extension-icon">🧠</span>
            <div>
              <strong>Memory</strong>
              <p className="ob-hint">
                Goose remembers facts you share across conversations using a
                local flat-file store. Nothing leaves your device.
              </p>
            </div>
          </div>
          <Toggle
            id="ext-memory"
            checked={draft.enableMcpMemory}
            onChange={v => onChange({ enableMcpMemory: v })}
          />
        </div>
      </div>

      <p className="ob-hint ob-muted" style={{ marginTop: '1rem' }}>
        More extensions — home automation, calendars, custom MCP servers — can
        be added in Settings → Extensions.
      </p>

      {error && <p className="ob-error">{error}</p>}
      <div className="ob-actions">
        <button className="ob-btn ob-btn-ghost" onClick={onBack}>Back</button>
        <button className="ob-btn ob-btn-primary" onClick={onNext} disabled={loading}>
          {loading ? 'Saving…' : 'Next'}
        </button>
      </div>
    </div>
  )
}

// ── Step 9 — Face Enrollment (optional) ──────────────────────────────────────
//
// Captures three webcam frames and POSTs each to /api/v1/faces/register
// against the primary profile created in step 1. Three samples is the
// minimum required by the matcher's `MIN_SAMPLES_TO_IDENTIFY` floor, so
// completing this step gives the user an immediately-usable face profile.
// Skipping is always allowed — face recognition can be added later from
// the Faces page without re-running onboarding.

function StepFace({ draft, onNext, onBack }: {
  draft: Draft
  onNext: () => void
  onBack: () => void
}) {
  const videoRef    = useRef<HTMLVideoElement | null>(null)
  const canvasRef   = useRef<HTMLCanvasElement | null>(null)
  const streamRef   = useRef<MediaStream | null>(null)
  const [cameraError, setCameraError] = useState<string | null>(null)
  const [count, setCount]             = useState(0)   // samples captured
  const [busy, setBusy]               = useState(false)
  const [featureMissing, setFeatureMissing] = useState(false)
  const [error, setError]             = useState<string | null>(null)

  useEffect(() => {
    let cancelled = false
    async function startCam() {
      if (!navigator.mediaDevices?.getUserMedia) {
        setCameraError('This browser does not support webcam capture.')
        return
      }
      try {
        const stream = await navigator.mediaDevices.getUserMedia({
          video: { width: { ideal: 640 }, height: { ideal: 480 }, facingMode: 'user' },
          audio: false,
        })
        if (cancelled) { stream.getTracks().forEach(t => t.stop()); return }
        streamRef.current = stream
        if (videoRef.current) {
          videoRef.current.srcObject = stream
          await videoRef.current.play().catch(() => {/* autoplay */})
        }
      } catch (e) {
        setCameraError(e instanceof Error ? e.message : 'Camera permission denied.')
      }
    }
    startCam()
    return () => {
      cancelled = true
      streamRef.current?.getTracks().forEach(t => t.stop())
      streamRef.current = null
    }
  }, [])

  async function captureBlob(): Promise<Blob | null> {
    const v = videoRef.current
    const c = canvasRef.current
    if (!v || !c || v.videoWidth === 0) return null
    c.width = v.videoWidth; c.height = v.videoHeight
    const ctx = c.getContext('2d')
    if (!ctx) return null
    ctx.drawImage(v, 0, 0, c.width, c.height)
    return new Promise(r => c.toBlob(b => r(b), 'image/jpeg', 0.92))
  }

  async function handleCaptureSample() {
    setError(null)
    if (!draft.primaryProfileId || isPreviewMode(draft.token)) {
      // No profile (preview mode) — just count locally.
      setCount(c => c + 1); return
    }
    setBusy(true)
    try {
      const blob = await captureBlob()
      if (!blob) { setError('Could not capture a frame yet — give the camera a moment.'); return }
      await api.registerFace(draft.primaryProfileId, blob, draft.token)
      setCount(c => c + 1)
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e)
      if (msg.includes('503') || msg.toLowerCase().includes('not configured')) {
        setFeatureMissing(true)
      } else {
        setError(msg)
      }
    } finally {
      setBusy(false)
    }
  }

  const enough = count >= 3

  return (
    <div className="ob-body ob-form">
      <h2>Recognise you on sight (optional)</h2>
      <p className="ob-hint">
        Capture three quick frames so Goose can tell who is talking when multiple
        people share the assistant. Only a small embedding vector is stored —
        never the photo itself, and the vector cannot be reversed back into an image.
        You can skip this and enrol later from the Faces page.
      </p>

      {featureMissing && (
        <p className="ob-error">
          This server was built without face recognition support. Rebuild
          pond-server with <code>--features face-onnx</code> to enable.
        </p>
      )}

      <div style={{ display: 'flex', gap: '1rem', alignItems: 'flex-start', marginTop: '0.75rem', flexWrap: 'wrap' }}>
        <div style={{ flex: '1 1 320px', position: 'relative', minWidth: 280 }}>
          {cameraError ? (
            <p className="ob-error">{cameraError}</p>
          ) : (
            <video
              ref={videoRef}
              muted
              playsInline
              style={{ width: '100%', borderRadius: 12, background: '#000', aspectRatio: '4/3' }}
            />
          )}
          <canvas ref={canvasRef} style={{ display: 'none' }} />
        </div>

        <div style={{ flex: '1 1 220px', minWidth: 200 }}>
          <p style={{ fontWeight: 600, marginBottom: '0.5rem' }}>
            Captured: {count} / 3
          </p>
          <ul className="ob-hint" style={{ margin: '0 0 0.75rem 1rem', padding: 0 }}>
            <li>Frame 1 — face the camera straight on</li>
            <li>Frame 2 — turn ~15° left</li>
            <li>Frame 3 — turn ~15° right</li>
          </ul>
          <button
            type="button"
            className="ob-btn ob-btn-primary"
            onClick={handleCaptureSample}
            disabled={busy || !!cameraError || featureMissing || count >= 3}
          >
            {busy ? 'Capturing…' : count >= 3 ? 'Done' : 'Capture frame'}
          </button>
        </div>
      </div>

      {error && <p className="ob-error" style={{ marginTop: '0.75rem' }}>{error}</p>}

      <div className="ob-actions" style={{ marginTop: '1rem' }}>
        <button className="ob-btn ob-btn-ghost" onClick={onBack}>Back</button>
        <button className="ob-btn ob-btn-ghost" onClick={onNext}>
          Skip for now
        </button>
        <button className="ob-btn ob-btn-primary" onClick={onNext} disabled={!enough && !featureMissing}>
          {enough ? 'Continue' : `Capture ${3 - count} more`}
        </button>
      </div>
    </div>
  )
}

// ── Orchestrator ─────────────────────────────────────────────────────────────

interface Props {
  onComplete: (token: string, displayName: string) => void
}

export default function Onboarding({ onComplete }: Props) {
  const [step, setStep]       = useState<Step>(0)
  const [draft, setDraft]     = useState<Draft>(DEFAULT_DRAFT)
  const [loading, setLoading] = useState(false)
  const [error, setError]     = useState<string | null>(null)

  function patch(p: Partial<Draft>) {
    setDraft(d => ({ ...d, ...p }))
    setError(null)
  }

  function back(to: Step) {
    setError(null)
    setStep(to)
  }

  async function wrap(fn: () => Promise<void>) {
    setError(null)
    setLoading(true)
    try {
      await fn()
    } catch (e) {
      setError(e instanceof Error ? e.message : 'Something went wrong. Please try again.')
    } finally {
      setLoading(false)
    }
  }

  // ── Step 0 → 1: handshake ────────────────────────────────────────────────
  async function handleWelcome() {
    await wrap(async () => {
      let clientId = localStorage.getItem('pond_client_id') ?? ''
      if (!clientId) {
        clientId = typeof crypto.randomUUID === 'function'
          ? crypto.randomUUID()
          : `${Date.now().toString(36)}-${Math.random().toString(36).slice(2)}-${Math.random().toString(36).slice(2)}`
        localStorage.setItem('pond_client_id', clientId)
      }

      if (window.location.search.includes('preview')) {
        patch({ token: DEV_MOCK_TOKEN, clientId })
        setStep(1)
        return
      }

      const res = await api.handshake({ client_id: clientId, client_type: 'web', client_version: '1.0.0' })
      if (!res.accepted || !res.session_token) {
        throw new Error(res.rejection_reason ?? 'Server rejected the connection.')
      }

      localStorage.setItem('pond_session_token', res.session_token)
      patch({ token: res.session_token, clientId })
      try { await api.startOnboarding() } catch { /* fine — may already be started */ }
      setStep(1)
    })
  }

  // ── Step 1 → 2: create profile + save user_name ──────────────────────────
  async function handleBasics() {
    await wrap(async () => {
      if (!isPreviewMode(draft.token)) {
        const profile = await api.createProfile(
          { display_name: draft.userName.trim(), avatar_emoji: draft.avatarEmoji },
          draft.token,
        )
        patch({ primaryProfileId: profile.id })
        await api.saveSettings(
          { user_name: draft.userName.trim(), primary_profile_id: profile.id },
          draft.token,
        )
      }
      setStep(2)
    })
  }

  // ── Step 2 → 3: save location to profile + global settings ──────────────
  async function handleLocation() {
    await wrap(async () => {
      if (!isPreviewMode(draft.token) && draft.primaryProfileId) {
        await api.updateProfilePreferences(draft.primaryProfileId, {
          language: draft.language,
        }, draft.token)
        await api.saveSettings({
          timezone:               draft.timezone,
          weather_enabled:        draft.enableWeather,
          weather_location_name:  draft.locationName.trim(),
          weather_latitude:       draft.latitude ? parseFloat(draft.latitude) : 0,
          weather_longitude:      draft.longitude ? parseFloat(draft.longitude) : 0,
        }, draft.token)
      }
      setStep(3)
    })
  }

  // ── Step 3 → 4: save accessibility to profile ───────────────────────────
  async function handleAccessibility() {
    await wrap(async () => {
      if (!isPreviewMode(draft.token) && draft.primaryProfileId) {
        await api.updateProfilePreferences(draft.primaryProfileId, {
          accessibility_atypical_speech: draft.atypicalSpeech ? 'true' : 'false',
          accessibility_slow_speech:     draft.slowSpeech     ? 'true' : 'false',
          accessibility_high_contrast:   draft.highContrast   ? 'true' : 'false',
          accessibility_reduce_motion:   draft.reduceMotion   ? 'true' : 'false',
        }, draft.token)
      }
      setStep(4)
    })
  }

  // ── Step 4 → 5: save personality ────────────────────────────────────────
  async function handlePersonality() {
    await wrap(async () => {
      if (!isPreviewMode(draft.token)) {
        await api.saveSettings({
          prompt_style:          draft.promptStyle,
          assistant_personality: draft.personality.trim(),
        }, draft.token)
      }
      setStep(5)
    })
  }

  // ── Step 5 → 6: save Goose's identity ───────────────────────────────────
  async function handleGooseIdentity() {
    await wrap(async () => {
      if (!isPreviewMode(draft.token)) {
        const ttsVoice = draft.ttsVoice === 'custom' ? draft.ttsVoice : draft.ttsVoice
        await api.saveSettings({
          assistant_name:   draft.assistantName.trim(),
          voice_tts_voice:  ttsVoice,
          active_tts_model: ttsVoice,
        }, draft.token)
      }
      setStep(6)
    })
  }

  // ── Step 6 → 7: save wake word ───────────────────────────────────────────
  async function handleWakeWord() {
    await wrap(async () => {
      const word = draft.wakeWord === 'custom' ? draft.wakeWordCustom.trim() : draft.wakeWord
      if (!isPreviewMode(draft.token)) {
        await api.saveSettings({ voice_wake_word: word }, draft.token)
      }
      setStep(7)
    })
  }

  // ── Step 7 → 8: save model ───────────────────────────────────────────────
  async function handleModel() {
    await wrap(async () => {
      if (!isPreviewMode(draft.token)) {
        await api.saveSettings({
          llm_provider:     draft.llmProvider,
          active_llm_model: draft.llmModel.trim(),
        }, draft.token)
      }
      setStep(8)
    })
  }

  // ── Step 8 → 9: save extensions ─────────────────────────────────────────
  async function handleExtensions() {
    await wrap(async () => {
      if (!isPreviewMode(draft.token)) {
        await api.saveSettings({ weather_enabled: draft.enableWeather }, draft.token)
      }
      setStep(9)
    })
  }

  // ── Step 9 → Done: face enrollment is purely optional ────────────────────
  // The captured frames are POSTed to /faces/register against the primary
  // profile created in step 1. If the backend was built without
  // `--features face-onnx` the call 503s — we surface the error inline and
  // let the user skip past, since face recognition is an enhancement, not a
  // gate to using GIAP.
  function handleFaceDone() {
    setStep(10)
  }

  // ── Done: complete onboarding ────────────────────────────────────────────
  async function handleFinish() {
    await wrap(async () => {
      if (!isPreviewMode(draft.token)) {
        await api.completeOnboarding()
      }
      const displayName = draft.preferredName.trim() || draft.userName.trim() || draft.assistantName
      onComplete(draft.token, displayName)
    })
  }

  // ── Done screen ──────────────────────────────────────────────────────────
  if (step === 10) {
    const name = draft.preferredName.trim() || draft.userName.trim()
    const resolvedWakeWord = draft.wakeWord === 'custom' ? draft.wakeWordCustom : draft.wakeWord
    return (
      <div className="ob-shell">
        <div className="ob-card ob-complete">
          <div className="ob-complete-check">✓</div>
          <h2>You're all set{name ? `, ${name}` : ''}!</h2>
          <p>
            <strong>{draft.assistantName}</strong> is ready and waiting on your
            home network.
          </p>
          <div className="ob-summary-pills">
            <span className="ob-summary-pill">🎙️ {draft.assistantName}</span>
            <span className="ob-summary-pill">🔊 "{resolvedWakeWord}"</span>
            <span className="ob-summary-pill">🧠 {draft.llmModel}</span>
          </div>
          <p className="ob-hint">You can fine-tune everything in Settings at any time.</p>
          {error && <p className="ob-error">{error}</p>}
          <button
            className="ob-btn ob-btn-primary ob-btn-lg"
            disabled={loading}
            onClick={handleFinish}
          >
            {loading ? 'Starting up…' : 'Go to dashboard'}
          </button>
        </div>
      </div>
    )
  }

  // ── Wizard shell ─────────────────────────────────────────────────────────
  return (
    <div className="ob-shell">
      <div className={`ob-card ${step === 0 ? 'ob-card-welcome' : ''}`}>
        <div className="ob-card-top">
          <img src={logo} alt="Goose In A Pond" className="ob-logo" />
          <ProgressBar step={step} />
        </div>

        <StepHeader step={step} />

        <div className="ob-step-body" key={step}>
          {step === 0 && <StepWelcome     onNext={handleWelcome}     loading={loading} error={error} />}
          {step === 1 && <StepBasics      draft={draft} onChange={patch} onNext={handleBasics}       onBack={() => back(0)} loading={loading} error={error} />}
          {step === 2 && <StepLocation    draft={draft} onChange={patch} onNext={handleLocation}     onBack={() => back(1)} loading={loading} error={error} />}
          {step === 3 && <StepAccessibility draft={draft} onChange={patch} onNext={handleAccessibility} onBack={() => back(2)} loading={loading} error={error} />}
          {step === 4 && <StepPersonality draft={draft} onChange={patch} onNext={handlePersonality}  onBack={() => back(3)} loading={loading} error={error} />}
          {step === 5 && <StepGooseIdentity draft={draft} onChange={patch} onNext={handleGooseIdentity} onBack={() => back(4)} loading={loading} error={error} />}
          {step === 6 && <StepWakeWord    draft={draft} onChange={patch} onNext={handleWakeWord}     onBack={() => back(5)} loading={loading} error={error} />}
          {step === 7 && <StepModel       draft={draft} onChange={patch} onNext={handleModel}        onBack={() => back(6)} loading={loading} error={error} />}
          {step === 8 && <StepExtensions  draft={draft} onChange={patch} onNext={handleExtensions}   onBack={() => back(7)} loading={loading} error={error} />}
          {step === 9 && <StepFace        draft={draft} onNext={handleFaceDone}                       onBack={() => back(8)} />}
        </div>
      </div>
    </div>
  )
}
