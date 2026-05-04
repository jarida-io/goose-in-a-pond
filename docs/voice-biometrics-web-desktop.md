# Voice Biometrics — Web & Desktop Implementation Spec

This document covers everything needed to add speaker enrollment and identification
to the GIAP web dashboard and Tauri desktop app.

---

## Background — What Already Exists

| Layer | Status |
|-------|--------|
| Speaker model (`speaker.onnx`) | ✅ Downloaded by `pond-server setup` |
| `OnnxSpeakerAdapter` | ✅ `crates/pond-adapters-speaker-embed/src/lib.rs` |
| `SpeakerIdentification` port | ✅ `pond-core/src/ports/speaker_id.rs` |
| `POST /api/v1/profiles/{id}/enroll` | ✅ exists — but records audio **on the server** |
| `DELETE /api/v1/profiles/{id}/biometrics` | ✅ exists |
| `GET /api/v1/profiles/{id}/enroll/status` | ❌ missing |
| Client-side audio upload endpoint | ❌ missing |
| Web enrollment UI | ❌ missing |
| Desktop enrollment UI | ❌ missing |
| Speaker label in web/desktop chat | ❌ missing |

---

## Scope

### 1 — Backend (pond-api)

#### 1a — Status endpoint

**File:** `crates/pond-api/src/routes.rs`

Add a route:
```
GET /api/v1/profiles/{id}/enroll/status
```

Response:
```json
{
  "profile_id": "abc123",
  "enrolled": true,
  "sample_count": 3
}
```

Wire it in the router alongside the existing enroll route:
```rust
.route("/profiles/{id}/enroll/status", get(enroll_speaker_status))
```

Handler pattern (mirrors `enroll_speaker` for state access):
```rust
async fn enroll_speaker_status(
    State(state): State<Arc<AppState>>,
    Path(profile_id): Path<String>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let speaker_id = state.speaker_id.as_ref().ok_or_else(|| (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error": "speaker identification not configured"})),
    ))?;
    let count = speaker_id.enrollment_count(&profile_id).await.unwrap_or(0);
    Ok(Json(json!({
        "profile_id":   profile_id,
        "enrolled":     count > 0,
        "sample_count": count,
    })))
}
```

---

#### 1b — Client-audio upload endpoint

The existing `POST /profiles/{id}/enroll` records audio on the **server's microphone**.
Web and desktop clients need to record on their own microphone and upload the bytes.

Add a new route:
```
POST /api/v1/profiles/{id}/enroll/upload
Content-Type: audio/wav   (or multipart/form-data with field name "audio")
```

The body is raw WAV bytes recorded by the client (16-bit mono, 16 kHz).

Response (same shape as existing enroll):
```json
{
  "embedding_id":   "...",
  "profile_id":     "...",
  "enrolled_count": 2
}
```

Handler:
```rust
async fn enroll_speaker_upload(
    State(state): State<Arc<AppState>>,
    Path(profile_id): Path<String>,
    body: axum::body::Bytes,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let speaker_id = state.speaker_id.as_ref().ok_or_else(|| (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(json!({"error": "speaker identification not configured"})),
    ))?;

    state.profile_repo.get(&profile_id).await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, Json(json!({"error": e.to_string()}))))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, Json(json!({"error": "profile not found"}))))?;

    let embedding = speaker_id.register_speaker(&profile_id, &body).await
        .map_err(|e| (StatusCode::UNPROCESSABLE_ENTITY, Json(json!({"error": e.to_string()}))))?;

    let count = speaker_id.enrollment_count(&profile_id).await.unwrap_or(0);

    Ok(Json(json!({
        "embedding_id":   embedding.id,
        "profile_id":     embedding.profile_id,
        "enrolled_count": count,
    })))
}
```

Wire it:
```rust
.route("/profiles/{id}/enroll/upload", post(enroll_speaker_upload))
```

> **Note on audio format:** The browser `MediaRecorder` API produces WebM/Opus by default.
> Either resample on the client (see Section 2) or add server-side conversion.
> The simplest server-side approach is a `ffmpeg` subprocess call wrapping the bytes before
> passing to `register_speaker`. The cleanest client-side approach uses the Web Audio API
> (see Section 2a).

---

### 2 — Shared API module

Create a shared TypeScript module used by both web and desktop.

**Web file:** `web/src/enrollApi.ts`
**Desktop file:** `pond-desktop/src/enrollApi.ts`

(They are identical )

```typescript
const BASE = '/api/v1'

export interface EnrollStatus {
  profile_id: string
  enrolled: boolean
  sample_count: number
}

export interface EnrollResult {
  embedding_id: string
  profile_id: string
  enrolled_count: number
}

export async function getEnrollStatus(profileId: string, token: string): Promise<EnrollStatus> {
  const res = await fetch(`${BASE}/profiles/${profileId}/enroll/status`, {
    headers: { Authorization: `Bearer ${token}` },
  })
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
  return res.json()
}

export async function uploadSample(profileId: string, wavBytes: ArrayBuffer, token: string): Promise<EnrollResult> {
  const res = await fetch(`${BASE}/profiles/${profileId}/enroll/upload`, {
    method: 'POST',
    headers: {
      'Content-Type': 'audio/wav',
      Authorization: `Bearer ${token}`,
    },
    body: wavBytes,
  })
  if (!res.ok) {
    const err = await res.json().catch(() => ({}))
    throw new Error(err.error ?? `HTTP ${res.status}`)
  }
  return res.json()
}

export async function clearEnrollment(profileId: string, token: string): Promise<void> {
  const res = await fetch(`${BASE}/profiles/${profileId}/biometrics`, {
    method: 'DELETE',
    headers: { Authorization: `Bearer ${token}` },
  })
  if (!res.ok) throw new Error(`HTTP ${res.status}`)
}
```

---

### 2a — Browser audio recording utility

The browser gives WebM/Opus. This utility resamples to 16-bit mono WAV at 16 kHz
using the Web Audio API before uploading.

**File:** `web/src/recordWav.ts` 

```typescript
export async function recordWavSample(durationSeconds: number): Promise<ArrayBuffer> {
  const stream = await navigator.mediaDevices.getUserMedia({ audio: true })
  const ctx = new AudioContext({ sampleRate: 16000 })
  const source = ctx.createMediaStreamSource(stream)
  const bufferSize = 16000 * durationSeconds
  const recorder = ctx.createScriptProcessor(4096, 1, 1)

  return new Promise((resolve, reject) => {
    const samples: Float32Array[] = []
    let collected = 0

    recorder.onaudioprocess = (e) => {
      const chunk = e.inputBuffer.getChannelData(0).slice()
      samples.push(chunk)
      collected += chunk.length
      if (collected >= bufferSize) {
        stream.getTracks().forEach(t => t.stop())
        recorder.disconnect()
        source.disconnect()
        ctx.close()

        // Merge samples
        const merged = new Float32Array(collected)
        let offset = 0
        for (const s of samples) { merged.set(s, offset); offset += s.length }

        // Encode as 16-bit PCM WAV
        resolve(encodeWav(merged, 16000))
      }
    }

    source.connect(recorder)
    recorder.connect(ctx.destination)
  })
}

function encodeWav(samples: Float32Array, sampleRate: number): ArrayBuffer {
  const buffer = new ArrayBuffer(44 + samples.length * 2)
  const view = new DataView(buffer)
  const writeStr = (offset: number, str: string) =>
    [...str].forEach((c, i) => view.setUint8(offset + i, c.charCodeAt(0)))

  writeStr(0, 'RIFF')
  view.setUint32(4, 36 + samples.length * 2, true)
  writeStr(8, 'WAVE')
  writeStr(12, 'fmt ')
  view.setUint32(16, 16, true)
  view.setUint16(20, 1, true)          // PCM
  view.setUint16(22, 1, true)          // mono
  view.setUint32(24, sampleRate, true)
  view.setUint32(28, sampleRate * 2, true)
  view.setUint16(32, 2, true)
  view.setUint16(34, 16, true)
  writeStr(36, 'data')
  view.setUint32(40, samples.length * 2, true)

  let offset = 44
  for (const s of samples) {
    const v = Math.max(-1, Math.min(1, s))
    view.setInt16(offset, v < 0 ? v * 0x8000 : v * 0x7fff, true)
    offset += 2
  }
  return buffer
}
```

---

### 3 — Web UI (`web/src`)

#### Where to add it

Add a **Voice Biometrics** section to the existing Settings page,
under the TTS/voice settings.

#### Component: `VoiceBiometrics.tsx`

**File:** `web/src/components/VoiceBiometrics.tsx`

States to manage:
```typescript
type EnrollPhase = 'idle' | 'recording' | 'uploading' | 'done' | 'error'

const [status, setStatus] = useState<EnrollStatus | null>(null)
const [phase, setPhase] = useState<EnrollPhase>('idle')
const [currentSample, setCurrentSample] = useState(0) // 1, 2, 3
const [errorMsg, setErrorMsg] = useState('')
```

UI flow:
1. On mount → `getEnrollStatus(profileId, token)` → show `Enrolled (3 samples)` or `Not enrolled`
2. **Enroll** button (or **Re-enroll** if already enrolled, with a confirmation dialog)
3. When enrolling:
   - Show `Recording sample 1 of 3… (10 seconds)`
   - Call `recordWavSample(5)` → `uploadSample()`
   - Repeat for samples 2 and 3
   - Show `✅ Enrolled successfully (3 samples)`
4. **Clear** button → confirm dialog → `clearEnrollment()` → refresh status

Rough JSX structure:
```tsx
<section className="settings-section">
  <h3>Voice Biometrics</h3>
  {status === null && <p>Loading…</p>}
  {status && (
    <>
      <p>
        {status.enrolled
          ? `✅ Enrolled — ${status.sample_count} samples stored`
          : '⚠ Not enrolled — the assistant cannot identify your voice'}
      </p>

      {phase === 'recording' && (
        <p>🎙 Recording sample {currentSample} of 3… speak naturally</p>
      )}
      {phase === 'uploading' && <p>⏳ Saving sample {currentSample}…</p>}
      {phase === 'done' && <p>✅ Enrollment complete</p>}
      {phase === 'error' && <p>❌ {errorMsg}</p>}

      <button onClick={startEnroll} disabled={phase !== 'idle'}>
        {status.enrolled ? 'Re-enroll' : 'Enroll'}
      </button>

      {status.enrolled && (
        <button onClick={handleClear} disabled={phase !== 'idle'}>
          Clear enrollment
        </button>
      )}
    </>
  )}
</section>
```

`startEnroll` function:
```typescript
async function startEnroll() {
  if (status?.enrolled) {
    const ok = window.confirm('This will replace your existing voice samples. Continue?')
    if (!ok) return
    await clearEnrollment(profileId, token)
  }
  setPhase('recording')
  try {
    for (let i = 1; i <= 3; i++) {
      setCurrentSample(i)
      setPhase('recording')
      const wav = await recordWavSample(10)
      setPhase('uploading')
      await uploadSample(profileId, wav, token)
    }
    setPhase('done')
    setStatus(await getEnrollStatus(profileId, token))
  } catch (e) {
    setPhase('error')
    setErrorMsg(String(e))
  }
}
```

---

### 4 — Desktop UI (`pond-desktop/src`)

Same component structure as web. Paste `VoiceBiometrics.tsx` into
`pond-desktop/src/components/VoiceBiometrics.tsx` and add it to the Settings panel.

The `recordWavSample` utility works identically in the Tauri webview —
the Web Audio API is available. No Tauri-specific plugin needed.

Add to `pond-desktop/src/sections/Settings.tsx` (or wherever the settings panel lives):
```tsx
import VoiceBiometrics from '../components/VoiceBiometrics'

// Inside the settings JSX:
<VoiceBiometrics profileId={activeProfile.id} token={sessionToken} />
```

---

### 5 — Speaker label in chat

The server already returns a `speaker_id` field in chat responses when a speaker
is identified. Wire it through to the UI.

#### Web (`ChatWidget.tsx`)

In the `onDone` callback of `api.chatStream`, the payload already includes
`model_role`. Extend the `Message` type and the done handler to also capture
`speaker_id` if the server sends it:

```typescript
// In Message type:
speaker?: string

// In onDone callback — extend done payload parsing in api.ts:
const speaker = payload.speaker_id  // add to done event in routes.rs if not already there

// In message rendering:
{msg.speaker && (
  <span className="db-chat-speaker">🎙 {msg.speaker}</span>
)}
```

#### Desktop (`Chat.tsx`)

Same — in the `ev.type === "done"` branch, read `ev.speaker_id` and attach it
to the agent message.

---

## Implementation Order

1. **Backend status endpoint** (`GET .../enroll/status`) — no UI needed, unblocks frontend work
2. **Backend upload endpoint** (`POST .../enroll/upload`) — needed before any client recording works
3. **`recordWav.ts`** utility — can be tested independently in the browser console
4. **`enrollApi.ts`** shared module
5. **`VoiceBiometrics.tsx`** component — build in web first, then copy to desktop
6. **Wire into web Settings page**
7. **Wire into desktop Settings panel**
8. **Speaker label in chat** — both web and desktop

---

## Testing Checklist

- [ ] `GET /profiles/{id}/enroll/status` returns `enrolled: false` for a fresh profile
- [ ] Recording 3 samples via the web UI sets `enrolled: true`, `sample_count: 3`
- [ ] Re-enroll flow shows confirmation dialog and replaces samples
- [ ] Clear button resets status to `enrolled: false`
- [ ] Speaker label appears in chat after enrolling and sending a voice message
- [ ] Desktop enrollment flow works identically to web
- [ ] Attempting to enroll with microphone permission denied shows a clear error message
