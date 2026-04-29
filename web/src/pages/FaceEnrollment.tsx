import { useState, useEffect, useRef, useCallback } from 'react'
import { api, isPreviewMode } from '../api'

interface Props {
  token: string
}

interface Profile {
  id: string
  display_name: string
  avatar_emoji: string
}

interface Enrollment {
  id: string
  profile_id: string
  model_dims: number
  created_at: string
}

type FeatureAvailability = 'unknown' | 'available' | 'unavailable'

interface IdentifyResult {
  identified: boolean
  profile_id: string | null
  confidence: number | null
  threshold: number
}

/**
 * Face enrollment & identification page.
 *
 * Connects the web dashboard to the Phase 2 `/api/v1/faces/*` endpoints:
 *   - capture a webcam frame via `getUserMedia`
 *   - POST it to `/faces/register` to enroll a household member
 *   - POST it to `/faces/identify` to look up who is in front of the camera
 *
 * The page degrades gracefully when the backend was built without the
 * `face-onnx` feature (503 on the first call → "unavailable" banner).
 */
export default function FaceEnrollment({ token }: Props) {
  const videoRef = useRef<HTMLVideoElement | null>(null)
  const canvasRef = useRef<HTMLCanvasElement | null>(null)
  const streamRef = useRef<MediaStream | null>(null)

  const [profiles, setProfiles] = useState<Profile[]>([])
  const [selectedProfile, setSelectedProfile] = useState<string>('')
  const [enrollments, setEnrollments] = useState<Enrollment[]>([])
  const [availability, setAvailability] = useState<FeatureAvailability>('unknown')
  const [cameraError, setCameraError] = useState<string | null>(null)
  const [busy, setBusy] = useState<'idle' | 'enrolling' | 'identifying' | 'deleting'>('idle')
  const [lastResult, setLastResult] = useState<IdentifyResult | null>(null)
  const [banner, setBanner] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)

  // ── Profiles ───────────────────────────────────────────────────────────────
  useEffect(() => {
    if (isPreviewMode(token)) return
    api.listProfiles(token)
      .then(res => {
        setProfiles(res.profiles)
        if (res.profiles.length > 0 && !selectedProfile) {
          setSelectedProfile(res.profiles[0].id)
        }
      })
      .catch(() => setError("We couldn't load your household profiles right now. Try again in a moment."))
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [token])

  // ── Camera stream ──────────────────────────────────────────────────────────
  useEffect(() => {
    let cancelled = false
    async function startCamera() {
      if (!navigator.mediaDevices?.getUserMedia) {
        setCameraError('This browser does not support getUserMedia.')
        return
      }
      try {
        // Low-light tuning: ask the webcam ISP for continuous auto-exposure
        // with a +1 EV bias and continuous AWB.  Items in `advanced[]` are
        // best-effort per the MediaCapabilities spec — unsupported entries
        // are silently dropped, so cameras without these capabilities still
        // honour the base resolution + facingMode constraints.
        const stream = await navigator.mediaDevices.getUserMedia({
          video: {
            width:      { ideal: 640 },
            height:     { ideal: 480 },
            facingMode: 'user',
            advanced: [{
              exposureMode:         'continuous',
              exposureCompensation: 1.0,    // +1 EV brighter
              brightness:           128,
              whiteBalanceMode:     'continuous',
              focusMode:            'continuous',
            }] as unknown as MediaTrackConstraintSet[],
          },
          audio: false,
        })
        if (cancelled) {
          stream.getTracks().forEach(t => t.stop())
          return
        }
        streamRef.current = stream
        if (videoRef.current) {
          videoRef.current.srcObject = stream
          await videoRef.current.play().catch(() => {/* autoplay policy */})
        }
      } catch (err) {
        setCameraError(err instanceof Error ? err.message : 'Camera permission denied.')
      }
    }
    startCamera()
    return () => {
      cancelled = true
      streamRef.current?.getTracks().forEach(t => t.stop())
      streamRef.current = null
    }
  }, [])

  // ── Load enrollments for the selected profile ─────────────────────────────
  const refreshEnrollments = useCallback(
    async (profileId: string) => {
      if (!profileId || isPreviewMode(token)) return
      try {
        const res = await api.listFaceEnrollments(profileId, token)
        setEnrollments(res.enrollments)
        setAvailability('available')
      } catch (err) {
        const msg = err instanceof Error ? err.message : String(err)
        if (msg.includes('503') || msg.toLowerCase().includes('not configured')) {
          setAvailability('unavailable')
        } else {
          setError(msg)
        }
      }
    },
    [token],
  )
  useEffect(() => { refreshEnrollments(selectedProfile) }, [selectedProfile, refreshEnrollments])

  // ── Capture a JPEG blob from the current video frame ──────────────────────
  async function captureFrame(): Promise<Blob | null> {
    const video = videoRef.current
    const canvas = canvasRef.current
    if (!video || !canvas || video.videoWidth === 0) return null
    canvas.width = video.videoWidth
    canvas.height = video.videoHeight
    const ctx = canvas.getContext('2d')
    if (!ctx) return null
    ctx.drawImage(video, 0, 0, canvas.width, canvas.height)
    return new Promise(resolve => canvas.toBlob(b => resolve(b), 'image/jpeg', 0.92))
  }

  // ── Actions ───────────────────────────────────────────────────────────────
  async function handleEnroll() {
    setError(null); setBanner(null)
    if (!selectedProfile) {
      setError("Choose who you're enrolling first, then capture a sample.")
      return
    }
    const blob = await captureFrame()
    if (!blob) {
      setError("The camera isn't sending video yet. Give it a moment, then try again.")
      return
    }
    setBusy('enrolling')
    try {
      await api.registerFace(selectedProfile, blob, token)
      const fresh = await api.listFaceEnrollments(selectedProfile, token)
      const n = fresh.enrollments.length
      if (n >= 3) {
        setBanner(`Sample ${n} saved — ${profileName(selectedProfile)} is ready to be recognised. ✅`)
      } else {
        setBanner(`Sample ${n} of 3 saved — capture ${3 - n} more from a slightly different angle for best accuracy.`)
      }
      setEnrollments(fresh.enrollments)
      setAvailability('available')
    } catch (err) {
      handleBackendError(err)
    } finally {
      setBusy('idle')
    }
  }

  async function handleIdentify() {
    // Production-style verification: always capture a 5-frame burst and
    // send it through /faces/identify-burst.  That endpoint runs
    // inter-frame embedding sameness + landmark pixel-motion gates which
    // a single frame cannot perform — so a held-up photo / phone screen
    // comes back as `identified: false, reason: "liveness_failed"`.
    setError(null); setBanner(null); setLastResult(null)
    setBusy('identifying')
    try {
      const frames: Blob[] = []
      for (let i = 0; i < 5; i++) {
        if (i > 0) await new Promise(r => setTimeout(r, 400))
        const blob = await captureFrame()
        if (!blob) {
          setError("The camera isn't sending video yet. Give it a moment, then try again.")
          return
        }
        frames.push(blob)
      }
      const result = await api.identifyFaceBurst(frames, token)
      if (result.reason === 'liveness_failed') {
        setError(
          "We couldn't confirm a real, live face in front of the camera. " +
          "If you're using a real face, look at the camera and blink or move slightly, then try again. " +
          "Photos and phone screens won't work — that's by design, to keep your account safe."
        )
      } else if (result.identified && result.profile_id) {
        setBanner(`Welcome back, ${profileName(result.profile_id)}! 👋`)
      } else if (!result.identified) {
        setBanner(
          "Hi there — we don't recognise this face yet. " +
          "If you're a household member, capture a few enrolment samples first."
        )
      }
      setLastResult({
        identified: result.identified,
        profile_id: result.profile_id,
        confidence: result.confidence,
        threshold: result.threshold,
      })
    } catch (err) {
      handleBackendError(err)
    } finally {
      setBusy('idle')
    }
  }

  async function handleDelete() {
    if (!selectedProfile) return
    if (!confirm(`Delete every biometric record for ${profileName(selectedProfile)}? This cannot be undone.`)) return
    setError(null); setBanner(null)
    setBusy('deleting')
    try {
      const res = await api.deleteUserBiometrics(selectedProfile, token)
      const n = res.face_embeddings_deleted
      setBanner(
        n === 0
          ? `${profileName(selectedProfile)} had nothing on file — you're all clear.`
          : `Removed ${n} face sample${n === 1 ? '' : 's'} for ${profileName(selectedProfile)}.`
      )
      await refreshEnrollments(selectedProfile)
    } catch (err) {
      handleBackendError(err)
    } finally {
      setBusy('idle')
    }
  }

  function handleBackendError(err: unknown) {
    const msg = err instanceof Error ? err.message : String(err)
    if (msg.includes('503') || msg.toLowerCase().includes('not configured')) {
      setAvailability('unavailable')
      setError("Face recognition isn't enabled on this server yet. Ask your installer to rebuild it with face support turned on.")
    } else if (msg.includes('401')) {
      setError('Your session expired. Refresh the page to sign back in.')
    } else if (/HTTP 5\d\d|fetch/i.test(msg)) {
      setError('Something went wrong on the server. Please try again in a few seconds.')
    } else {
      setError("That didn't work — please try again. If it keeps happening, refresh the page.")
      console.warn('[FaceEnrollment] action failed:', msg)
    }
  }

  // ── Helpers ───────────────────────────────────────────────────────────────
  function profileName(id: string): string {
    const p = profiles.find(p => p.id === id)
    return p ? `${p.avatar_emoji} ${p.display_name}` : id.slice(0, 8)
  }
  function pct(v: number | null | undefined): string {
    if (v === null || v === undefined) return '—'
    return `${(v * 100).toFixed(1)}%`
  }

  // ── Render ────────────────────────────────────────────────────────────────
  return (
    <div className="db-page">
      <div className="db-page-header">
        <h1 className="db-page-title">Face Enrollment</h1>
        <p className="db-page-subtitle">
          Register a face for each household member so the assistant can tell who is talking.
          Raw images are never stored — only compact embedding vectors that cannot be reversed.
        </p>
      </div>

      <div className="db-page-content">
        {availability === 'unavailable' && (
          <div style={bannerStyle('#fef3c7', '#92400e')}>
            <strong>Face recognition isn't turned on yet.</strong>{' '}
            Ask whoever set up Goose for you to rebuild the server with face support enabled, then come back to this page.
          </div>
        )}

        {error && <div style={bannerStyle('#fee2e2', '#991b1b')}>{error}</div>}
        {banner && <div style={bannerStyle('#dcfce7', '#065f46')}>{banner}</div>}

        <div style={{ display: 'grid', gridTemplateColumns: 'minmax(280px, 1fr) minmax(280px, 1fr)', gap: 20, marginTop: 20 }}>
          {/* ── Camera panel ─────────────────────────────────────────────── */}
          <section style={panelStyle}>
            <h2 style={panelHeading}>Camera</h2>
            {cameraError ? (
              <p style={{ color: '#991b1b' }}>{cameraError}</p>
            ) : (
              <div style={{ position: 'relative' }}>
                <video
                  ref={videoRef}
                  muted
                  playsInline
                  style={{ width: '100%', borderRadius: 8, background: '#000', aspectRatio: '4/3' }}
                />
                {/* Centered guide showing the crop window the server will use. */}
                <div style={cropGuideStyle} aria-hidden />
              </div>
            )}
            <canvas ref={canvasRef} style={{ display: 'none' }} />
            <p style={{ fontSize: 12, color: 'var(--text-muted)', marginTop: 8 }}>
              Keep your face inside the dashed square. The server auto-crops to the largest
              centered square when no bounding box is supplied.
            </p>
          </section>

          {/* ── Controls panel ───────────────────────────────────────────── */}
          <section style={panelStyle}>
            <h2 style={panelHeading}>Enroll & Identify</h2>

            <label style={{ display: 'block', fontSize: 13, fontWeight: 500, marginBottom: 4 }}>
              Household member
            </label>
            <select
              value={selectedProfile}
              onChange={e => setSelectedProfile(e.target.value)}
              disabled={profiles.length === 0 || busy !== 'idle'}
              style={selectStyle}
            >
              {profiles.length === 0 && <option value="">No profiles yet — create one first</option>}
              {profiles.map(p => (
                <option key={p.id} value={p.id}>{p.avatar_emoji} {p.display_name}</option>
              ))}
            </select>

            <div style={{ display: 'flex', gap: 10, marginTop: 16, flexWrap: 'wrap' }}>
              <button
                onClick={handleEnroll}
                disabled={busy !== 'idle' || !selectedProfile || availability === 'unavailable'}
                style={primaryButton}
              >
                {busy === 'enrolling' ? 'Enrolling…' : 'Enroll sample'}
              </button>
              <button
                onClick={handleIdentify}
                disabled={busy !== 'idle' || availability === 'unavailable'}
                style={secondaryButton}
              >
                {busy === 'identifying' ? 'Identifying…' : 'Identify face'}
              </button>
              <button
                onClick={handleDelete}
                disabled={busy !== 'idle' || enrollments.length === 0 || !selectedProfile}
                style={dangerButton}
              >
                {busy === 'deleting' ? 'Deleting…' : 'Delete biometrics'}
              </button>
            </div>

            {lastResult && (
              <div style={{ marginTop: 16, padding: 12, background: 'var(--surface-muted)', color: 'var(--text-primary)', borderRadius: 8 }}>
                {lastResult.identified ? (
                  <>
                    <div style={{ fontSize: 14, fontWeight: 600, marginBottom: 4 }}>
                      Recognised as {profileName(lastResult.profile_id ?? '')}
                    </div>
                    <div style={{ fontSize: 12, color: 'var(--text-muted)' }}>
                      Confidence {pct(lastResult.confidence)} · this looks like a strong match.
                    </div>
                  </>
                ) : (
                  <>
                    <div style={{ fontSize: 14, fontWeight: 600, marginBottom: 4 }}>
                      No matching profile found
                    </div>
                    <div style={{ fontSize: 12, color: 'var(--text-muted)' }}>
                      {lastResult.confidence != null && lastResult.confidence > 0
                        ? "We weren't confident enough to make a match. Try a brighter spot, or enrol another sample under similar lighting."
                        : "We couldn't find a face in the camera view. Make sure your face is centred in the dashed square and try again."}
                    </div>
                  </>
                )}
              </div>
            )}
          </section>
        </div>

        {/* ── Enrollments list ─────────────────────────────────────────────── */}
        <section style={{ ...panelStyle, marginTop: 20 }}>
          <h2 style={panelHeading}>
            Enrollments for {selectedProfile ? profileName(selectedProfile) : '—'}
            {enrollments.length > 0 && <span style={{ fontSize: 13, fontWeight: 400, color: 'var(--text-muted)' }}> · {enrollments.length} sample(s)</span>}
          </h2>
          {enrollments.length === 0 ? (
            <p style={{ color: 'var(--text-muted)', fontSize: 14 }}>
              No samples enrolled yet. Capture 3+ samples (different lighting / angles) for best accuracy.
            </p>
          ) : (
            <ul style={{ listStyle: 'none', padding: 0, margin: 0 }}>
              {enrollments.map(e => (
                <li key={e.id} style={{ display: 'flex', justifyContent: 'space-between', padding: '8px 0', borderBottom: '1px solid var(--surface-border)', fontSize: 13, color: 'var(--text-primary)' }}>
                  <code style={{ color: 'var(--text-muted)' }}>{e.id.slice(0, 12)}…</code>
                  <span>{e.model_dims}-d</span>
                  <span style={{ color: 'var(--text-muted)' }}>{new Date(e.created_at).toLocaleString()}</span>
                </li>
              ))}
            </ul>
          )}
        </section>
      </div>
    </div>
  )
}

// ── Inline styles (kept here to avoid reshuffling global CSS) ───────────────
// All surface / text tokens route through CSS variables so light + dark
// themes render legibly without per-style overrides.
const panelStyle: React.CSSProperties = {
  background: 'var(--surface)',
  border: '1px solid var(--surface-border)',
  borderRadius: 12,
  padding: 16,
  color: 'var(--text-primary)',
}
const panelHeading: React.CSSProperties = {
  margin: '0 0 12px',
  fontSize: 15,
  fontWeight: 600,
  color: 'var(--text-primary)',
}
const selectStyle: React.CSSProperties = {
  width: '100%',
  padding: '8px 10px',
  borderRadius: 8,
  border: '1px solid var(--input-border)',
  fontSize: 14,
  background: 'var(--input-bg)',
  color: 'var(--input-text)',
}
const baseButton: React.CSSProperties = {
  padding: '8px 14px',
  borderRadius: 8,
  fontSize: 14,
  fontWeight: 500,
  border: 'none',
  cursor: 'pointer',
}
const primaryButton: React.CSSProperties = {
  ...baseButton, background: '#2563eb', color: '#fff',
}
const secondaryButton: React.CSSProperties = {
  ...baseButton,
  background: 'var(--btn-secondary-bg)',
  color: 'var(--btn-secondary-text)',
}
const dangerButton: React.CSSProperties = {
  ...baseButton, background: '#fee2e2', color: '#991b1b',
}
const cropGuideStyle: React.CSSProperties = {
  position: 'absolute',
  top: '50%', left: '50%',
  width: '60%', aspectRatio: '1/1',
  transform: 'translate(-50%, -50%)',
  border: '2px dashed rgba(255,255,255,0.75)',
  borderRadius: 8,
  pointerEvents: 'none',
}
function bannerStyle(bg: string, fg: string): React.CSSProperties {
  return {
    background: bg, color: fg, padding: '10px 14px', borderRadius: 8,
    fontSize: 14, marginTop: 8,
  }
}
