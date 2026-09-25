// Browser VoiceBackend on Web Audio and pond-server HTTP; a plain class, no React.

import { api } from "../../api/PondApiClient";
import type {
  VoiceBackend, VoiceState, PipelineOpts, ResponseMeta, ToolCallData,
} from "./VoiceBackend";
import {
  MIC_CONSTRAINTS,
  encodeWav, calculateRms, downsampleTo16k, getAudioContext, closeAudioContext,
  playPingTone, playThinkingTone, splitSentences, stripMarkdown, normalizeForSpeech,
  isWhisperArtifact, checkDismissal, getToolAnnouncement, getQuip,
  filterThinkingFull, createVadState, advanceVad, DEFAULT_VAD_CONFIG,
  registerTtsSource, clearTtsSource, stopTtsPlayback, isTtsInterrupted, resetTtsInterrupt,
} from "./webAudioUtils";

// ── Internal types ───────────────────────────────────────────────

interface RecordingContext {
  stream: MediaStream;
  processor: ScriptProcessorNode;
  source: MediaStreamAudioSourceNode;
  analyser: AnalyserNode;
  audioContext: AudioContext;
  chunks: Float32Array[];
  sampleRate: number;
  levelPump: ReturnType<typeof setInterval> | null;
}

// ── Constants ────────────────────────────────────────────────────

const WAKE_SPEECH_RMS = 0.008;
const WAKE_SILENCE_RMS = 0.004;
const WAKE_ONSET_FRAMES = 2;       // 2 * 30ms = 60ms hysteresis
const WAKE_TAIL_MS = 360;
const WAKE_POST_TRIGGER_MS = 2000;
const WAKE_MIN_SAMPLES = 800;
const WAKE_CONT_SILENCE_MS = 600;

// ── Class ────────────────────────────────────────────────────────

export class WebVoiceBackend implements VoiceBackend {
  // Callbacks (set by orchestration hook)
  onAudioLevel: ((level: number) => void) | null = null;
  onStateChange: ((state: VoiceState) => void) | null = null;
  onTranscript: ((text: string) => void) | null = null;
  onAgentToken: ((token: string, done: boolean) => void) | null = null;
  onToolCall: ((data: ToolCallData) => void) | null = null;
  onError: ((msg: string) => void) | null = null;
  onSessionId: ((id: string) => void) | null = null;
  onResponseMeta: ((meta: ResponseMeta) => void) | null = null;
  onWakeDetected: ((wav: Blob) => void) | null = null;
  onWakeInterrupt: (() => void) | null = null;
  onDismissed: ((isExit: boolean) => void) | null = null;

  private serverUrl: string;
  private cancelled = false;
  private pipelineActive = false;
  private abortController: AbortController | null = null;
  private ttsSource: AudioBufferSourceNode | null = null;
  /** Stops the working tone; null when none is playing, including when it's disabled in settings. */
  private stopThinkingFn: (() => void) | null = null;
  private recording: RecordingContext | null = null;
  private wakeActive = false;
  private wakeDetecting = false;
  private _wakeWord = '';          // stored so runPipeline can restart the listener after finish
  private _wakeNorm: string[] = [];
  private wakeStream: MediaStream | null = null;
  private wakeInterval: ReturnType<typeof setInterval> | null = null;

  // ASR run during the silence-confirm wait; reused only for the identical Blob (reference equality).
  private speculative: { wav: Blob; transcript: string } | null = null;

  // LLM fetch fired on speculative ASR; runPipeline uses it only if the confirmed transcript matches.
  private speculativeLlm: {
    transcript: string;
    response: Promise<Response>;
    abort: AbortController;
    firedAt: number;
    silenceOnset: number;
  } | null = null;

  constructor(serverUrl: string) { this.serverUrl = serverUrl; }

  // ════════════════════════════════════════════════════════════════
  // Public -- VoiceBackend interface
  // ════════════════════════════════════════════════════════════════

  async recordWithVad(authToken?: string, sessionId?: string): Promise<Blob | null> {
    // One mic owner at a time: a prior runPipeline's finally may have restarted the wake listener,
    // and both loops would catch the same utterance and each run the pipeline.
    this.stopWakeInternal();
    this.closeMic();
    this.cancelled = false;

    let ctx: RecordingContext;
    try { ctx = await this.openMic(); }
    catch (err) { this.emitMicError(err); return null; }

    const vad = createVadState();
    const td = new Float32Array(ctx.analyser.fftSize);
    const t0 = Date.now();

    // Transcribe once trailing silence starts, not after silenceTimeoutMs; discarded if speech resumes.
    let speculative: Promise<string | null> | null = null;

    return new Promise<Blob | null>((resolve) => {
      ctx.levelPump = setInterval(() => {
        if (this.cancelled || !this.recording) {
          this.endPump(ctx); resolve(null); return;
        }
        ctx.analyser.getFloatTimeDomainData(td);
        const rms = calculateRms(td);
        this.onAudioLevel?.(rms);

        const wasSpeech = vad.phase === "speech";
        const wasTrailingSilence = vad.phase === "trailing_silence";
        const stop = advanceVad(vad, rms, Date.now(), DEFAULT_VAD_CONFIG);

        if (wasSpeech && vad.phase === "trailing_silence") {
          const silenceOnset = Date.now();
          speculative = this.transcribe(this.collectWav());
          // Start the chat stream once ASR resolves, still inside the silence-confirmation window.
          speculative.then((transcript) => {
            if (!transcript || this.cancelled) return;
            const llmAbort = new AbortController();
            const firedAt = Date.now();
            const headers: Record<string, string> = { "Content-Type": "application/json" };
            if (authToken) headers["Authorization"] = `Bearer ${authToken}`;
            const responsePromise = fetch(`${this.serverUrl}/api/v1/chat/stream`, {
              method: "POST",
              headers,
              // false: the voice prompt garbled small models, and TTS makes any reply speakable. Known cost: it
              // also drops the voice turn cap and `spoken_time`; the fix is splitting the flag, not flipping it.
              body: JSON.stringify({ message: transcript, session_id: sessionId, voice_mode: false }),
              signal: llmAbort.signal,
            });
            this.speculativeLlm = { transcript, response: responsePromise, abort: llmAbort, firedAt, silenceOnset };
          });
        } else if (wasTrailingSilence && vad.phase === "speech") {
          // False pause — discard speculative work
          speculative = null;
          if (this.speculativeLlm) { this.speculativeLlm.abort.abort(); this.speculativeLlm = null; }
        }

        if (Date.now() - t0 >= DEFAULT_VAD_CONFIG.maxDurationMs || stop) {
          this.endPump(ctx);
          const blob = this.blobFromCtx(ctx);
          if (speculative) {
            speculative.then((transcript) => {
              this.speculative = transcript ? { wav: blob, transcript } : null;
            }).finally(() => resolve(blob));
          } else {
            resolve(blob);
          }
        }
      }, 30);
    });
  }

  abortRecording(): void {
    this.cancelled = true;
    this.closeMic();
    this.cancelPipeline();
    this.onAudioLevel?.(0);
  }

  async runPipeline(wav: Blob, opts: PipelineOpts): Promise<void> {
    // Reject concurrent runs: the wake listener and a follow-up recording can both catch one utterance.
    if (this.pipelineActive) return;
    this.cancelled = false;
    this.pipelineActive = true;
    resetTtsInterrupt();
    const ac = new AbortController();
    this.abortController = ac;

    try {
      // Reuse the speculative transcript if it was computed for this same recording.
      const reusable = this.speculative?.wav === wav ? this.speculative.transcript : null;
      this.speculative = null;
      let text = reusable ?? (await this.transcribe(await wav.arrayBuffer()));
      if (this.cancelled || !text) { this.onStateChange?.("idle"); return; }

      // Use the pre-started LLM response if its transcript matches.
      const specLlm = this.speculativeLlm;
      this.speculativeLlm = null;
      const preStartedLlm =
        specLlm && specLlm.transcript === text && !specLlm.abort.signal.aborted
          ? { response: specLlm.response, firedAt: specLlm.firedAt, silenceOnset: specLlm.silenceOnset }
          : null;
      if (specLlm && !preStartedLlm) specLlm.abort.abort();

      if (opts.stripWakeWord) {
        const idx = text.toLowerCase().indexOf(opts.stripWakeWord.toLowerCase());
        if (idx !== -1) text = text.slice(idx + opts.stripWakeWord.length).trim();
        if (!text) {
          this.onStateChange?.("recording");
          const cmd = await this.recordWithVad();
          if (cmd) { this.onStateChange?.("thinking"); return this.runPipeline(cmd, { ...opts, stripWakeWord: undefined }); }
          this.onStateChange?.("idle"); return;
        }
      }

      const dm = checkDismissal(text);
      if (dm.dismissed) {
        const msg = dm.isExit
          ? "Goodbye! I'll be here whenever you need me."
          : "Until next time. Just say my name when you need me.";
        this.onStateChange?.("speaking");
        await this.playTtsSentence(msg, ac.signal);
        this.onDismissed?.(dm.isExit);
        this.onStateChange?.("idle");
        return;
      }

      this.onTranscript?.(text);
      this.onStateChange?.("thinking");

      // Concurrent quip + thinking tone while the LLM streams
      let quipDone = false;
      void this.playTtsSentence(getQuip(), ac.signal).catch(() => {}).then(() => { quipDone = true; });
      // `!== false`: absent means settings haven't loaded, which should sound normal, not mute.
      const stopThink = opts.thinkingTone !== false ? playThinkingTone() : null;
      this.stopThinkingFn = stopThink;

      await this.streamChat(text, opts, ac, () => {
        stopThink?.(); this.stopThinkingFn = null;
        // Stop quip if still playing so first real sentence starts immediately
        if (!quipDone) { stopTtsPlayback(); resetTtsInterrupt(); }
      }, preStartedLlm);

      if (this.stopThinkingFn) { this.stopThinkingFn(); this.stopThinkingFn = null; }
      if (!this.cancelled) this.onStateChange?.("idle");
    } catch (err) {
      if (this.cancelled || (err as Error).name === "AbortError") return;
      console.error("Web voice pipeline error:", err);
      this.onError?.(String(err));
      this.onStateChange?.("error");
    } finally {
      this.pipelineActive = false;
      this.wakeDetecting = false;
      this.abortController = null;
      if (this.stopThinkingFn) { this.stopThinkingFn(); this.stopThinkingFn = null; }
      // Restart the wake listener (it was killed when detection fired).
      if (this._wakeWord && !this.cancelled) {
        this.startWakeListener(this._wakeWord, this._wakeNorm.slice(1));
      }
    }
  }

  cancelPipeline(): void {
    this.cancelled = true;
    this.pipelineActive = false;
    this.wakeDetecting = false;
    // Don't restart the wake listener: after a barge-in, the next runPipeline's finally does.
    this.abortController?.abort(); this.abortController = null;
    if (this.speculativeLlm) { this.speculativeLlm.abort.abort(); this.speculativeLlm = null; }
    if (this.stopThinkingFn) { this.stopThinkingFn(); this.stopThinkingFn = null; }
    // Also sets the interrupt flag, so queued sentences are skipped.
    stopTtsPlayback();
    this.ttsSource = null;
    this.closeMic();
    this.onAudioLevel?.(0);
  }

  startWakeListener(word: string, variants: string[]): void {
    this._wakeWord = word;
    this._wakeNorm = [word.toLowerCase().trim(), ...variants.map((v) => v.toLowerCase().trim()).filter(Boolean)];
    this.stopWakeInternal();
    this.wakeActive = true;
    const norm = [word.toLowerCase().trim()];
    for (const v of variants) {
      const n = v.toLowerCase().trim();
      if (n && !norm.includes(n)) norm.push(n);
    }
    navigator.mediaDevices.getUserMedia(MIC_CONSTRAINTS)
      .then((s) => this.runWakeLoop(s, norm, word))
      .catch((err) => {
        console.warn("Wake listener mic failed:", err);
        this.onError?.("Mic access required for wake word detection.");
        this.onStateChange?.("error");
      });
  }

  stopWakeListener(): void { this.stopWakeInternal(); }
  playPing(): void { playPingTone(); }

  destroy(): void {
    this.cancelPipeline();
    this.stopWakeInternal();
    this.closeMic();
    resetTtsInterrupt();
    closeAudioContext();
  }

  // ════════════════════════════════════════════════════════════════
  // Private -- mic management
  // ════════════════════════════════════════════════════════════════

  private async openMic(): Promise<RecordingContext> {
    const stream = await navigator.mediaDevices.getUserMedia(MIC_CONSTRAINTS);
    const audioContext = new AudioContext();
    if (audioContext.state === "suspended") await audioContext.resume();
    const source = audioContext.createMediaStreamSource(stream);
    const analyser = audioContext.createAnalyser();
    analyser.fftSize = 2048;
    source.connect(analyser);

    const processor = audioContext.createScriptProcessor(4096, 1, 1);
    const chunks: Float32Array[] = [];
    processor.onaudioprocess = (e) => { chunks.push(new Float32Array(e.inputBuffer.getChannelData(0))); };
    source.connect(processor);
    processor.connect(audioContext.destination);

    const ctx: RecordingContext = {
      stream, processor, source, analyser, audioContext,
      chunks, sampleRate: audioContext.sampleRate, levelPump: null,
    };
    this.recording = ctx;
    return ctx;
  }

  private closeMic(): void {
    const ctx = this.recording;
    if (!ctx) return;
    if (ctx.levelPump) clearInterval(ctx.levelPump);
    try { ctx.processor.disconnect(); } catch { /* ok */ }
    try { ctx.source.disconnect(); } catch { /* ok */ }
    ctx.stream.getTracks().forEach((t) => t.stop());
    if (ctx.audioContext.state !== "closed") ctx.audioContext.close().catch(() => {});
    this.recording = null;
  }

  private collectWav(): ArrayBuffer {
    const ctx = this.recording;
    if (!ctx) return encodeWav(new Float32Array(0), 16000);
    const total = ctx.chunks.reduce((s, c) => s + c.length, 0);
    const merged = new Float32Array(total);
    let off = 0;
    for (const c of ctx.chunks) { merged.set(c, off); off += c.length; }
    return encodeWav(downsampleTo16k(merged, ctx.sampleRate), 16000);
  }

  // ════════════════════════════════════════════════════════════════
  // Private -- transcription + TTS
  // ════════════════════════════════════════════════════════════════

  private async transcribe(wavBuffer: ArrayBuffer): Promise<string | null> {
    const r = await api.transcribe(wavBuffer);
    const t = r.text?.trim() ?? "";
    return (!t || isWhisperArtifact(t)) ? null : t;
  }

  private async playTtsSentence(text: string, signal: AbortSignal): Promise<void> {
    if (this.cancelled || signal.aborted || isTtsInterrupted()) return;
    try {
      const res = await fetch(`${this.serverUrl}/api/v1/tts`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ text }),
        signal,
      });
      if (!res.ok) { console.warn("TTS failed:", res.status); return; }

      const data = await res.arrayBuffer();
      if (this.cancelled || signal.aborted || isTtsInterrupted()) return;
      const actx = getAudioContext();
      const buf = await actx.decodeAudioData(data);
      if (this.cancelled || signal.aborted || isTtsInterrupted()) return;

      return new Promise<void>((resolve) => {
        const src = actx.createBufferSource();
        src.buffer = buf;
        src.connect(actx.destination);
        this.ttsSource = src;
        registerTtsSource(src, resolve);
        src.onended = () => { this.ttsSource = null; clearTtsSource(); resolve(); };
        src.start(0);
      });
    } catch (err) {
      if ((err as Error).name !== "AbortError") console.warn("TTS error:", err);
    }
  }

  // ════════════════════════════════════════════════════════════════
  // Private -- SSE chat stream
  // ════════════════════════════════════════════════════════════════

  private async streamChat(
    text: string,
    opts: PipelineOpts,
    controller: AbortController,
    onFirst: () => void,
    preStarted?: { response: Promise<Response>; firedAt: number; silenceOnset: number } | null,
  ): Promise<void> {
    const headers: Record<string, string> = { "Content-Type": "application/json" };
    if (opts.authToken) headers["Authorization"] = `Bearer ${opts.authToken}`;

    const [res, llmFiredAt, silenceOnset, usedSpeculative] = await (async (): Promise<
      [Response, number, number, boolean]
    > => {
      if (preStarted && !controller.signal.aborted) {
        const specRes = await preStarted.response.catch(() => null);
        if (specRes?.ok) return [specRes, preStarted.firedAt, preStarted.silenceOnset, true];
      }
      const firedAt = Date.now();
      const freshRes = await fetch(`${opts.serverUrl}/api/v1/chat/stream`, {
        method: "POST", headers, signal: controller.signal,
        // See the speculative fetch above for why this is false, not true.
        body: JSON.stringify({ message: text, session_id: opts.sessionId, voice_mode: false }),
      });
      return [freshRes, firedAt, firedAt, false];
    })();

    if (!res.ok || !res.body) throw new Error(`Chat failed: ${res.status} ${res.statusText}`);

    const reader = res.body.getReader();
    const decoder = new TextDecoder();
    let sseBuf = "", thinkIn = false, firstSent = false, ttsBuf = "";
    let ttftLogged = false;
    const ttsQ: string[] = [];
    let playing = false;

    const playNext = async (): Promise<void> => {
      if (this.cancelled || isTtsInterrupted() || !ttsQ.length) {
        if (isTtsInterrupted()) ttsQ.length = 0; // drain remaining sentences
        playing = false;
        return;
      }
      playing = true;
      const cleaned = normalizeForSpeech(stripMarkdown(ttsQ.shift()!));
      if (cleaned.trim()) await this.playTtsSentence(cleaned, controller.signal);
      return playNext();
    };

    const enqueue = (s: string): void => {
      if (!firstSent) { firstSent = true; onFirst(); }
      ttsQ.push(s);
      if (!playing) { this.onStateChange?.("speaking"); void playNext(); }
    };

    try {
      while (!this.cancelled) {
        const { value, done } = await reader.read();
        if (done) break;
        if (!ttftLogged) {
          const now = Date.now();
          console.info(`[Q2-26 TTFT] from-fire=${now - llmFiredAt}ms from-silence=${now - silenceOnset}ms (${usedSpeculative ? "speculative" : "fresh"})`);
          ttftLogged = true;
        }
        sseBuf += decoder.decode(value, { stream: true });
        const lines = sseBuf.split("\n");
        sseBuf = lines.pop() ?? "";

        for (const line of lines) {
          const tr = line.trim();
          if (!tr || tr === "data: [DONE]") continue;
          const raw = tr.startsWith("data: ") ? tr.slice(6) : tr;
          let ev: Record<string, unknown>;
          try { ev = JSON.parse(raw); } catch { continue; }

          if (ev.error) { this.onError?.(String(ev.error)); this.onStateChange?.("error"); return; }

          if (ev.type === "text") {
            const tok = (ev.content ?? ev.token ?? "") as string;
            const [vis, blk] = filterThinkingFull(tok, thinkIn);
            thinkIn = blk;
            if (vis) {
              this.onAgentToken?.(vis, false);
              ttsBuf += vis;
              const sents = splitSentences(ttsBuf);
              if (sents.length > 1) {
                for (let i = 0; i < sents.length - 1; i++) enqueue(sents[i]);
                ttsBuf = sents[sents.length - 1];
              }
            }
          }

          if (ev.type === "tool_call") {
            const tn = (ev.tool as string) ?? "tool";
            if (ttsBuf.trim()) { enqueue(ttsBuf.trim()); ttsBuf = ""; }
            enqueue(getToolAnnouncement(tn));
            this.onToolCall?.({ tool: tn, data: (ev.arguments as Record<string, unknown>) ?? {} });
          }

          if (ev.done === true) {
            this.onAgentToken?.("", true);
            if (typeof ev.session_id === "string") this.onSessionId?.(ev.session_id);
            if (ev.model_name && ev.model_role && ev.usage) {
              const u = ev.usage as { completion_tokens?: number };
              this.onResponseMeta?.({
                modelName: ev.model_name as string,
                modelRole: ev.model_role as string,
                completionTokens: u.completion_tokens ?? 0,
              });
            }
          }
        }
      }
    } finally { reader.releaseLock(); }

    if (ttsBuf.trim() && !this.cancelled) enqueue(ttsBuf.trim());

    // Wait for TTS queue to drain (exits immediately if interrupted)
    await new Promise<void>((resolve) => {
      const id = setInterval(() => {
        if (!playing || this.cancelled || isTtsInterrupted()) { clearInterval(id); resolve(); }
      }, 100);
    });
  }

  // ════════════════════════════════════════════════════════════════
  // Private -- wake word listener
  // ════════════════════════════════════════════════════════════════

  private runWakeLoop(stream: MediaStream, normalised: string[], rawWord: string): void {
    if (!this.wakeActive) { stream.getTracks().forEach((t) => t.stop()); return; }

    this.wakeStream = stream;
    const actx = new AudioContext();
    const src = actx.createMediaStreamSource(stream);
    const analyser = actx.createAnalyser();
    analyser.fftSize = 2048;
    src.connect(analyser);

    const proc = actx.createScriptProcessor(4096, 1, 1);
    let capturing = false;
    let chunks: Float32Array[] = [];
    proc.onaudioprocess = (e) => { if (capturing) chunks.push(new Float32Array(e.inputBuffer.getChannelData(0))); };
    src.connect(proc);
    proc.connect(actx.destination);

    const td = new Float32Array(analyser.fftSize);
    let speechStart: number | null = null;
    let onsetFrames = 0;

    this.wakeInterval = setInterval(async () => {
      if (!this.wakeActive) return;
      analyser.getFloatTimeDomainData(td);
      const rms = calculateRms(td);
      this.onAudioLevel?.(rms * 0.3);

      // Speech onset (hysteresis)
      if (!capturing) {
        if (rms >= WAKE_SPEECH_RMS) { if (++onsetFrames >= WAKE_ONSET_FRAMES) { capturing = true; chunks = []; speechStart = Date.now(); onsetFrames = 0; } }
        else onsetFrames = 0;
        return;
      }

      // Speech end detection
      if (!speechStart) return;
      const elapsed = Date.now() - speechStart;
      if (elapsed < WAKE_POST_TRIGGER_MS && !(rms < WAKE_SILENCE_RMS && elapsed > WAKE_TAIL_MS)) return;

      capturing = false;
      speechStart = null;
      const total = chunks.reduce((s, c) => s + c.length, 0);
      if (total < WAKE_MIN_SAMPLES) { chunks = []; return; }

      const merged = new Float32Array(total);
      let off = 0;
      for (const c of chunks) { merged.set(c, off); off += c.length; }
      chunks = [];

      const ds = downsampleTo16k(merged, actx.sampleRate);
      // Skip bursts while a detection is still resolving or a pipeline runs.
      if (this.wakeDetecting || this.pipelineActive) { chunks = []; return; }
      this.wakeDetecting = true;
      try {
        const transcript = await this.transcribe(encodeWav(ds, 16000));
        if (!transcript || !this.wakeActive) { this.wakeDetecting = false; return; }
        if (!normalised.some((w) => transcript.toLowerCase().includes(w))) { this.wakeDetecting = false; return; }

        // Barge-in: cancel running pipeline, leave wakeDetecting=false so listener re-arms.
        if (this.pipelineActive) { this.wakeDetecting = false; this.cancelPipeline(); this.onWakeInterrupt?.(); return; }

        // Clear now: only clearInterval stops async ticks already past the wakeDetecting guard.
        // runPipeline's finally restarts the listener.
        if (this.wakeInterval) { clearInterval(this.wakeInterval); this.wakeInterval = null; }

        playPingTone();
        this.runPostTrigger(analyser, td, ds, (c) => { capturing = c; }, (c) => { chunks = c; });
      } catch { this.wakeDetecting = false; /* transcription failed, ignore in wake mode */ }
    }, 30);
  }

  /** Keep recording 2s after wake word to capture spoken command. */
  private runPostTrigger(
    analyser: AnalyserNode,
    td: Float32Array<ArrayBuffer>,
    seed: Float32Array,
    setCapturing: (v: boolean) => void,
    setChunks: (c: Float32Array[]) => void,
  ): void {
    const postChunks: Float32Array[] = [seed];
    setCapturing(true);
    setChunks(postChunks);

    const t0 = Date.now();
    let silStart: number | null = null;

    const iv = setInterval(() => {
      if (!this.wakeActive) { clearInterval(iv); setCapturing(false); return; }
      analyser.getFloatTimeDomainData(td);
      const rms = calculateRms(td);

      if (rms < WAKE_SILENCE_RMS) {
        if (!silStart) silStart = Date.now();
        else if (Date.now() - silStart > WAKE_CONT_SILENCE_MS) { clearInterval(iv); finish(); return; }
      } else { silStart = null; }

      if (Date.now() - t0 >= WAKE_POST_TRIGGER_MS) { clearInterval(iv); finish(); }
    }, 30);

    const finish = (): void => {
      setCapturing(false);
      const total = postChunks.reduce((s, c) => s + c.length, 0);
      const combined = new Float32Array(total);
      let off = 0;
      for (const c of postChunks) { combined.set(c, off); off += c.length; }
      setChunks([]);
      this.onWakeDetected?.(new Blob([encodeWav(combined, 16000)], { type: "audio/wav" }));
    };
  }

  private stopWakeInternal(): void {
    this.wakeActive = false;
    this.wakeDetecting = false;
    if (this.wakeInterval) { clearInterval(this.wakeInterval); this.wakeInterval = null; }
    if (this.wakeStream) { this.wakeStream.getTracks().forEach((t) => t.stop()); this.wakeStream = null; }
  }

  // ── Helpers ────────────────────────────────────────────────────

  private blobFromCtx(ctx: RecordingContext): Blob {
    this.recording = ctx;
    const wav = this.collectWav();
    this.closeMic();
    return new Blob([wav], { type: "audio/wav" });
  }

  private endPump(ctx: RecordingContext): void {
    if (ctx.levelPump) clearInterval(ctx.levelPump);
    this.onAudioLevel?.(0);
  }

  private emitMicError(err: unknown): void {
    const msg = String(err);
    this.onError?.(/permission|notallowederror|denied/i.test(msg)
      ? "Microphone access denied. Please allow mic access in your browser settings."
      : `Mic error: ${msg}`);
    this.onStateChange?.("error");
  }
}
