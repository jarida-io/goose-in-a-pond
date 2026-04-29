import { useState, useEffect, useRef, useCallback } from 'react'
import { api } from '../api'
import { useSettings } from '../context/SettingsContext'

type LoopState = 'wait' | 'listen' | 'think' | 'speak'

interface Props {
  token: string
}

const STATE_LABEL: Record<LoopState, string> = {
  wait: 'Tap to speak',
  listen: 'Listening…',
  think: 'Thinking…',
  speak: 'Speaking…',
}

export default function VoiceOrb({ token }: Props) {
  const { settings } = useSettings()
  const assistantName = settings?.assistant_name || 'Assistant'
  const [loopState, setLoopState] = useState<LoopState>('wait')
  const [transcript, setTranscript] = useState('')
  const [response, setResponse] = useState('')
  const [muted, setMuted] = useState(() => localStorage.getItem('pond_tts_muted') === 'true')
  const [showOverlay, setShowOverlay] = useState(false)
  const [error, setError] = useState<string | null>(null)

  const mediaRecorderRef = useRef<MediaRecorder | null>(null)
  const audioChunksRef = useRef<Blob[]>([])
  const streamRef = useRef<MediaStream | null>(null)
  const timerRef = useRef<ReturnType<typeof setTimeout> | null>(null)
  const currentAudioRef = useRef<HTMLAudioElement | null>(null)

  // Cancel everything and return to Wait
  const cancel = useCallback(() => {
    if (timerRef.current) clearTimeout(timerRef.current)
    if (mediaRecorderRef.current?.state === 'recording') {
      mediaRecorderRef.current.stop()
    }
    if (streamRef.current) {
      streamRef.current.getTracks().forEach(t => t.stop())
      streamRef.current = null
    }
    if (currentAudioRef.current) {
      currentAudioRef.current.pause()
      currentAudioRef.current = null
    }
    setLoopState('wait')
    setError(null)
  }, [])

  // Escape key always cancels
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === 'Escape' && loopState !== 'wait') cancel()
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [loopState, cancel])

  // The mute toggle now lives only in the chat composer (ChatWidget), but
  // VoiceOrb still gates TTS playback on `muted`.  Subscribe to the shared
  // localStorage key so toggling there immediately silences the orb without
  // a page reload.  The `storage` event fires only on *other* tabs, so we
  // also poll for in-tab updates via a custom event ChatWidget dispatches.
  useEffect(() => {
    function syncFromStorage() {
      setMuted(localStorage.getItem('pond_tts_muted') === 'true')
    }
    window.addEventListener('storage', syncFromStorage)
    window.addEventListener('pond-tts-muted-changed', syncFromStorage)
    return () => {
      window.removeEventListener('storage', syncFromStorage)
      window.removeEventListener('pond-tts-muted-changed', syncFromStorage)
    }
  }, [])

  // Cleanup on unmount
  useEffect(() => () => { cancel() }, [cancel])

  async function handleRecordingStop(chunks: Blob[]) {
    setLoopState('think')
    const blob = new Blob(chunks, { type: 'audio/webm' })

    try {
      // Transcribe via whisper proxy — the server uses its configured whisper URL
      const formData = new FormData()
      formData.append('audio', blob, 'audio.webm')
      const transcribeRes = await fetch('/api/v1/transcribe', {
        method: 'POST',
        body: formData,
      })
      if (!transcribeRes.ok) throw new Error('Transcription failed')
      const { text } = await transcribeRes.json() as { text: string }

      if (!text?.trim()) {
        setLoopState('wait')
        return
      }

      setTranscript(text.trim())
      setShowOverlay(true)

      // Stream chat response
      const sessionId = localStorage.getItem('pond_chat_session_id') ?? undefined
      let replyText = ''
      let newSessionId = ''

      await api.chatStream(
        text.trim(),
        token,
        sessionId,
        (tokenText) => { replyText += tokenText },
        (sid) => { newSessionId = sid },
        (err) => { throw new Error(err) },
      )

      if (newSessionId) localStorage.setItem('pond_chat_session_id', newSessionId)
      setResponse(replyText)

      if (muted || !replyText) {
        setLoopState('wait')
        return
      }

      // Speak via server TTS
      const blobUrl = await api.speak(replyText, token)
      if (!blobUrl) {
        setLoopState('wait')
        return
      }

      setLoopState('speak')
      const audio = new Audio(blobUrl)
      currentAudioRef.current = audio
      audio.onended = () => {
        URL.revokeObjectURL(blobUrl)
        currentAudioRef.current = null
        setLoopState('wait')
      }
      audio.onerror = () => {
        URL.revokeObjectURL(blobUrl)
        currentAudioRef.current = null
        setLoopState('wait')
      }
      void audio.play()

    } catch (err) {
      setError(err instanceof Error ? err.message : 'Something went wrong')
      setLoopState('wait')
    }
  }

  async function startListen() {
    setError(null)
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
        void handleRecordingStop(audioChunksRef.current)
      }

      recorder.start()
      setLoopState('listen')

      // Auto-stop after 5 seconds
      timerRef.current = setTimeout(() => {
        if (mediaRecorderRef.current?.state === 'recording') {
          mediaRecorderRef.current.stop()
        }
      }, 5000)

    } catch {
      setError('Microphone access denied')
      setLoopState('wait')
    }
  }

  function stopListen() {
    if (timerRef.current) clearTimeout(timerRef.current)
    if (mediaRecorderRef.current?.state === 'recording') {
      mediaRecorderRef.current.stop()
    }
  }

  function handleOrbClick() {
    if (loopState === 'wait') {
      void startListen()
    } else if (loopState === 'listen') {
      stopListen()
    } else {
      cancel()
    }
  }

  // When mute flips on (from the chat toolbar) while we're mid-speak,
  // cut the playback immediately and return to Wait — same UX the old
  // floating mute button used to provide.
  useEffect(() => {
    if (muted && loopState === 'speak') {
      if (currentAudioRef.current) {
        currentAudioRef.current.pause()
        currentAudioRef.current = null
      }
      setLoopState('wait')
    }
  }, [muted, loopState])

  return (
    <div className="voice-orb-container" aria-label="Voice assistant">
      {/* Transcript + response overlay */}
      {showOverlay && (transcript || response) && (
        <div className="voice-orb-overlay">
          <button
            className="voice-orb-overlay-close"
            onClick={() => setShowOverlay(false)}
            aria-label="Close"
          >
            ✕
          </button>
          {transcript && (
            <div className="voice-orb-overlay-row">
              <span className="voice-orb-overlay-label">You</span>
              <span className="voice-orb-overlay-text">{transcript}</span>
            </div>
          )}
          {response && (
            <div className="voice-orb-overlay-row">
              <span className="voice-orb-overlay-label">{assistantName}</span>
              <span className="voice-orb-overlay-text">{response}</span>
            </div>
          )}
        </div>
      )}

      {/* Error toast */}
      {error && (
        <div className="voice-orb-error" onClick={() => setError(null)}>
          {error}
        </div>
      )}

      {/* The big orb itself — the only floating control. The small mute
          toggle that used to live next to it has moved into the chat
          composer toolbar, alongside Stop / Clear, where its state is
          easier to see and discoverable from one place. */}
      <div className="voice-orb-controls">
        <button
          className={`voice-orb-btn voice-orb-${loopState}`}
          onClick={handleOrbClick}
          title={STATE_LABEL[loopState]}
          aria-label={STATE_LABEL[loopState]}
          aria-live="polite"
        >
          {loopState === 'wait' && (
            <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
              <path d="M12 1a3 3 0 0 0-3 3v8a3 3 0 0 0 6 0V4a3 3 0 0 0-3-3z" />
              <path d="M19 10v2a7 7 0 0 1-14 0v-2" />
              <line x1="12" y1="19" x2="12" y2="22" />
              <line x1="8" y1="22" x2="16" y2="22" />
            </svg>
          )}
          {loopState === 'listen' && (
            <svg width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
              <rect x="4" y="4" width="16" height="16" rx="2" />
            </svg>
          )}
          {loopState === 'think' && (
            <svg className="voice-orb-spinner-icon" width="22" height="22" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
              <path d="M21 12a9 9 0 1 1-6.219-8.56" />
            </svg>
          )}
          {loopState === 'speak' && (
            <span className="voice-orb-wave">
              <span /><span /><span /><span /><span />
            </span>
          )}
        </button>
      </div>

      <span className="voice-orb-label">{STATE_LABEL[loopState]}</span>
    </div>
  )
}
