import { useState, useRef, useEffect, useCallback } from 'react'
import { api, isPreviewMode } from '../api'
import { useSettings } from '../context/SettingsContext'
import { logActivity } from '../activityLog'

interface Props {
  token: string
}

interface Message {
  id: string
  role: 'user' | 'assistant'
  text: string
  modelRole?: string   // 'chat' | 'think' | 'task' — which role handled this response
  streaming?: boolean  // true while tokens are still arriving
}

interface Suggestion {
  icon: string
  label: string
  prompt: string
}

function getDeviceCount(): { total: number; active: number } {
  try {
    const stored = localStorage.getItem('pond_devices')
    if (!stored) return { total: 0, active: 0 }
    const devices: { id: string; name: string; type: string }[] = JSON.parse(stored)
    return { total: devices.length, active: devices.length }
  } catch {
    return { total: 0, active: 0 }
  }
}

function buildGreeting(userName: string): string {
  const hour = new Date().getHours()
  const salutation = userName.trim() ? `, ${userName.trim()}` : ''
  const { total, active } = getDeviceCount()
  const deviceLine = total > 0
    ? ` ${active} of your ${total} device${total !== 1 ? 's' : ''} ${active === 1 ? 'is' : 'are'} online and ready.`
    : ' No devices connected yet — add one from the Devices page.'

  if (hour < 12) return `Good morning${salutation}.${deviceLine}`
  if (hour < 17) return `Good afternoon${salutation}.${deviceLine}`
  return `Good evening${salutation}.${deviceLine}`
}

function getSuggestions(): Suggestion[] {
  const hour = new Date().getHours()
  if (hour < 12) return [
    { icon: '🌅', label: 'Morning routine',    prompt: 'Start my morning routine — turn on all devices' },
    { icon: '💡', label: 'Turn on devices',    prompt: 'Turn on all my connected devices' },
    { icon: '📋', label: 'What can you do?',   prompt: 'What tasks can you perform for me?' },
    { icon: '📱', label: 'Check devices',      prompt: 'Show me the status of all my devices' },
  ]
  if (hour < 17) return [
    { icon: '📱', label: 'Device status',      prompt: 'What devices are currently active?' },
    { icon: '⏰', label: 'Set a reminder',     prompt: 'Set a reminder for me' },
    { icon: '💡', label: 'Turn off a device',  prompt: 'Turn off a specific device for me' },
    { icon: '📋', label: 'What can you do?',   prompt: 'What tasks can you perform for me?' },
  ]
  return [
    { icon: '🌙', label: 'Bedtime routine',    prompt: 'Set a bedtime routine — turn off all devices and set an alarm for 7am' },
    { icon: '💡', label: 'Turn off all',       prompt: 'Turn off all my connected devices' },
    { icon: '⏰', label: 'Set an alarm',       prompt: 'Set an alarm for tomorrow morning' },
    { icon: '📋', label: 'What can you do?',   prompt: 'What tasks can you perform for me?' },
  ]
}

export default function ChatWidget({ token }: Props) {
  const { settings } = useSettings()
  const userName = settings?.user_name ?? localStorage.getItem('pond_display_name') ?? ''

  const [messages, setMessages] = useState<Message[]>([
    { id: '0', role: 'assistant', text: buildGreeting(localStorage.getItem('pond_display_name') ?? '') },
  ])
  const [input, setInput] = useState('')
  const [loading, setLoading] = useState(false)
  const [recording, setRecording] = useState(false)
  const [converting, setConverting] = useState(false)  // WAV encoding step between recording and transcribing
  const [transcribing, setTranscribing] = useState(false)
  const [voiceError, setVoiceError] = useState<string | null>(null)
  const [showSuggestions, setShowSuggestions] = useState(true)
  const [agentRunning, setAgentRunning] = useState(true)
  const [sessionId, setSessionId] = useState<string | undefined>(() =>
    localStorage.getItem('pond_chat_session_id') ?? undefined
  )
  const [muted, setMuted] = useState(() => localStorage.getItem('pond_tts_muted') === 'true')
  const [speaking, setSpeaking] = useState(false)          // true while TTS audio is playing
  const [speakingMsgId, setSpeakingMsgId] = useState<string | null>(null) // which bubble to glow
  const [historyLoaded, setHistoryLoaded] = useState(false)
  const suggestions = getSuggestions()
  const bottomRef = useRef<HTMLDivElement>(null)
  const mediaRecorderRef = useRef<MediaRecorder | null>(null)
  const audioChunksRef = useRef<Blob[]>([])
  const streamRef = useRef<MediaStream | null>(null)
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const audioRef = useRef<HTMLAudioElement | null>(null)    // current TTS audio element

  // Update greeting when settings load (only if the greeting message is still shown)
  useEffect(() => {
    if (!settings) return
    setMessages(prev => {
      if (prev.length === 1 && prev[0].id === '0' && prev[0].role === 'assistant') {
        return [{ ...prev[0], text: buildGreeting(settings.user_name ?? '') }]
      }
      return prev
    })
  }, [settings?.user_name])

  // Load chat history from backend on mount
  useEffect(() => {
    const storedSessionId = localStorage.getItem('pond_chat_session_id')
    if (!storedSessionId || isPreviewMode(token)) {
      setHistoryLoaded(true)
      return
    }
    api.getSessionMessages(storedSessionId, token)
      .then(res => {
        if (res.messages.length > 0) {
          const loaded: Message[] = res.messages
            .filter(m => m.role === 'user' || m.role === 'assistant')
            .map(m => ({
              id: m.id,
              role: m.role as 'user' | 'assistant',
              text: m.content,
            }))
          setMessages(loaded)
          setShowSuggestions(false)
        }
      })
      .catch(() => { /* no history, keep greeting */ })
      .finally(() => setHistoryLoaded(true))
  }, [token])

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: 'smooth' })
  }, [messages])

  // Stop and clean up any active recording or TTS on unmount
  useEffect(() => {
    return () => {
      if (timerRef.current) clearTimeout(timerRef.current)
      if (mediaRecorderRef.current?.state === 'recording') {
        mediaRecorderRef.current.stop()
      }
      if (streamRef.current) {
        streamRef.current.getTracks().forEach(t => t.stop())
      }
      if (audioRef.current) {
        audioRef.current.pause()
        audioRef.current = null
      }
    }
  }, [])

  // Clear voice errors after 4 seconds
  useEffect(() => {
    if (!voiceError) return
    const t = setTimeout(() => setVoiceError(null), 4000)
    return () => clearTimeout(t)
  }, [voiceError])

  const speakText = useCallback(async (text: string, msgId?: string) => {
    if (muted) return
    const blobUrl = await api.speak(text, token)
    if (!blobUrl) return
    const audio = new Audio(blobUrl)
    audioRef.current = audio
    setSpeaking(true)
    if (msgId) setSpeakingMsgId(msgId)
    audio.onended = () => {
      URL.revokeObjectURL(blobUrl)
      audioRef.current = null
      setSpeaking(false)
      setSpeakingMsgId(null)
    }
    void audio.play()
  }, [muted, token])

  function toggleMute() {
    const next = !muted
    setMuted(next)
    localStorage.setItem('pond_tts_muted', String(next))
    // Stop any current TTS immediately when muting
    if (next && audioRef.current) {
      audioRef.current.pause()
      audioRef.current = null
      setSpeaking(false)
      setSpeakingMsgId(null)
    }
    // `storage` events do not fire in the originating tab, so dispatch a
    // custom event for in-tab listeners (notably VoiceOrb, which gates
    // its TTS playback on this same key).
    window.dispatchEvent(new Event('pond-tts-muted-changed'))
  }

  /**
   * Converts any browser-recorded audio blob (webm, mp4, ogg) into a
   * 16kHz mono 16-bit PCM WAV blob that whisper.cpp accepts.
   *
   * Whisper expects WAV input — the browser's MediaRecorder produces compressed
   * formats (webm/opus on Chrome, mp4/aac on Safari) which whisper rejects with
   * "Invalid request". We use the Web Audio API to decode the compressed audio
   * into raw PCM, mix it down to mono, and re-encode it as a standard WAV file.
   */
  async function encodeWav(audioBlob: Blob): Promise<Blob> {
    const arrayBuffer = await audioBlob.arrayBuffer()

    // Decode the compressed browser audio into raw PCM at 16kHz
    const audioCtx = new AudioContext({ sampleRate: 16000 })
    const decoded = await audioCtx.decodeAudioData(arrayBuffer)
    await audioCtx.close()

    const numSamples = decoded.length
    // Start with the left (or only) channel
    const channelData = decoded.getChannelData(0)

    // If stereo, average both channels to produce mono
    if (decoded.numberOfChannels > 1) {
      const ch1 = decoded.getChannelData(1)
      for (let i = 0; i < numSamples; i++) {
        channelData[i] = (channelData[i] + ch1[i]) / 2
      }
    }

    // Build a standard WAV file: 44-byte header + 16-bit PCM samples
    const dataLen = numSamples * 2 // 2 bytes per i16 sample
    const buf = new ArrayBuffer(44 + dataLen)
    const view = new DataView(buf)
    const writeStr = (off: number, s: string) => {
      for (let i = 0; i < s.length; i++) view.setUint8(off + i, s.charCodeAt(i))
    }

    // RIFF chunk descriptor
    writeStr(0, 'RIFF')
    view.setUint32(4, 36 + dataLen, true)   // file size minus 8 bytes
    writeStr(8, 'WAVE')

    // fmt sub-chunk (PCM format, mono, 16kHz, 16-bit)
    writeStr(12, 'fmt ')
    view.setUint32(16, 16, true)            // sub-chunk size
    view.setUint16(20, 1, true)             // audio format: PCM = 1
    view.setUint16(22, 1, true)             // channels: mono
    view.setUint32(24, 16000, true)         // sample rate: 16kHz
    view.setUint32(28, 32000, true)         // byte rate: 16000 * 1 * 2
    view.setUint16(32, 2, true)             // block align: 1 channel * 2 bytes
    view.setUint16(34, 16, true)            // bits per sample

    // data sub-chunk
    writeStr(36, 'data')
    view.setUint32(40, dataLen, true)

    // Write PCM samples as signed 16-bit little-endian integers
    for (let i = 0; i < numSamples; i++) {
      const s = Math.max(-1, Math.min(1, channelData[i]))
      view.setInt16(44 + i * 2, s < 0 ? s * 32768 : s * 32767, true)
    }

    return new Blob([buf], { type: 'audio/wav' })
  }

  /**
   * Called when the MediaRecorder stops. Converts the recorded chunks to a
   * 16kHz mono WAV, sends it to the server's Whisper transcription endpoint,
   * then auto-submits the resulting text as a chat message.
   *
   * @param chunks   Raw audio chunks from MediaRecorder.ondataavailable
   * @param mimeType The MIME type the browser actually recorded (e.g. audio/webm, audio/mp4)
   */
  async function handleVoiceStop(chunks: Blob[], mimeType: string) {
    if (chunks.length === 0) return
    try {
      // Step 1: convert compressed browser audio to 16kHz mono WAV
      setConverting(true)
      const rawBlob = new Blob(chunks, { type: mimeType })
      const wavBlob = await encodeWav(rawBlob)
      setConverting(false)

      // Step 2: send WAV to whisper for transcription
      setTranscribing(true)
      const formData = new FormData()
      formData.append('audio', wavBlob, 'audio.wav')

      const res = await fetch('/api/v1/transcribe', {
        method: 'POST',
        headers: { 'Authorization': `Bearer ${token}` },
        body: formData,
      })
      if (!res.ok) {
        // Surface the real whisper error (e.g. model not loaded, unsupported format)
        const body = await res.json().catch(() => ({})) as { error?: string }
        throw new Error(body.error || `Transcription failed (${res.status})`)
      }
      const { text } = await res.json() as { text: string }
      if (text?.trim()) {
        setInput(text.trim())
        await sendMessage(text.trim())
      }
    } catch (err) {
      setVoiceError(err instanceof Error ? err.message : 'Transcription failed')
    } finally {
      // Always clear both processing states on completion or error
      setConverting(false)
      setTranscribing(false)
    }
  }

  async function toggleVoice() {
    if (recording) {
      if (timerRef.current) clearTimeout(timerRef.current)
      if (mediaRecorderRef.current?.state === 'recording') {
        mediaRecorderRef.current.stop()
      }
      return
    }

    setVoiceError(null)
    try {
      const stream = await navigator.mediaDevices.getUserMedia({ audio: true })
      streamRef.current = stream
      const recorder = new MediaRecorder(stream)
      mediaRecorderRef.current = recorder
      audioChunksRef.current = []

      recorder.ondataavailable = (e) => {
        if (e.data.size > 0) audioChunksRef.current.push(e.data)
      }
      recorder.onstop = () => {
        if (streamRef.current) {
          streamRef.current.getTracks().forEach(t => t.stop())
          streamRef.current = null
        }
        setRecording(false)
        void handleVoiceStop(audioChunksRef.current, recorder.mimeType)
      }

      recorder.start()
      setRecording(true)

      // Auto-stop after 30 seconds
      timerRef.current = setTimeout(() => {
        if (mediaRecorderRef.current?.state === 'recording') {
          mediaRecorderRef.current.stop()
        }
      }, 30000)
    } catch {
      setVoiceError('Microphone access denied')
    }
  }

  async function sendMessage(text: string) {
    setShowSuggestions(false)
    const userMsg: Message = { id: crypto.randomUUID(), role: 'user', text }
    setMessages(prev => [...prev, userMsg])
    logActivity('chat', text)
    setInput('')
    setLoading(true)

    if (isPreviewMode(token)) {
      await new Promise(r => setTimeout(r, 600))
      const reply = '(Preview mode — connect to a live server to get real responses.)'
      setMessages(prev => [...prev, { id: crypto.randomUUID(), role: 'assistant', text: reply }])
      setLoading(false)
      return
    }

    const assistantMsgId = crypto.randomUUID()
    let fullText = ''
    let streamingStarted = false

    await api.chatStream(
      text,
      token,
      sessionId,
      (tokenText) => {
        fullText += tokenText
        if (!streamingStarted) {
          streamingStarted = true
          setLoading(false)
          setMessages(prev => [
            ...prev,
            { id: assistantMsgId, role: 'assistant', text: tokenText, streaming: true },
          ])
        } else {
          setMessages(prev => {
            const idx = prev.findIndex(m => m.id === assistantMsgId)
            if (idx === -1) return prev
            const updated = { ...prev[idx], text: prev[idx].text + tokenText }
            return [...prev.slice(0, idx), updated, ...prev.slice(idx + 1)]
          })
        }
      },
      (newSessionId, modelRole) => {
        setSessionId(newSessionId)
        localStorage.setItem('pond_chat_session_id', newSessionId)
        setMessages(prev => {
          const idx = prev.findIndex(m => m.id === assistantMsgId)
          if (idx === -1) return prev
          const updated = { ...prev[idx], modelRole, streaming: false }
          return [...prev.slice(0, idx), updated, ...prev.slice(idx + 1)]
        })
        setLoading(false)
        void speakText(fullText, assistantMsgId)
      },
      (err) => {
        const errText = `Error: ${err}`
        if (!streamingStarted) {
          setMessages(prev => [
            ...prev,
            { id: assistantMsgId, role: 'assistant', text: errText },
          ])
        } else {
          setMessages(prev => {
            const idx = prev.findIndex(m => m.id === assistantMsgId)
            if (idx === -1) return prev
            const updated = { ...prev[idx], text: errText, streaming: false }
            return [...prev.slice(0, idx), updated, ...prev.slice(idx + 1)]
          })
        }
        setLoading(false)
      },
    )
  }

  async function handleSend(e: React.FormEvent) {
    e.preventDefault()
    const text = input.trim()
    if (!text) return
    await sendMessage(text)
  }

  function handleSuggestion(prompt: string) {
    setInput(prompt)
  }

  function clearChat() {
    setMessages([{ id: crypto.randomUUID(), role: 'assistant', text: buildGreeting(userName) }])
    setShowSuggestions(true)
    setInput('')
    setSessionId(undefined)
    localStorage.removeItem('pond_chat_session_id')
  }

  function toggleAgent() {
    const next = !agentRunning
    setAgentRunning(next)
    setMessages(prev => [...prev, {
      id: crypto.randomUUID(),
      role: 'assistant',
      text: next
        ? 'Agent started. I\'m back online and ready to help.'
        : 'Agent stopped. I\'m paused — press Start to resume.',
    }])
    setShowSuggestions(false)
  }

  // True when user input should be blocked (processing in progress, not during recording
  // since the user needs to be able to click the mic button to stop recording)
  const voiceBusy = converting || transcribing

  if (!historyLoaded) {
    return <div className="db-loading">Loading conversation…</div>
  }

  return (
    <div className="db-chat">

      {/* Control bar */}
      <div className="db-chat-controls">
        <span className={`db-chat-agent-status ${agentRunning ? 'online' : 'offline'}`}>
          <span className="db-chat-agent-dot" />
          {agentRunning ? 'Agent Online' : 'Agent Offline'}
        </span>
        <div className="db-chat-control-btns">
          {/* Mute/unmute TTS — shows animated sound bars while Goose is speaking */}
          <button
            className={`db-chat-control-btn db-chat-mute-btn ${muted ? 'is-muted' : ''} ${speaking && !muted ? 'speaking' : ''}`}
            onClick={toggleMute}
            title={muted ? 'Unmute voice' : speaking ? 'Goose is speaking — click to mute' : 'Mute voice'}
          >
            {speaking && !muted ? (
              <span className="db-chat-speak-wave-mini">
                <span /><span /><span /><span /><span />
              </span>
            ) : muted ? (
              <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                <line x1="1" y1="1" x2="23" y2="23" />
                <path d="M9 9v3a3 3 0 0 0 5.12 2.12M15 9.34V4a3 3 0 0 0-5.94-.6" />
                <path d="M17 16.95A7 7 0 0 1 5 12v-2m14 0v2a7 7 0 0 1-.11 1.23" />
                <line x1="12" y1="19" x2="12" y2="22" />
              </svg>
            ) : (
              <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
                <path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z" />
                <path d="M19 10v2a7 7 0 0 1-14 0v-2" />
                <line x1="12" y1="19" x2="12" y2="22" />
                <line x1="8" y1="22" x2="16" y2="22" />
              </svg>
            )}
            {speaking && !muted ? 'Speaking' : muted ? 'Unmute' : 'Mute'}
          </button>

          <button
            className={`db-chat-control-btn ${agentRunning ? 'stop' : 'start'}`}
            onClick={toggleAgent}
            disabled={loading}
            title={agentRunning ? 'Stop agent' : 'Start agent'}
          >
            {agentRunning ? (
              <svg width="13" height="13" viewBox="0 0 24 24" fill="currentColor">
                <rect x="4" y="4" width="16" height="16" rx="2" />
              </svg>
            ) : (
              <svg width="13" height="13" viewBox="0 0 24 24" fill="currentColor">
                <polygon points="5,3 19,12 5,21" />
              </svg>
            )}
            {agentRunning ? 'Stop' : 'Start'}
          </button>
          <button
            className="db-chat-control-btn clear"
            onClick={clearChat}
            disabled={loading}
            title="Clear chat"
          >
            <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
              <polyline points="3 6 5 6 21 6" />
              <path d="M19 6l-1 14a2 2 0 0 1-2 2H8a2 2 0 0 1-2-2L5 6" />
              <path d="M10 11v6M14 11v6" />
            </svg>
            Clear
          </button>
        </div>
      </div>

      <div className="db-chat-messages">
        {messages.map(msg => (
          <div key={msg.id} className={`db-chat-msg db-chat-msg-${msg.role}`}>
            {msg.role === 'assistant' && msg.modelRole && msg.modelRole !== 'chat' && (
              <div style={{ marginBottom: '0.2rem' }}>
                <span style={{
                  fontSize: '0.65rem',
                  fontWeight: 600,
                  opacity: 0.55,
                  padding: '0.1rem 0.45rem',
                  borderRadius: '20px',
                  border: '1px solid rgba(169,111,245,0.3)',
                  color: '#a96ff5',
                  background: 'rgba(169,111,245,0.08)',
                  letterSpacing: '0.04em',
                }}>
                  {msg.modelRole === 'think' ? '🧠 Think' : '⚙️ Task'}
                </span>
              </div>
            )}
            <span className={`db-chat-bubble ${msg.streaming ? 'streaming' : ''} ${speakingMsgId === msg.id ? 'speaking' : ''}`}>
              {msg.text}
              {msg.streaming && <span className="db-chat-cursor" />}
            </span>
          </div>
        ))}

        {/* Suggestion chips — shown after greeting, hidden once user sends a message */}
        {showSuggestions && (
          <div className="db-chat-suggestions">
            {suggestions.map(s => (
              <button
                key={s.prompt}
                className="db-chat-suggestion-chip"
                onClick={() => handleSuggestion(s.prompt)}
              >
                <span className="db-chat-suggestion-icon">{s.icon}</span>
                {s.label}
              </button>
            ))}
          </div>
        )}

        {loading && (
          <div className="db-chat-msg db-chat-msg-assistant">
            <span className="db-chat-bubble db-chat-typing">
              <span /><span /><span />
            </span>
          </div>
        )}
        <div ref={bottomRef} />
      </div>

      <form onSubmit={handleSend} className="db-chat-input-row">
        {/* Voice state badges — one shown at a time reflecting the current pipeline step */}
        {recording && (
          <span className="db-chat-listening-badge">
            <span className="db-chat-speak-wave-mini">
              <span /><span /><span /><span /><span />
            </span>
            Listening — tap mic to stop
          </span>
        )}
        {(converting || transcribing) && (
          <span className="db-chat-listening-badge db-chat-listening-badge--transcribing">
            Thinking…
          </span>
        )}
        {speaking && !muted && (
          <span className="db-chat-listening-badge db-chat-listening-badge--speaking">
            <span className="db-chat-speak-wave-mini">
              <span /><span /><span /><span /><span />
            </span>
            Goose is speaking — click Mute to stop
          </span>
        )}
        {voiceError && (
          <span className="db-chat-listening-badge db-chat-listening-badge--error">
            {voiceError}
          </span>
        )}

        <input
          type="text"
          placeholder={
            !agentRunning         ? 'Agent is stopped — press Start to resume…' :
            recording             ? 'Listening — tap mic to stop…' :
            converting || transcribing ? 'Thinking…' :
                                    'Type a message or tap the mic…'
          }
          value={input}
          onChange={e => setInput(e.target.value)}
          // Keep input locked during recording so the user's focus stays on the mic button
          disabled={loading || !agentRunning || recording || voiceBusy}
        />

        <button
          type="button"
          className={`db-chat-mic-btn ${recording ? 'active' : ''} ${converting ? 'converting' : ''} ${transcribing ? 'transcribing' : ''}`}
          onClick={toggleVoice}
          // Locked only during converting/transcribing; enabled during recording so user can tap to stop
          disabled={loading || !agentRunning || voiceBusy}
          title={
            recording             ? 'Tap to stop' :
            converting || transcribing ? 'Thinking…' :
                                    'Voice input'
          }
          aria-label={
            recording             ? 'Stop recording' :
            converting || transcribing ? 'Thinking' :
                                    'Start voice input'
          }
        >
          {converting || transcribing ? (
            // Spinner while Goose processes the audio
            <svg className="db-chat-mic-spinner" width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
              <path d="M21 12a9 9 0 1 1-6.219-8.56" />
            </svg>
          ) : recording ? (
            // Waveform animation — the user is speaking
            <span className="db-chat-speak-wave">
              <span /><span /><span /><span /><span />
            </span>
          ) : (
            // Idle mic icon
            <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
              <rect x="9" y="2" width="6" height="11" rx="3" />
              <path d="M5 10a7 7 0 0 0 14 0" />
              <line x1="12" y1="19" x2="12" y2="22" />
              <line x1="8" y1="22" x2="16" y2="22" />
            </svg>
          )}
        </button>

        <button type="submit" className="db-btn-primary" disabled={loading || !input.trim() || !agentRunning}>
          Send
        </button>
      </form>
    </div>
  )
}
