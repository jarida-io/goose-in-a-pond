import { useState, useEffect } from 'react'
import { api, isPreviewMode } from '../api'
import { useSettings } from '../context/SettingsContext'
import { clearActivity, logActivity } from '../activityLog'

interface Props {
  token: string
}

type Theme = 'system' | 'light' | 'dark'

const THEMES: { value: Theme; label: string; description: string }[] = [
  { value: 'system', label: 'System', description: 'Follows your OS or browser dark/light preference.' },
  { value: 'light',  label: 'Light',  description: 'Always use the light theme.' },
  { value: 'dark',   label: 'Dark',   description: 'Always use the dark theme.' },
]

const PROMPT_STYLES = [
  { value: 'balanced',  label: 'Balanced',  description: 'Warm and practical, full safety rules. Best for most households.' },
  { value: 'concise',   label: 'Concise',   description: 'One-sentence replies, action-first. For power users.' },
  { value: 'technical', label: 'Technical', description: 'Verbose, narrates tool use and reasoning. For developers.' },
  { value: 'warm',      label: 'Warm',      description: 'Conversational and family-friendly. No jargon.' },
]

const LLM_PROVIDERS = [
  { value: 'llamafile', label: 'Llamafile', description: 'Self-contained local LLM binary. Auto-started by GIAP.' },
  { value: 'ollama',    label: 'Ollama',    description: 'Local Ollama server. Start separately with `ollama serve`.' },
  { value: 'local',     label: 'GGUF (local)', description: 'In-process GGUF model via Goose LocalInference. No server needed.' },
]

function load<T>(key: string, fallback: T): T {
  try {
    const v = localStorage.getItem(key)
    return v !== null ? (JSON.parse(v) as T) : fallback
  } catch {
    return fallback
  }
}

export default function Settings({ token }: Props) {
  const { settings: ctxSettings, refetch } = useSettings()

  // ── Appearance ──
  const [theme, setTheme] = useState<Theme>(() => (localStorage.getItem('pond_theme') as Theme) ?? 'system')

  // ── Profile ──
  const [displayName, setDisplayName]     = useState(() => localStorage.getItem('pond_display_name') ?? '')
  const [assistantName, setAssistantName] = useState(() => localStorage.getItem('pond_assistant_name') ?? '')

  // ── Prompt ──
  const [personality, setPersonality]       = useState(() => localStorage.getItem('pond_personality') ?? '')
  const [promptStyle, setPromptStyle]       = useState(() => load('pond_prompt_style', 'balanced'))
  const [promptAddendum, setPromptAddendum] = useState(() => localStorage.getItem('pond_prompt_addendum') ?? '')
  const [customPrompt, setCustomPrompt]     = useState(() => localStorage.getItem('pond_custom_prompt') ?? '')
  const [showAdvanced, setShowAdvanced]     = useState(false)

  // ── LLM pipeline ──
  const [llmProvider, setLlmProvider]   = useState(() => localStorage.getItem('pond_llm_provider') ?? '')
  const [llmModel, setLlmModel]         = useState(() => localStorage.getItem('pond_llm_model') ?? '')
  const [llmMaxTokens, setLlmMaxTokens] = useState(() => load('pond_llm_max_tokens', 1024))
  const [llmTemp, setLlmTemp]           = useState(() => load('pond_llm_temperature', 0.7))
  // Model role assignments
  const [chatProvider,  setChatProvider]  = useState(() => localStorage.getItem('pond_chat_provider') ?? '')
  const [chatModel,     setChatModel]     = useState(() => localStorage.getItem('pond_chat_model') ?? '')
  const [toolModel,     setToolModel]     = useState(() => localStorage.getItem('pond_tool_model') ?? '')
  const [reviewMode,    setReviewMode]    = useState(() => localStorage.getItem('pond_review_mode') ?? 'off')

  // ── Voice ──
  const [wakeWord, setWakeWord]               = useState(() => localStorage.getItem('pond_wake_word') ?? '')
  const [whisperModel, setWhisperModel]       = useState(() => localStorage.getItem('pond_whisper_model') ?? '')
  const [whisperUrl, setWhisperUrl]           = useState(() => localStorage.getItem('pond_whisper_url') ?? '')
  const [recordingDuration, setRecordingDuration] = useState(() => load('pond_recording_duration', 5))
  const [ttsModel, setTtsModel]               = useState(() => localStorage.getItem('pond_tts_model') ?? '')
  const [ttsVoice, setTtsVoice]               = useState(() => localStorage.getItem('pond_tts_voice') ?? '')

  // ── Desktop App (Tauri only) ──
  const isTauri = typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window
  const [serverUrl,  setServerUrl]  = useState('')
  const [canvasPos,  setCanvasPos]  = useState<'left' | 'center' | 'right'>('right')
  const [autoStart,  setAutoStart]  = useState(false)

  // ── UI state ──
  const [saving, setSaving]   = useState(false)
  const [saved, setSaved]     = useState(false)
  const [error, setError]     = useState<string | null>(null)
  const [cleared, setCleared] = useState(false)

  // ── Hydrate from API context (runs every time settings are fetched from server) ──
  useEffect(() => {
    const s = ctxSettings
    if (!s || isPreviewMode(token)) return

    if (s.user_name)             setDisplayName(s.user_name)
    if (s.assistant_name)        setAssistantName(s.assistant_name)
    if (s.assistant_personality) setPersonality(s.assistant_personality)
    if (s.prompt_style)          setPromptStyle(s.prompt_style)
    if (s.prompt_addendum !== undefined) setPromptAddendum(s.prompt_addendum)
    if (s.custom_system_prompt !== undefined) setCustomPrompt(s.custom_system_prompt ?? '')
    // LLM pipeline
    if (s.llm_provider)               setLlmProvider(s.llm_provider)
    if (s.active_llm_model)           setLlmModel(s.active_llm_model)
    if (s.llm_max_tokens)             setLlmMaxTokens(s.llm_max_tokens)
    if (s.llm_temperature !== undefined) setLlmTemp(s.llm_temperature)
    // Model role assignments
    if (s.chat_provider)  setChatProvider(s.chat_provider)
    if (s.chat_model)     setChatModel(s.chat_model)
    if (s.tool_model)     setToolModel(s.tool_model)
    if ((s as any).review_mode) setReviewMode((s as any).review_mode)
    // Voice
    if (s.voice_wake_word)               setWakeWord(s.voice_wake_word)
    if (s.active_whisper_model)          setWhisperModel(s.active_whisper_model)
    if (s.voice_whisper_url)             setWhisperUrl(s.voice_whisper_url)
    if (s.voice_recording_duration_secs) setRecordingDuration(s.voice_recording_duration_secs)
    if (s.active_tts_model)              setTtsModel(s.active_tts_model)
    if (s.voice_tts_voice)               setTtsVoice(s.voice_tts_voice)
  }, [ctxSettings, token])

  // Load desktop settings from Tauri on first render
  useEffect(() => {
    if (!isTauri) return
    ;(async () => {
      try {
        // window.__TAURI__ is injected by Tauri 2.0 — no npm import needed
        const invoke = (window as any).__TAURI__?.core?.invoke
        if (!invoke) return
        const [url, enabled] = await Promise.all([
          invoke('get_server_url') as Promise<string>,
          invoke('is_autostart_enabled') as Promise<boolean>,
        ])
        setServerUrl(url)
        setAutoStart(enabled)
      } catch { /* non-Tauri build or command unavailable */ }
    })()
  }, [isTauri])

  async function handleSave(e: React.FormEvent) {
    e.preventDefault()
    setError(null)
    setSaved(false)
    setSaving(true)

    try {
      // Settings are persisted exclusively via the API; localStorage is no longer
      // written for settings fields (it caused stale-cache divergence from the DB).
      // The API response is refetched after save, which re-hydrates all form fields.
      if (!isPreviewMode(token)) {
        await api.saveSettings(
          {
            user_name:             displayName.trim(),
            assistant_name:        assistantName.trim(),
            assistant_personality: personality.trim(),
            prompt_style:          promptStyle,
            prompt_addendum:       promptAddendum.trim(),
            custom_system_prompt:  customPrompt.trim() || null,
            llm_provider:          llmProvider,
            active_llm_model:      llmModel.trim(),
            llm_max_tokens:        llmMaxTokens,
            llm_temperature:       llmTemp,
            voice_wake_word:              wakeWord.trim(),
            active_whisper_model:         whisperModel.trim(),
            voice_whisper_url:            whisperUrl.trim(),
            voice_recording_duration_secs: recordingDuration,
            active_tts_model:             ttsModel.trim(),
            voice_tts_voice:              ttsVoice.trim(),
            chat_provider:         chatProvider.trim(),
            chat_model:            chatModel.trim(),
            tool_model:            toolModel.trim() || null,
            review_mode:           reviewMode,
          },
          token,
        )
      }

      await refetch()

      // Save desktop-specific settings via Tauri commands (no-op in browser)
      if (isTauri) {
        const invoke = (window as any).__TAURI__?.core?.invoke
        if (invoke) {
          await Promise.all([
            serverUrl.trim() ? invoke('set_server_url', { url: serverUrl.trim() }) : Promise.resolve(),
            autoStart ? invoke('enable_autostart') : invoke('disable_autostart'),
            invoke('position_canvas', { position: canvasPos }),
          ]).catch(() => { /* desktop commands optional */ })
        }
      }

      logActivity('chat', 'Settings updated')
      setSaved(true)
      setTimeout(() => setSaved(false), 3000)
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save settings.')
    } finally {
      setSaving(false)
    }
  }

  function handleThemeChange(value: Theme) {
    setTheme(value)
    localStorage.setItem('pond_theme', value)
    if (value === 'system') {
      delete document.documentElement.dataset.theme
    } else {
      document.documentElement.dataset.theme = value
    }
    // Notify same-tab listeners (App.tsx swaps the logo asset on this).
    window.dispatchEvent(new Event('pond-theme-change'))
  }

  function handleClearActivity() {
    clearActivity()
    setCleared(true)
    setTimeout(() => setCleared(false), 3000)
  }

  function handleClearSession() {
    localStorage.removeItem('pond_session_token')
    localStorage.removeItem('pond_display_name')
    localStorage.removeItem('pond_assistant_name')
    localStorage.removeItem('pond_client_id')
    localStorage.removeItem('pond_devices')
    localStorage.removeItem('pond_activity')
    localStorage.removeItem('pond_personality')
    localStorage.removeItem('pond_prompt_style')
    localStorage.removeItem('pond_prompt_addendum')
    localStorage.removeItem('pond_custom_prompt')
    localStorage.removeItem('pond_theme')
    localStorage.removeItem('pond_llm_provider')
    localStorage.removeItem('pond_llm_model')
    localStorage.removeItem('pond_llm_max_tokens')
    localStorage.removeItem('pond_llm_temperature')
    localStorage.removeItem('pond_wake_word')
    localStorage.removeItem('pond_whisper_model')
    localStorage.removeItem('pond_whisper_url')
    localStorage.removeItem('pond_recording_duration')
    localStorage.removeItem('pond_tts_model')
    localStorage.removeItem('pond_tts_voice')
    delete document.documentElement.dataset.theme
    window.location.reload()
  }

  return (
    <div className="db-page">
      <div className="db-page-header">
        <h1 className="db-page-title">Settings</h1>
        <p className="db-page-subtitle">Customise how your assistant looks and behaves.</p>
      </div>

      <form onSubmit={handleSave} className="db-page-content">

        {/* ── Appearance ── */}
        <div className="db-card">
          <div className="db-card-header"><h3>Appearance</h3></div>
          <div className="db-settings-radio-group">
            {THEMES.map(opt => (
              <label key={opt.value} className={`db-settings-radio-card ${theme === opt.value ? 'selected' : ''}`}>
                <input type="radio" name="theme" value={opt.value} checked={theme === opt.value} onChange={() => handleThemeChange(opt.value)} />
                <div><strong>{opt.label}</strong><span>{opt.description}</span></div>
              </label>
            ))}
          </div>
        </div>

        {/* ── Profile ── */}
        <div className="db-card">
          <div className="db-card-header"><h3>Profile</h3></div>
          <div className="db-settings-field">
            <label className="db-settings-label" htmlFor="displayName">Your name</label>
            <input id="displayName" className="db-settings-input" type="text" value={displayName} onChange={e => setDisplayName(e.target.value)} placeholder="Your name" maxLength={50} />
          </div>
          <div className="db-settings-field">
            <label className="db-settings-label" htmlFor="assistantName">Assistant name</label>
            <input id="assistantName" className="db-settings-input" type="text" value={assistantName} onChange={e => setAssistantName(e.target.value)} placeholder="Goose" maxLength={50} />
          </div>
        </div>

        {/* ── LLM Pipeline ── */}
        <div className="db-card">
          <div className="db-card-header"><h3>LLM Pipeline</h3></div>
          <p className="db-settings-hint">
            Configure your main LLM and optional tool-calling specialist.
            Use the <strong>Models</strong> page to download and manage model files.
          </p>

          {/* Main LLM */}
          <div className="db-settings-field" style={{ borderTop: '1px solid rgba(128,128,128,0.1)', paddingTop: '0.85rem' }}>
            <label className="db-settings-label">Main LLM</label>
            <p className="db-settings-hint" style={{ margin: '0 0 0.5rem' }}>Handles all conversation, reasoning, and responses.</p>
            <div style={{ display: 'flex', gap: '0.5rem', flexWrap: 'wrap' }}>
              <select
                className="db-settings-input"
                style={{ flex: '0 0 auto', width: 'auto', minWidth: '8rem' }}
                value={chatProvider}
                onChange={e => setChatProvider(e.target.value)}
              >
                <option value="">-- provider --</option>
                {LLM_PROVIDERS.map(p => <option key={p.value} value={p.value}>{p.label}</option>)}
              </select>
              <input
                className="db-settings-input"
                style={{ flex: '1 1 10rem' }}
                type="text"
                value={chatModel}
                onChange={e => setChatModel(e.target.value)}
                placeholder="model name"
                maxLength={100}
              />
            </div>
          </div>

          {/* Tool Caller */}
          <div className="db-settings-field" style={{ borderTop: '1px solid rgba(128,128,128,0.1)', paddingTop: '0.85rem' }}>
            <label className="db-settings-label">Tool Caller (optional)</label>
            <p className="db-settings-hint" style={{ margin: '0 0 0.5rem' }}>Small specialist model for generating structured tool-call arguments. Select "None" to use the main LLM.</p>
            <select
              className="db-settings-input"
              value={toolModel}
              onChange={e => setToolModel(e.target.value)}
            >
              <option value="">None (use main LLM for tool calls)</option>
              {/* Populated dynamically from available GGUF models */}
            </select>
            <input
              className="db-settings-input"
              style={{ marginTop: '0.35rem' }}
              type="text"
              value={toolModel}
              onChange={e => setToolModel(e.target.value)}
              placeholder="Or type a model name manually"
              maxLength={100}
            />
          </div>

          {/* Thinking mode */}
          <div className="db-settings-field" style={{ borderTop: '1px solid rgba(128,128,128,0.1)', paddingTop: '0.85rem' }}>
            <label className="db-settings-label">Thinking Mode</label>
            <p className="db-settings-hint" style={{ margin: '0 0 0.5rem' }}>Enable internal reasoning for better analysis, planning, and complex answers.</p>
            <select className="db-settings-input" defaultValue="auto">
              <option value="auto">Auto (enable for capable models)</option>
              <option value="on">Always On</option>
              <option value="off">Off</option>
            </select>
          </div>

          {/* Answer review */}
          <div className="db-settings-field" style={{ borderTop: '1px solid rgba(128,128,128,0.1)', paddingTop: '0.85rem' }}>
            <label className="db-settings-label">Answer Review</label>
            <p className="db-settings-hint" style={{ margin: '0 0 0.5rem' }}>
              Adversarial critic reviews answers for completeness, accuracy, and depth before delivery. Adds ~3-5s per review.
            </p>
            <select
              className="db-settings-input"
              value={reviewMode}
              onChange={e => setReviewMode(e.target.value)}
            >
              <option value="off">Off</option>
              <option value="auto">Auto (review factual/analytical questions only)</option>
              <option value="on">Always On (review every answer)</option>
            </select>
          </div>

          {/* Shared generation params */}
          <div className="db-settings-field" style={{ borderTop: '1px solid rgba(128,128,128,0.1)', paddingTop: '0.85rem' }}>
            <label className="db-settings-label" htmlFor="llmMaxTokens">
              Max tokens <span style={{ opacity: 0.55, fontWeight: 400 }}>({llmMaxTokens})</span>
            </label>
            <p className="db-settings-hint">Maximum tokens in each LLM response. Applies to all roles.</p>
            <input id="llmMaxTokens" className="db-settings-input" type="range" min={128} max={4096} step={128} value={llmMaxTokens} onChange={e => setLlmMaxTokens(Number(e.target.value))} style={{ padding: '0.25rem 0', cursor: 'pointer' }} />
            <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: '0.7rem', opacity: 0.45, marginTop: '0.15rem' }}>
              <span>128</span><span>4096</span>
            </div>
          </div>
          <div className="db-settings-field">
            <label className="db-settings-label" htmlFor="llmTemp">
              Temperature <span style={{ opacity: 0.55, fontWeight: 400 }}>({llmTemp.toFixed(2)})</span>
            </label>
            <p className="db-settings-hint">Controls randomness. Lower = more predictable, higher = more creative.</p>
            <input id="llmTemp" className="db-settings-input" type="range" min={0} max={2} step={0.05} value={llmTemp} onChange={e => setLlmTemp(Number(e.target.value))} style={{ padding: '0.25rem 0', cursor: 'pointer' }} />
            <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: '0.7rem', opacity: 0.45, marginTop: '0.15rem' }}>
              <span>0 (precise)</span><span>2 (creative)</span>
            </div>
          </div>
        </div>

        {/* ── Prompt Style ── */}
        <div className="db-card">
          <div className="db-card-header"><h3>Prompt Style</h3></div>
          <p className="db-settings-hint">
            Controls how the assistant structures its replies.
          </p>
          <div className="db-settings-radio-group">
            {PROMPT_STYLES.map(opt => (
              <label key={opt.value} className={`db-settings-radio-card ${promptStyle === opt.value ? 'selected' : ''}`}>
                <input type="radio" name="promptStyle" value={opt.value} checked={promptStyle === opt.value} onChange={() => setPromptStyle(opt.value)} />
                <div><strong>{opt.label}</strong><span>{opt.description}</span></div>
              </label>
            ))}
          </div>
        </div>

        {/* ── Personality ── */}
        <div className="db-card">
          <div className="db-card-header"><h3>Personality</h3></div>
          <div className="db-settings-field">
            <label className="db-settings-label" htmlFor="personality">Assistant personality</label>
            <p className="db-settings-hint">A short description injected into every conversation.</p>
            <input id="personality" className="db-settings-input" type="text" value={personality} onChange={e => setPersonality(e.target.value)} placeholder="warm and helpful" maxLength={200} />
          </div>
          <div className="db-settings-field">
            <label className="db-settings-label" htmlFor="promptAddendum">Extra instructions</label>
            <p className="db-settings-hint">Appended after every system prompt.</p>
            <textarea id="promptAddendum" className="db-settings-textarea" value={promptAddendum} onChange={e => setPromptAddendum(e.target.value)} placeholder="e.g. Always greet me by name." rows={3} maxLength={500} />
          </div>
        </div>

        {/* ── Voice ── */}
        <div className="db-card">
          <div className="db-card-header"><h3>Speech Recognition (STT)</h3></div>
          <p className="db-settings-hint">
            GIAP uses a local <strong>whisper.cpp</strong> server for transcription. Set the model and server URL below.
            Use the <strong>Models</strong> page to download Whisper models.
          </p>
          <div className="db-settings-field">
            <label className="db-settings-label" htmlFor="wakeWord">Wake word</label>
            <p className="db-settings-hint">Spoken phrase that activates the assistant (case-insensitive).</p>
            <input id="wakeWord" className="db-settings-input" type="text" value={wakeWord} onChange={e => setWakeWord(e.target.value)} placeholder="goose" maxLength={50} />
          </div>
          <div className="db-settings-field">
            <label className="db-settings-label" htmlFor="whisperModel">Whisper model</label>
            <p className="db-settings-hint">
              Model used for transcription: <code>tiny</code>, <code>base</code>, <code>small</code>, <code>medium</code>, <code>large</code>.
              Larger = more accurate but slower. <code>base</code> works well for most home use.
            </p>
            <input id="whisperModel" className="db-settings-input" type="text" value={whisperModel} onChange={e => setWhisperModel(e.target.value)} placeholder="base" maxLength={50} />
          </div>
          <div className="db-settings-field">
            <label className="db-settings-label" htmlFor="whisperUrl">Whisper server URL</label>
            <p className="db-settings-hint">
              URL of the whisper.cpp HTTP server. Leave as default if GIAP manages the process.
              Set a custom URL to use an external or remote whisper server.
            </p>
            <input id="whisperUrl" className="db-settings-input" type="url" value={whisperUrl} onChange={e => setWhisperUrl(e.target.value)} placeholder="http://127.0.0.1:9000" maxLength={200} />
          </div>
          <div className="db-settings-field">
            <label className="db-settings-label" htmlFor="recordingDuration">
              Recording duration <span style={{ opacity: 0.55, fontWeight: 400 }}>({recordingDuration}s)</span>
            </label>
            <p className="db-settings-hint">Seconds of audio captured per voice input. 3–8s is typical for sentences.</p>
            <input id="recordingDuration" className="db-settings-input" type="range" min={2} max={15} step={1} value={recordingDuration} onChange={e => setRecordingDuration(Number(e.target.value))} style={{ padding: '0.25rem 0', cursor: 'pointer' }} />
            <div style={{ display: 'flex', justifyContent: 'space-between', fontSize: '0.7rem', opacity: 0.45, marginTop: '0.15rem' }}>
              <span>2s</span><span>15s</span>
            </div>
          </div>
        </div>

        {/* ── TTS ── */}
        <div className="db-card">
          <div className="db-card-header"><h3>Text-to-Speech (TTS)</h3></div>
          <p className="db-settings-hint">
            GIAP uses <strong>Piper</strong> for local, low-latency text-to-speech.
          </p>

          {/* Active TTS model */}
          <div className="db-settings-field">
            <label className="db-settings-label" htmlFor="ttsModel">Active TTS model</label>
            <p className="db-settings-hint">
              Name from the registry: <code>piper-lessac</code> (or any <code>piper-*</code>) uses the Piper engine.
              Use the <strong>Models</strong> page to see all TTS options.
            </p>
            <input id="ttsModel" className="db-settings-input" type="text" value={ttsModel} onChange={e => setTtsModel(e.target.value)} placeholder="piper-lessac" maxLength={50} />
          </div>

          {/* Piper config */}
          <div style={{ borderTop: '1px solid rgba(128,128,128,0.1)', paddingTop: '0.85rem', marginTop: '0.25rem' }}>
            <p className="db-settings-label" style={{ marginBottom: '0.25rem' }}>Piper (local subprocess)</p>
            <div className="db-settings-field" style={{ marginTop: '0.5rem' }}>
              <label className="db-settings-label" htmlFor="ttsVoice">Voice model filename</label>
              <p className="db-settings-hint">
                ONNX file in <code>$DATA_DIR/models/tts/</code>, e.g. <code>en_US-lessac-medium.onnx</code>.
                Browse{' '}
                <a href="https://rhasspy.github.io/piper-samples/" target="_blank" rel="noopener noreferrer" style={{ color: '#a96ff5' }}>Piper voice samples ↗</a>
                {' '}and{' '}
                <a href="https://huggingface.co/rhasspy/piper-voices/tree/main" target="_blank" rel="noopener noreferrer" style={{ color: '#a96ff5' }}>download voices from HuggingFace ↗</a>.
              </p>
              <input id="ttsVoice" className="db-settings-input" type="text" value={ttsVoice} onChange={e => setTtsVoice(e.target.value)} placeholder="en_US-lessac-medium.onnx" maxLength={200} />
            </div>
          </div>
        </div>

        {/* ── Advanced ── */}
        <div className="db-card">
          <div className="db-card-header" onClick={() => setShowAdvanced(v => !v)} style={{ cursor: 'pointer', userSelect: 'none', display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
            <h3>Advanced</h3>
            <span style={{ fontSize: '0.75rem', opacity: 0.6 }}>{showAdvanced ? '▲' : '▼'}</span>
          </div>
          {showAdvanced && (
            <div className="db-settings-field">
              <label className="db-settings-label" htmlFor="customPrompt">Custom system prompt</label>
              <p className="db-settings-hint">
                Replaces the built-in template entirely. Leave blank to use the Prompt Style above.
                Supports <code>{'{{assistant_name}}'}</code>, <code>{'{{user_name}}'}</code>,{' '}
                <code>{'{{timezone}}'}</code>, <code>{'{{personality}}'}</code>,{' '}
                <code>{'{{location}}'}</code>, <code>{'{{prompt_addendum}}'}</code>.
              </p>
              <textarea id="customPrompt" className="db-settings-textarea" value={customPrompt} onChange={e => setCustomPrompt(e.target.value)} placeholder={`I am {{assistant_name}}, your home assistant.\nI speak only to {{user_name}}. Timezone: {{timezone}}.{{location}}\n{{prompt_addendum}}`} rows={7} maxLength={4000} />
            </div>
          )}
        </div>

        {/* ── Desktop App (Tauri only) ── */}
        {isTauri && (
          <div className="db-card">
            <div className="db-card-header"><h3>Desktop App</h3></div>
            <p className="db-settings-hint">
              Settings specific to the native desktop application.
            </p>

            <div className="db-settings-field">
              <label className="db-settings-label" htmlFor="serverUrl">pond-server URL</label>
              <p className="db-settings-hint">
                Address of the pond-server. Use the default for local setup, or enter your Jetson's IP to connect remotely.
              </p>
              <input
                id="serverUrl"
                className="db-settings-input"
                type="url"
                value={serverUrl}
                onChange={e => setServerUrl(e.target.value)}
                placeholder="http://127.0.0.1:4000"
                maxLength={200}
              />
            </div>

            <div className="db-settings-field">
              <label className="db-settings-label">Canvas overlay position</label>
              <p className="db-settings-hint">Where the canvas overlay snaps when opened via hotkey.</p>
              <div className="db-settings-radio-group" style={{ flexDirection: 'row' }}>
                {(['left', 'center', 'right'] as const).map(pos => (
                  <label key={pos} className={`db-settings-radio-card ${canvasPos === pos ? 'selected' : ''}`} style={{ flex: 1 }}>
                    <input type="radio" name="canvasPos" value={pos} checked={canvasPos === pos} onChange={() => setCanvasPos(pos)} />
                    <div><strong>{pos.charAt(0).toUpperCase() + pos.slice(1)}</strong></div>
                  </label>
                ))}
              </div>
            </div>

            <div className="db-settings-field" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'center' }}>
              <div>
                <label className="db-settings-label">Launch at login</label>
                <p className="db-settings-hint" style={{ marginBottom: 0 }}>Start Goose In A Pond automatically when you log in (tray-only mode).</p>
              </div>
              <label style={{ cursor: 'pointer', display: 'flex', alignItems: 'center', gap: '0.5rem', flexShrink: 0, paddingLeft: '1rem' }}>
                <input type="checkbox" checked={autoStart} onChange={e => setAutoStart(e.target.checked)} />
                <span style={{ fontSize: '0.85rem', opacity: 0.7 }}>{autoStart ? 'On' : 'Off'}</span>
              </label>
            </div>
          </div>
        )}

        {error && <p className="db-error">{error}</p>}

        <div className="db-settings-actions">
          <button type="submit" className="db-btn-primary" disabled={saving}>
            {saving ? 'Saving…' : saved ? 'Saved!' : 'Save Settings'}
          </button>
        </div>
      </form>

      {/* ── Session ── */}
      <div className="db-card db-settings-danger-card">
        <div className="db-card-header"><h3>Session</h3></div>

        <div className="db-settings-danger-row">
          <div>
            <p className="db-settings-danger-title">Clear activity log</p>
            <p className="db-settings-danger-desc">Remove all recent activity entries stored on this device.</p>
          </div>
          <button type="button" className="db-btn-sm" onClick={handleClearActivity}>
            {cleared ? 'Cleared!' : 'Clear Log'}
          </button>
        </div>

        <div className="db-settings-danger-row">
          <div>
            <p className="db-settings-danger-title">Sign out &amp; reset</p>
            <p className="db-settings-danger-desc">Clears all local data and returns to the onboarding screen.</p>
          </div>
          <button type="button" className="db-btn-danger-outline" onClick={handleClearSession}>
            Sign Out
          </button>
        </div>
      </div>
    </div>
  )
}
