use crate::audio::{self, AudioState, WakeListenerState};
use crate::canvas_feed::dispatch_sse_event;
use crate::process::ServerProcess;
use reqwest::multipart;
use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter, State};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptResult {
    pub text: String,
}

/// Quips spoken by Goose while it processes your request.
/// Short phrases — aim for ≤ 2 seconds of synthesised audio each.
const QUIPS: &[&str] = &[
    "On it.",
    "Let me think.",
    "Ruffling through possibilities.",
    "Consulting the pond elders.",
    "Wading into the knowledge pool.",
    "Hatching a response.",
    "Migrating toward an answer.",
    "Paddling upstream.",
    "Preening my thoughts.",
    "Assembling ideas, feather by feather.",
    "Surveying the flock.",
    "Squinting at the data.",
    "Flocking toward clarity.",
    "Skimming the surface.",
];

/// Pick a quip using sub-millisecond time as a cheap source of variety.
fn pick_quip() -> &'static str {
    let idx = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as usize)
        .unwrap_or(0)
        % QUIPS.len();
    QUIPS[idx]
}

/// Begin capturing audio from the microphone.
/// Emits `audio-level` events (f32 0..1) for the VoiceOrb animation.
#[tauri::command]
pub async fn start_recording(
    app: AppHandle,
    audio_state: State<'_, AudioState>,
) -> Result<(), String> {
    let app_clone = app.clone();
    audio::start_capture(&audio_state, move |level| {
        let _ = app_clone.emit("audio-level", level);
    })?;
    let _ = app.emit("recording-started", ());
    Ok(())
}

/// Stop recording. Waits for the capture thread to drain, then returns WAV bytes.
#[tauri::command]
pub async fn stop_recording(
    audio_state: State<'_, AudioState>,
) -> Result<Vec<u8>, String> {
    // Run blocking stop on a thread pool so we don't block the Tauri async runtime
    let samples = audio_state.samples.clone();
    let is_recording = audio_state.is_recording.clone();
    let native_rate = audio_state.native_sample_rate.clone();
    let stop_tx = audio_state.stop_tx.clone();

    tokio::task::spawn_blocking(move || {
        // Signal stop
        if let Some(tx) = stop_tx.lock().unwrap().take() {
            let _ = tx.send(());
        }
        // Wait for thread to finish (max 2s)
        for _ in 0..40 {
            if !is_recording.load(std::sync::atomic::Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        let captured = samples.lock().unwrap().clone();
        if captured.is_empty() {
            return Err("No audio captured".to_string());
        }
        let rate = *native_rate.lock().unwrap();
        // Normalise to 16 kHz — same as the wake listener — so the
        // transcribe endpoint always receives a consistent sample rate.
        let (pcm, wav_rate) = if rate != 16000 {
            (audio::resample_linear(&captured, rate, 16000), 16000u32)
        } else {
            (captured, rate)
        };
        audio::encode_wav(&pcm, wav_rate)
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Abort recording without returning audio.
#[tauri::command]
pub async fn abort_recording(
    audio_state: State<'_, AudioState>,
    app: AppHandle,
) -> Result<(), String> {
    audio::abort_capture(&audio_state);
    let _ = app.emit("recording-aborted", ());
    Ok(())
}

/// VAD-aware recording — matches the CLI's recording technique.
///
/// Opens the mic, waits for speech, records until silence, returns WAV bytes.
/// No countdown timer, no manual stop needed. Emits `audio-level` events
/// for waveform animation during recording.
///
/// Returns empty Vec if no speech is detected within the wait period.
#[tauri::command]
pub async fn record_with_vad(
    app: AppHandle,
) -> Result<Vec<u8>, String> {
    tokio::task::spawn_blocking(move || {
        audio::record_with_vad(
            &app,
            10,   // max 10s waiting for speech to start
            30,   // hard cap on total recording
            400,  // end-of-speech silence threshold (ms)
        )
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Play a short confirmation ping — used to signal wake-word detection.
/// Synthesises a brief two-tone chime (~200ms) using pure math, no assets needed.
#[tauri::command]
pub async fn play_ping() -> Result<(), String> {
    tokio::task::spawn_blocking(|| {
        use rodio::{OutputStream, Sink, Source};
        use std::time::Duration;

        let (_stream, handle) = OutputStream::try_default().map_err(|e| e.to_string())?;
        let sink = Sink::try_new(&handle).map_err(|e| e.to_string())?;
        sink.set_volume(0.35);

        // Two-tone chime: C6 (1047 Hz, 100ms) → E6 (1319 Hz, 120ms)
        let rate = 44100u32;
        let tone = |freq: f32, ms: u64| {
            let samples = (rate as u64 * ms / 1000) as usize;
            let data: Vec<f32> = (0..samples)
                .map(|i| {
                    let t = i as f32 / rate as f32;
                    let envelope = 1.0 - (i as f32 / samples as f32); // linear fade-out
                    (2.0 * std::f32::consts::PI * freq * t).sin() * envelope * 0.6
                })
                .collect();
            rodio::buffer::SamplesBuffer::new(1, rate, data)
        };

        sink.append(tone(1047.0, 100).mix(
            rodio::source::Zero::<f32>::new(1, rate).take_duration(Duration::from_millis(0)),
        ));
        sink.append(tone(1319.0, 120));
        sink.sleep_until_end();
        Ok::<_, String>(())
    })
    .await
    .map_err(|e| e.to_string())?
}

/// Start the passive wake-word listening loop.
///
/// Captures audio in 0.8s chunks, applies a silence gate, transcribes non-silent
/// chunks via the pond-server /api/v1/transcribe endpoint, and emits
/// `wake-word-detected` when `wake_word` is found in the transcript.
///
/// `variants` — calibrated transcription variants from wake-word calibration.
/// If non-empty, the listener uses OR-matching against all variants instead of
/// just the raw wake word. This dramatically improves detection accuracy because
/// Whisper transcribes the same phrase differently across attempts.
#[tauri::command]
pub async fn start_wake_listener(
    app: AppHandle,
    wake_word: String,
    variants: Option<Vec<String>>,
    wake_state: State<'_, WakeListenerState>,
    server: State<'_, ServerProcess>,
) -> Result<(), String> {
    let base_url = server.get_url();
    let variants = variants.unwrap_or_default();
    audio::start_wake_listener(&wake_state, wake_word, variants, base_url, app)
}

/// Stop the passive wake-word listening loop.
#[tauri::command]
pub async fn stop_wake_listener(
    wake_state: State<'_, WakeListenerState>,
) -> Result<(), String> {
    audio::stop_wake_listener(&wake_state);
    Ok(())
}

/// Full voice pipeline:
/// (caller passes WAV bytes from stop_recording) → transcribe → chat stream → speak
///
/// Sentence-level streaming TTS — mirrors the CLI's ChatService pipeline:
///   1. Transcribe user audio via Whisper
///   2. Stream chat response via SSE
///   3. As tokens arrive: filter thinking blocks → split into sentences →
///      strip markdown → normalize symbols → TTS each sentence immediately
///   4. Tool calls are announced audibly ("Let me check the weather.")
///
/// A short quip is synthesised concurrently so the silence between the user
/// speaking and the model's first sentence is filled with audio.
///
/// Emits:
///   `transcript`      — {text: string}
///   `response-token`  — {token: string, done: bool}
///   `tool-result`     — {tool: string, data: object, timestamp_ms: number}
///   `tts-start`
///   `tts-end`
///   `pipeline-error`  — string
#[tauri::command]
pub async fn run_voice_pipeline(
    app: AppHandle,
    wav_bytes: Vec<u8>,
    auth_token: String,
    session_id: Option<String>,
    server: State<'_, ServerProcess>,
) -> Result<(), String> {
    use crate::tts_text;

    let base_url = server.get_url();
    let client = reqwest::Client::new();

    let bearer = if auth_token.is_empty() {
        None
    } else {
        Some(format!("Bearer {}", auth_token))
    };

    // ── 0. Speaker identification — runs concurrently with transcription ─────
    // Sends the same WAV bytes to /speaker/identify-audio. Because both tasks
    // run in parallel the user feels no additional latency: identification
    // resolves while Whisper is still computing the transcript.
    {
        let sid_client  = client.clone();
        let sid_url     = base_url.clone();
        let sid_app     = app.clone();
        let sid_auth    = bearer.clone();
        let sid_wav     = wav_bytes.clone();
        tokio::spawn(async move {
            let part = multipart::Part::bytes(sid_wav)
                .file_name("audio.wav")
                .mime_str("audio/wav")
                .unwrap_or_else(|_| multipart::Part::bytes(vec![]));
            let form = multipart::Form::new().part("audio", part);
            let mut req = sid_client
                .post(format!("{}/api/v1/speaker/identify-audio", sid_url))
                .multipart(form);
            if let Some(auth) = sid_auth {
                req = req.header("Authorization", auth);
            }
            if let Ok(res) = req.send().await {
                if let Ok(json) = res.json::<serde_json::Value>().await {
                    let _ = sid_app.emit("speaker-identified", json);
                }
            }
        });
    }

    // ── 1. Transcribe (quip starts AFTER, not before — avoids talking over user) ──
    let part = multipart::Part::bytes(wav_bytes)
        .file_name("audio.wav")
        .mime_str("audio/wav")
        .map_err(|e| e.to_string())?;
    let form = multipart::Form::new().part("audio", part);

    let mut transcribe_req = client
        .post(format!("{}/api/v1/transcribe", base_url))
        .multipart(form);
    if let Some(ref auth) = bearer {
        transcribe_req = transcribe_req.header("Authorization", auth.as_str());
    }

    let transcript_res = transcribe_req
        .send()
        .await
        .map_err(|e| {
            let msg = format!("Transcribe request failed: {e}");
            let _ = app.emit("pipeline-error", &msg);
            msg
        })?;

    if !transcript_res.status().is_success() {
        let msg = format!("Transcribe error: {}", transcript_res.status());
        let _ = app.emit("pipeline-error", &msg);
        return Err(msg);
    }

    let TranscriptResult { text: raw_transcript } = transcript_res
        .json::<TranscriptResult>()
        .await
        .map_err(|e| format!("Failed to parse transcript: {e}"))?;

    // ── Strip Whisper artifacts (matching CLI's false-positive curbing) ──
    let transcript = tts_text::strip_whisper_artifacts(&raw_transcript);
    if transcript.is_empty() {
        tracing::debug!("Pipeline: artifact-only transcript stripped: {:?}", raw_transcript);
        let _ = app.emit("tts-end", ());
        return Ok(());
    }

    let _ = app.emit("transcript", TranscriptResult { text: transcript.clone() });

    // ── Dismissal / farewell handling (matching CLI's "bye" / "exit") ────
    if let Some((farewell, is_exit)) = tts_text::check_dismissal(&transcript) {
        let _ = app.emit("transcript", TranscriptResult { text: farewell.to_string() });
        // Speak the farewell via TTS
        match fetch_tts_bytes(&client, &base_url, farewell).await {
            Ok(bytes) => { let _ = play_wav_bytes(bytes).await; }
            Err(e)    => { tracing::debug!("Farewell TTS skipped: {e}"); }
        }
        let _ = app.emit("tts-end", ());
        // Emit a special event so VoiceMode knows to reset to wake word
        let _ = app.emit("voice-dismissed", is_exit);
        return Ok(());
    }

    // ── 2. Quip — fills silence during LLM inference (after transcription) ──
    let quip_text   = pick_quip();
    let quip_client = client.clone();
    let quip_url    = base_url.clone();
    let quip_handle = tokio::spawn(async move {
        match fetch_tts_bytes(&quip_client, &quip_url, quip_text).await {
            Ok(bytes) => { let _ = play_wav_bytes(bytes).await; }
            Err(e)    => { tracing::debug!("Quip TTS skipped: {e}"); }
        }
    });

    // ── 3. Chat (streaming SSE) with sentence-level TTS ────────────────────
    crate::canvas_feed::reset_think_filter();
    let effective_session_id = session_id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    let chat_req = serde_json::json!({
        "message": transcript,
        "session_id": effective_session_id
    });

    let mut chat_builder = client
        .post(format!("{}/api/v1/chat/stream", base_url))
        .json(&chat_req);
    if let Some(ref auth) = bearer {
        chat_builder = chat_builder.header("Authorization", auth.as_str());
    }

    let mut chat_res = chat_builder
        .send()
        .await
        .map_err(|e| {
            let msg = format!("Chat request failed: {e}");
            let _ = app.emit("pipeline-error", &msg);
            msg
        })?;

    // ── Thinking tone — loops on a separate thread while the LLM is working ──
    // A subtle rhythmic pulse that fills the silence between the quip and the
    // first real sentence. Stopped via an atomic flag when TTS starts.
    let thinking_active = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let thinking_flag   = thinking_active.clone();
    let thinking_tone   = tokio::task::spawn_blocking(move || {
        use rodio::{OutputStream, Sink};
        use std::sync::atomic::Ordering;

        let Ok((_stream, handle)) = OutputStream::try_default() else { return };
        let Ok(sink) = Sink::try_new(&handle) else { return };
        sink.set_volume(0.08); // very quiet — ambient, not distracting

        // Generate a soft 1-second pulse: gentle sine fade-in/out at 440 Hz
        let rate = 22050u32;
        let pulse_samples = rate as usize; // 1 second
        let pulse: Vec<f32> = (0..pulse_samples)
            .map(|i| {
                let t = i as f32 / rate as f32;
                let envelope = (std::f32::consts::PI * t).sin(); // smooth bell curve
                (2.0 * std::f32::consts::PI * 440.0 * t).sin() * envelope * 0.5
            })
            .collect();

        // Loop the pulse while the flag is set
        while thinking_flag.load(Ordering::Relaxed) {
            let buf = rodio::buffer::SamplesBuffer::new(1, rate, pulse.clone());
            sink.append(buf);
            // Sleep through most of the pulse, checking the flag periodically
            for _ in 0..10 {
                std::thread::sleep(std::time::Duration::from_millis(100));
                if !thinking_flag.load(Ordering::Relaxed) {
                    sink.stop();
                    return;
                }
            }
        }
        sink.stop();
    });

    // Background TTS playback task — receives sentences and plays sequentially.
    // On first sentence: stops the thinking tone, drains the quip, then speaks.
    let (tts_tx, mut tts_rx) = tokio::sync::mpsc::channel::<String>(16);
    let tts_client = client.clone();
    let tts_url    = base_url.clone();
    let tts_app    = app.clone();
    let tts_task   = tokio::spawn(async move {
        let mut quip_done = false;
        let mut quip = Some(quip_handle);

        while let Some(text) = tts_rx.recv().await {
            if !quip_done {
                // Stop the thinking tone — first real content is arriving
                thinking_active.store(false, std::sync::atomic::Ordering::Relaxed);
                if let Some(h) = quip.take() {
                    h.await.ok();
                }
                quip_done = true;
                let _ = tts_app.emit("tts-start", ());
            }
            match fetch_tts_bytes(&tts_client, &tts_url, &text).await {
                Ok(bytes) => { let _ = play_wav_bytes(bytes).await; }
                Err(e)    => { tracing::warn!("Sentence TTS failed: {e}"); }
            }
        }

        // If no sentences were spoken, still clean up
        if !quip_done {
            thinking_active.store(false, std::sync::atomic::Ordering::Relaxed);
            if let Some(h) = quip.take() {
                h.await.ok();
            }
        }
    });

    // ── SSE parsing — extract text for TTS while emitting events to UI ──────
    let mut sentence_buf    = String::new();
    let mut in_think_block  = false;
    // Line buffer for cross-chunk SSE lines — HTTP chunked transfer can split
    // at any byte boundary, so a partial JSON line at the end of one chunk must
    // be joined with the start of the next chunk.
    let mut line_buf = String::new();

    while let Some(chunk) = chat_res
        .chunk()
        .await
        .map_err(|e| format!("Stream error: {e}"))?
    {
        line_buf.push_str(&String::from_utf8_lossy(&chunk));

        // Drain all complete lines from the buffer.
        while let Some(newline_pos) = line_buf.find('\n') {
            let line: String = line_buf[..newline_pos].to_string();
            line_buf = line_buf[newline_pos + 1..].to_string();

            let line = line.trim();
            if line.is_empty() { continue; }

            // Forward every SSE line to the frontend for transcript/card display.
            dispatch_sse_event(&app, line);

            let Some(data) = line.strip_prefix("data: ") else { continue };
            let Ok(val) = serde_json::from_str::<serde_json::Value>(data) else { continue };

            // ��─ Tool call → flush buffer, speak announcement ────────────
            if val.get("type").and_then(|t| t.as_str()) == Some("tool_call") {
                let flushed = sentence_buf.trim().to_string();
                sentence_buf.clear();
                if !flushed.is_empty() {
                    let spoken = tts_text::strip_markdown_for_speech(&flushed);
                    if !spoken.is_empty() {
                        let _ = tts_tx.send(spoken).await;
                    }
                }
                let tool = val.get("tool").and_then(|t| t.as_str()).unwrap_or("unknown");
                let _ = tts_tx.send(tts_text::tool_announcement(tool)).await;
                continue;
            }

            // ── Text token → filter + buffer + split sentences ──────────
            let content = if val.get("type").and_then(|t| t.as_str()) == Some("text") {
                val.get("content").and_then(|c| c.as_str()).map(|s| s.to_string())
            } else {
                val.get("token").and_then(|t| t.as_str()).map(|s| s.to_string())
            };

            if let Some(content) = content {
                let (visible, new_in_think) = tts_text::filter_thinking(&content, in_think_block);
                in_think_block = new_in_think;
                if visible.is_empty() {
                    continue;
                }

                sentence_buf.push_str(&visible);
                let (sentences, remainder) = tts_text::split_sentences(&sentence_buf);
                sentence_buf = remainder;
                for sentence in sentences {
                    let spoken = tts_text::strip_markdown_for_speech(&sentence);
                    if !spoken.is_empty() {
                        let _ = tts_tx.send(spoken).await;
                    }
                }
            }
        }
    }

    // Flush any remaining partial SSE line from the chunk buffer.
    let trailing = line_buf.trim().to_string();
    if !trailing.is_empty() {
        dispatch_sse_event(&app, &trailing);
        if let Some(data) = trailing.strip_prefix("data: ") {
            if let Ok(val) = serde_json::from_str::<serde_json::Value>(data) {
                let content = if val.get("type").and_then(|t| t.as_str()) == Some("text") {
                    val.get("content").and_then(|c| c.as_str()).map(|s| s.to_string())
                } else {
                    val.get("token").and_then(|t| t.as_str()).map(|s| s.to_string())
                };
                if let Some(content) = content {
                    let (visible, _) = tts_text::filter_thinking(&content, in_think_block);
                    if !visible.is_empty() {
                        sentence_buf.push_str(&visible);
                    }
                }
            }
        }
    }

    // ── Flush any remaining sentence buffer ──────────────────────────────────
    let remainder = sentence_buf.trim().to_string();
    if !remainder.is_empty() {
        let spoken = tts_text::strip_markdown_for_speech(&remainder);
        if !spoken.is_empty() {
            let _ = tts_tx.send(spoken).await;
        }
    }

    // Close channel → TTS task drains remaining sentences → exits
    drop(tts_tx);
    tts_task.await.ok();
    // Ensure thinking tone thread is fully stopped
    thinking_tone.await.ok();

    let _ = app.emit("tts-end", ());
    Ok(())
}

/// Fetch synthesised WAV bytes for `text` from the pond-server TTS endpoint.
/// Does NOT play audio — just returns the bytes.
async fn fetch_tts_bytes(
    client: &reqwest::Client,
    base_url: &str,
    text: &str,
) -> Result<Vec<u8>, String> {
    let res = tokio::time::timeout(
        std::time::Duration::from_secs(15),
        client
            .post(format!("{}/api/v1/tts", base_url))
            .json(&serde_json::json!({ "text": text }))
            .send(),
    )
    .await
    .map_err(|_| "TTS fetch timed out".to_string())?
    .map_err(|e| e.to_string())?;

    if !res.status().is_success() {
        return Err(format!("TTS server error: {}", res.status()));
    }

    res.bytes().await.map(|b| b.to_vec()).map_err(|e| e.to_string())
}

/// Play WAV bytes through the system audio output via rodio.
async fn play_wav_bytes(bytes: Vec<u8>) -> Result<(), String> {
    tokio::time::timeout(
        std::time::Duration::from_secs(30),
        tokio::task::spawn_blocking(move || {
            use rodio::{Decoder, OutputStream, Sink};
            let (_stream, handle) = OutputStream::try_default().map_err(|e| e.to_string())?;
            let sink = Sink::try_new(&handle).map_err(|e| e.to_string())?;
            let cursor = std::io::Cursor::new(bytes);
            let source = Decoder::new(cursor).map_err(|e| e.to_string())?;
            sink.append(source);
            sink.sleep_until_end();
            Ok::<_, String>(())
        }),
    )
    .await
    .map_err(|_| "Audio playback timed out".to_string())?
    .map_err(|e| e.to_string())?
}

/// Synthesise `text` and play it — convenience wrapper.
/// No longer used by the sentence-streaming pipeline but kept for utility.
#[allow(dead_code)]
async fn play_tts(client: &reqwest::Client, base_url: &str, text: &str) -> Result<(), String> {
    let bytes = tokio::time::timeout(
        std::time::Duration::from_secs(30),
        fetch_tts_bytes(client, base_url, text),
    )
    .await
    .map_err(|_| "TTS HTTP request timed out after 30s".to_string())??;

    play_wav_bytes(bytes).await
}
