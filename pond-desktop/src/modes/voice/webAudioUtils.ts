import type { AudioAttachment } from "../../api/types";

// ── WAV Encoding ──────────────────────────────────────────────────

/**
 * Shared by every capture path. 16 kHz mono spares transcription a resample; echo cancellation
 * stops the assistant hearing its own speech as a new utterance.
 */
export const MIC_CONSTRAINTS: MediaStreamConstraints = {
  audio: {
    sampleRate: { ideal: 16000 },
    channelCount: { exact: 1 },
    echoCancellation: true,
    noiseSuppression: true,
  } as MediaTrackConstraints,
};

/** Raw base64 of `bytes`, in chunks: a whole recording spread into one call overflows the stack. */
export function base64OfBytes(bytes: ArrayBuffer): string {
  const view = new Uint8Array(bytes);
  let binary = "";
  for (let i = 0; i < view.length; i += 0x8000) {
    binary += String.fromCharCode(...view.subarray(i, i + 0x8000));
  }
  return btoa(binary);
}

/** A spoken turn's recording, with what Whisper heard in it: the model hears the clip where it
 *  can, and the pond uses the words where it cannot. */
export function spokenClip(words: string, wav: ArrayBuffer): AudioAttachment {
  return { data: base64OfBytes(wav), mime_type: "audio/wav", heard_as: words };
}

/** 16-bit mono WAV (44-byte header) of samples in [-1, 1], as pond-server's Whisper endpoint takes. */
export function encodeWav(samples: Float32Array, sampleRate: number): ArrayBuffer {
  const numChannels = 1;
  const bitsPerSample = 16;
  const byteRate = sampleRate * numChannels * (bitsPerSample / 8);
  const blockAlign = numChannels * (bitsPerSample / 8);
  const dataSize = samples.length * (bitsPerSample / 8);
  const headerSize = 44;
  const buffer = new ArrayBuffer(headerSize + dataSize);
  const view = new DataView(buffer);

  // RIFF header
  writeString(view, 0, "RIFF");
  view.setUint32(4, 36 + dataSize, true);
  writeString(view, 8, "WAVE");

  // fmt chunk
  writeString(view, 12, "fmt ");
  view.setUint32(16, 16, true);             // chunk size
  view.setUint16(20, 1, true);              // PCM format
  view.setUint16(22, numChannels, true);
  view.setUint32(24, sampleRate, true);
  view.setUint32(28, byteRate, true);
  view.setUint16(32, blockAlign, true);
  view.setUint16(34, bitsPerSample, true);

  // data chunk
  writeString(view, 36, "data");
  view.setUint32(40, dataSize, true);

  // PCM samples -- clamp to 16-bit signed integer range
  let offset = 44;
  for (let i = 0; i < samples.length; i++) {
    const clamped = Math.max(-1, Math.min(1, samples[i]));
    const int16 = clamped < 0 ? clamped * 0x8000 : clamped * 0x7FFF;
    view.setInt16(offset, int16, true);
    offset += 2;
  }

  return buffer;
}

function writeString(view: DataView, offset: number, str: string): void {
  for (let i = 0; i < str.length; i++) {
    view.setUint8(offset + i, str.charCodeAt(i));
  }
}

// ── RMS Calculation ───────────────────────────────────────────────

/** RMS of the buffer: 0 to 1 for samples in [-1, 1]. */
export function calculateRms(data: Float32Array): number {
  if (data.length === 0) return 0;
  let sum = 0;
  for (let i = 0; i < data.length; i++) {
    sum += data[i] * data[i];
  }
  return Math.sqrt(sum / data.length);
}

// ── VAD Configuration ─────────────────────────────────────────────

export interface VadConfig {
  /** RMS above which speech is active. */
  speechOnsetThreshold: number;
  /** Sustained ms above the onset threshold that confirm speech. */
  speechOnsetDurationMs: number;
  /** RMS below which, after speech, silence is detected. */
  silenceThreshold: number;
  /** Silence in ms before recording stops. */
  silenceTimeoutMs: number;
  /** Maximum recording length in ms. */
  maxDurationMs: number;
}

export const DEFAULT_VAD_CONFIG: VadConfig = {
  speechOnsetThreshold: 0.010,
  speechOnsetDurationMs: 60,
  silenceThreshold: 0.005,
  silenceTimeoutMs: 400,
  maxDurationMs: 30_000,
};

// ── VAD State Machine ─────────────────────────────────────────────

export type VadPhase = "waiting" | "speech" | "trailing_silence";

export interface VadState {
  phase: VadPhase;
  /** Timestamp when speech onset was first detected (for onset duration check) */
  onsetStart: number | null;
  /** Timestamp when trailing silence began */
  silenceStart: number | null;
  /** Whether any speech was confirmed during this recording */
  speechConfirmed: boolean;
}

export function createVadState(): VadState {
  return {
    phase: "waiting",
    onsetStart: null,
    silenceStart: null,
    speechConfirmed: false,
  };
}

/** Feeds one RMS reading; returns true when recording should stop (end of speech or timeout). */
export function advanceVad(
  state: VadState,
  rms: number,
  now: number,
  config: VadConfig,
): boolean {
  switch (state.phase) {
    case "waiting":
      if (rms >= config.speechOnsetThreshold) {
        if (state.onsetStart === null) {
          state.onsetStart = now;
        } else if (now - state.onsetStart >= config.speechOnsetDurationMs) {
          state.phase = "speech";
          state.speechConfirmed = true;
          state.onsetStart = null;
        }
      } else {
        state.onsetStart = null;
      }
      return false;

    case "speech":
      if (rms < config.silenceThreshold) {
        state.phase = "trailing_silence";
        state.silenceStart = now;
      }
      return false;

    case "trailing_silence":
      if (rms >= config.speechOnsetThreshold) {
        state.phase = "speech";
        state.silenceStart = null;
        return false;
      }
      if (state.silenceStart !== null && now - state.silenceStart >= config.silenceTimeoutMs) {
        return true;
      }
      return false;

    default:
      return false;
  }
}

// ── Resampling ────────────────────────────────────────────────────

/** Linear interpolation; Whisper wants 16 kHz, and browsers capture at 44.1 or 48 kHz. */
export function downsampleTo16k(buffer: Float32Array, fromRate: number): Float32Array {
  if (fromRate === 16000) return buffer;
  const ratio = fromRate / 16000;
  const newLength = Math.round(buffer.length / ratio);
  const result = new Float32Array(newLength);
  for (let i = 0; i < newLength; i++) {
    const srcIdx = i * ratio;
    const low = Math.floor(srcIdx);
    const high = Math.min(low + 1, buffer.length - 1);
    const frac = srcIdx - low;
    result[i] = buffer[low] * (1 - frac) + buffer[high] * frac;
  }
  return result;
}

// ── AudioContext Singleton ────────────────────────────────────────

let _audioContext: AudioContext | null = null;

/** Shared playback context; browsers require the first call to come from a user gesture. */
export function getAudioContext(): AudioContext {
  if (!_audioContext || _audioContext.state === "closed") {
    _audioContext = new AudioContext();
  }
  if (_audioContext.state === "suspended") {
    _audioContext.resume().catch(() => {});
  }
  return _audioContext;
}

export function closeAudioContext(): void {
  if (_audioContext && _audioContext.state !== "closed") {
    _audioContext.close().catch(() => {});
    _audioContext = null;
  }
}

// ── TTS Playback Controller ──────────────────────────────────────

// The active source and its resolve are tracked so barge-in can silence playback and unblock the queue.

let _activeTtsSource: AudioBufferSourceNode | null = null;
let _activeTtsResolve: (() => void) | null = null;
let _ttsInterrupted = false;

/** Called by playTtsSentence before playback starts. */
export function registerTtsSource(
  source: AudioBufferSourceNode,
  resolve: () => void,
): void {
  _activeTtsSource = source;
  _activeTtsResolve = resolve;
}

/** After a natural finish; interruptions go through stopTtsPlayback. */
export function clearTtsSource(): void {
  _activeTtsSource = null;
  _activeTtsResolve = null;
}

/** Stops playback, resolves the pending promise and sets the flag the queue checks before each sentence. */
export function stopTtsPlayback(): void {
  _ttsInterrupted = true;
  if (_activeTtsSource) {
    try { _activeTtsSource.stop(); } catch { /* already stopped */ }
    _activeTtsSource = null;
  }
  if (_activeTtsResolve) {
    _activeTtsResolve();
    _activeTtsResolve = null;
  }
}

/** True from stopTtsPlayback() until resetTtsInterrupt(); the queue then drains without playing. */
export function isTtsInterrupted(): boolean {
  return _ttsInterrupted;
}

/** Call when a pipeline run starts, so an earlier interruption doesn't carry over. */
export function resetTtsInterrupt(): void {
  _ttsInterrupted = false;
}

// ── Ping Tone Generator ──────────────────────────────────────────

/** Two-note ascending chime, about 200 ms. */
export function playPingTone(): void {
  const ctx = getAudioContext();
  const now = ctx.currentTime;

  const gain = ctx.createGain();
  gain.connect(ctx.destination);
  gain.gain.setValueAtTime(0.15, now);
  gain.gain.exponentialRampToValueAtTime(0.001, now + 0.2);

  // First tone: A5 (880Hz)
  const osc1 = ctx.createOscillator();
  osc1.type = "sine";
  osc1.frequency.value = 880;
  osc1.connect(gain);
  osc1.start(now);
  osc1.stop(now + 0.1);

  // Second tone: C6 (1047Hz)
  const osc2 = ctx.createOscillator();
  osc2.type = "sine";
  osc2.frequency.value = 1047;
  osc2.connect(gain);
  osc2.start(now + 0.08);
  osc2.stop(now + 0.2);
}

// ── Speech Units ──────────────────────────────────────────────────
// Port of `ReplySplitter` in `crates/pond-voice/src/text.rs`; keep the two in step.

/** The most a unit holds, in estimated Kokoro tokens: past about 200 Kokoro speaks faster. */
export const UNIT_TOKEN_BUDGET = 200;
const UNIT_CHARS = Math.floor((UNIT_TOKEN_BUDGET * 10) / 11);
const CLOSERS = `"')]”’`;

/** Text the pond's Kokoro says in one pass, and whether more of the same speech follows it. */
export interface SpeechUnit {
  text: string;
  /** Nothing follows: the pond ends the audio at rest instead of in the gap before more. */
  last: boolean;
}

/** Kokoro tokens for `text`, roughly: espeak IPA runs about 1.1 symbols per character. */
export function estimateTokens(text: string): number {
  return Math.ceil((Array.from(text).length * 11) / 10);
}

/**
 * Where a text too long for one unit ends its first: the last sentence or line end that fits,
 * failing that the last space. Gives the unit's end and where the next one starts.
 */
function fittingCut(text: string): [number, number] {
  let limit = text.length;
  let count = 0;
  for (let i = 0; i < text.length; count += 1) {
    if (count === UNIT_CHARS) { limit = i; break; }
    i += (text.codePointAt(i) ?? 0) > 0xffff ? 2 : 1;
  }
  const pastSpace = (from: number): number => from + (/^\s*/.exec(text.slice(from))?.[0].length ?? 0);
  let best: [number, number] | null = null;
  let lastSpace: [number, number] | null = null;
  for (let i = 0; i < limit; i += 1) {
    const ch = text[i];
    if (ch === "\n") {
      if (text.slice(0, i).trim()) best = [i, pastSpace(i)];
      // A full stop after a digit is a decimal or a list number.
    } else if (".?!…".includes(ch) && !(ch === "." && /[0-9]/.test(text[i - 1] ?? ""))) {
      let end = i + 1;
      while (end < text.length && ".?!".includes(text[end])) end += 1;
      while (end < text.length && CLOSERS.includes(text[end])) end += 1;
      if (end <= limit && /\s/.test(text[end] ?? "")) best = [end, pastSpace(end)];
    } else if (/\s/.test(ch) && i > 0) {
      lastSpace = [i, pastSpace(i)];
    }
  }
  return best ?? lastSpace ?? [limit, limit];
}

/**
 * Holds a reply as it streams. Kokoro shapes the pauses of whatever it is given in one pass, so
 * the text is kept whole: it is given up when the model stops writing, and before that only once
 * it is more than one pass holds.
 */
export class ReplySplitter {
  private buf = "";

  /** Add streamed text and take the units that are already full. */
  push(text: string): SpeechUnit[] {
    this.buf += text;
    return this.fullUnits();
  }

  /** The model stopped writing to use a tool: everything it has written so far. */
  pause(): SpeechUnit[] {
    return this.takeAll();
  }

  /** The stream is over: whatever is left, the last unit closing the speech. */
  finish(): SpeechUnit[] {
    const units = this.takeAll();
    if (units.length) units[units.length - 1].last = true;
    return units;
  }

  get pending(): string {
    return this.buf;
  }

  private fullUnits(): SpeechUnit[] {
    const units: SpeechUnit[] = [];
    for (;;) {
      this.buf = this.buf.replace(/^\s+/, "");
      if (estimateTokens(this.buf) <= UNIT_TOKEN_BUDGET) break;
      const [end, resume] = fittingCut(this.buf);
      const text = this.buf.slice(0, end).trim();
      this.buf = this.buf.slice(resume);
      if (text) units.push({ text, last: false });
    }
    return units;
  }

  private takeAll(): SpeechUnit[] {
    const units = this.fullUnits();
    const rest = this.buf.trim();
    this.buf = "";
    if (rest) units.push({ text: rest, last: false });
    return units;
  }
}

/** A whole text as speech units: one, unless it is more than a pass holds. */
export function splitText(text: string): SpeechUnit[] {
  const s = new ReplySplitter();
  return [...s.push(text), ...s.finish()];
}

/**
 * How long to hold `readySecs` of audio so each unit still owed, given as the seconds its
 * synthesis and its speech take, is ready when the audio before it ends. An engine slower than
 * its own speech would otherwise stop between units. Port of `hold_secs` in
 * `crates/pond-core/src/models/services/voice/speech_pipeline.rs`.
 */
export function holdSecs(readySecs: number, owed: readonly (readonly [number, number])[]): number {
  let work = 0;
  let audio = readySecs;
  let hold = 0;
  for (const [synth, speech] of owed) {
    work += synth;
    hold = Math.max(hold, work - audio);
    audio += speech;
  }
  return hold;
}

// ── Markdown Stripping (for TTS) ─────────────────────────────────

/** Port of `SCAFFOLD_TAGS` in `crates/pond-voice/src/text.rs`. */
const SCAFFOLD_TAGS = [
  "system-context",
  "user-message",
  "answer-contract",
  "memories",
  "turn-context",
  "identity",
  "instructions",
  "context-handling",
  "tool-usage",
  "tool-failure",
  "memory-rules",
  "output-quality",
  "voice-mode",
  "vision",
  "conversation-summary",
  "home-devices",
];

/** Port of `strip_markup_for_speech` in `crates/pond-voice/src/text.rs`. */
export function stripMarkupForSpeech(text: string): string {
  let out = text;
  for (const tag of SCAFFOLD_TAGS) {
    out = out.replace(new RegExp(`<${tag}>[\\s\\S]*?</${tag}>`, "g"), "");
  }
  // Models invent snake_case tags for tool syntax and turn records; no HTML tag has an underscore.
  out = out.replace(/<([a-z][a-z0-9]*(?:_[a-z0-9]+)+)>[\s\S]*?<\/\1>/g, "");
  const attr = `\\s+[\\w.:-]+(?:\\s*=\\s*(?:"[^"]*"|'[^']*'|[^\\s>/]+))?`;
  // A tool-like name may be cut off unclosed by a clause split; an HTML tag never counts so.
  out = out.replace(new RegExp(`</?[A-Za-z][\\w.:-]*__[\\w.:-]*(?:${attr})*(?:\\s*/?>|[\\s\\S]*$)`, "g"), "");
  out = out.replace(new RegExp(`</?[A-Za-z][\\w.:-]*(?:${attr})*\\s*/?>`, "g"), "");
  out = out.replace(/(?<![A-Za-z0-9])[a-z0-9-]+__[a-z0-9_]+(?:\{[^}]*\}?|\([^)]*\)?)?/g, "");
  return out;
}

/** Port of `strip_markdown_for_speech` in `crates/pond-voice/src/text.rs`. */
export function stripMarkdown(text: string): string {
  let out = stripMarkupForSpeech(text);
  // Code fences
  out = out.replace(/```[\s\S]*?```/g, "");
  out = out.replace(/~~~[\s\S]*?~~~/g, "");
  // Horizontal rules
  out = out.replace(/^[-*_]{3,}\s*$/gm, "");
  // A list line with no mark of its own would run into the next one unpaused.
  const LIST_ITEM = String.raw`[ \t]*(?:[-*+\u2022]|\d{1,3}[.)])\s`;
  const UNMARKED = String.raw`.*[^.,;:!?\u2026\u2014\s]`;
  out = out.replace(new RegExp(`^(${LIST_ITEM}${UNMARKED})[ \\t]*(?=\\n[ \\t]*\\S)`, "gm"), "$1,");
  out = out.replace(new RegExp(`^(${UNMARKED})[ \\t]*(?=\\n${LIST_ITEM})`, "gm"), "$1,");
  // Line prefixes: headings, blockquotes, list markers
  out = out.replace(/^#{1,6}\s+/gm, "");
  out = out.replace(/^>\s*/gm, "");
  out = out.replace(/^[-*+]\s+/gm, "");
  out = out.replace(/^\d+\.\s+/gm, "");
  // Images
  out = out.replace(/!\[([^\]]*)\]\([^)]*\)/g, "$1");
  // Links
  out = out.replace(/\[([^\]]*)\]\([^)]*\)/g, "$1");
  // Inline formatting: bold, italic, strikethrough, code
  out = out.replace(/\*\*([^*]+)\*\*/g, "$1");
  out = out.replace(/__([^_]+)__/g, "$1");
  out = out.replace(/\*([^*]+)\*/g, "$1");
  out = out.replace(/_([^_]+)_/g, "$1");
  out = out.replace(/~~([^~]+)~~/g, "$1");
  out = out.replace(/`([^`]+)`/g, "$1");
  // Collapse whitespace
  out = out.replace(/\n{2,}/g, ". ");
  out = out.replace(/\s{2,}/g, " ");
  return out.trim();
}

// ── Symbol Normalization (for TTS) ───────────────────────────────

const SYMBOL_RULES: [RegExp, string][] = [
  // Time
  [/(\d{1,2}):(\d{2})\s*(am|pm)/gi, "$1 $2 $3"],
  [/(\d{1,2})(am|pm)/gi, "$1 $2"],
  // Units
  [/(\d+(?:\.\d+)?)\s*kg/gi, "$1 kilograms"],
  [/(\d+(?:\.\d+)?)\s*MB\/s/gi, "$1 megabytes per second"],
  [/(\d+(?:\.\d+)?)\s*KB\/s/gi, "$1 kilobytes per second"],
  [/(\d+(?:\.\d+)?)\s*GB/gi, "$1 gigabytes"],
  [/(\d+(?:\.\d+)?)\s*MB/gi, "$1 megabytes"],
  [/(\d+(?:\.\d+)?)\s*KB/gi, "$1 kilobytes"],
  [/(\d+(?:\.\d+)?)\s*ms/gi, "$1 milliseconds"],
  [/(\d+(?:\.\d+)?)\s*hz/gi, "$1 hertz"],
  // Currency
  [/\$(\d+(?:\.\d+)?)/g, "$1 dollars"],
  [/€(\d+(?:\.\d+)?)/g, "$1 euros"],
  [/£(\d+(?:\.\d+)?)/g, "$1 pounds"],
  [/(\d+(?:\.\d+)?)\s*(?:KES|KSh|Ksh|ksh)/gi, "$1 Kenyan shillings"],
  [/(?:KES|KSh|Ksh|ksh)\.?\s*(\d+(?:\.\d+)?)/gi, "$1 Kenyan shillings"],
  [/\b(?:KES|KSH)\b/g, "Kenyan shillings"],
  // Abbreviations
  [/\be\.g\./gi, "for example"],
  [/\bi\.e\./gi, "that is"],
  [/\bDr\./g, "doctor"],
  [/\bMr\./g, "mister"],
  [/\bMrs\./g, "missus"],
  // Special characters
  [/°C/g, "degrees Celsius"],
  [/°F/g, "degrees Fahrenheit"],
  [/%/g, " percent"],
  [/\+/g, " plus "],
  [/&/g, " and "],
  [/±/g, "plus or minus"],
  [/√/g, "square root of"],
  [/π/g, "pi"],
  // Fractions
  [/¼/g, "one quarter"],
  [/½/g, "one half"],
  [/¾/g, "three quarters"],
];

/** Port of `normalize_for_speech` in `crates/pond-voice/src/text.rs`. */
export function normalizeForSpeech(text: string): string {
  let out = text;
  for (const [re, replacement] of SYMBOL_RULES) {
    out = out.replace(re, replacement);
  }
  return out;
}

// ── Dismissal Detection ──────────────────────────────────────────

const DISMISSAL_PHRASES = [
  "bye", "goodbye", "good bye", "see you", "see ya",
  "dismissed", "stop", "shut up", "that's all", "that is all",
  "go to sleep", "never mind", "nevermind",
];
const EXIT_PHRASES = ["exit", "quit"];

export function checkDismissal(text: string): { dismissed: boolean; isExit: boolean } {
  const lower = text.toLowerCase().trim();
  if (EXIT_PHRASES.some((p) => lower === p || lower.startsWith(p + " "))) {
    return { dismissed: true, isExit: true };
  }
  if (DISMISSAL_PHRASES.some((p) => lower === p || lower.startsWith(p + " "))) {
    return { dismissed: true, isExit: false };
  }
  return { dismissed: false, isExit: false };
}

// ── Tool Announcements ───────────────────────────────────────────

const TOOL_ANNOUNCEMENTS: Record<string, string> = {
  weather: "Let me check the weather.",
  save_memory: "Got it, I'll remember that.",
  recall_memories: "Let me think back.",
  recall_memory: "Let me think back.",
  forget_memory: "Noted, I'll forget that.",
  wikipedia: "Let me look that up.",
  create_schedule: "Setting that up for you.",
  list_schedules: "Let me check your schedules.",
  delete_schedule: "Removing that schedule.",
  run_schedule_now: "Running that now.",
  devices: "Checking your devices.",
};

export function getToolAnnouncement(toolName: string): string {
  return TOOL_ANNOUNCEMENTS[toolName] ?? `Working on that.`;
}

// ── Thinking Tone ────────────────────────────────────────────────

/** Unconditional: callers gate it on `voice_thinking_tone_enabled`, so only the settings owner decides. */
export function playThinkingTone(): () => void {
  const ctx = getAudioContext();
  let stopped = false;
  let osc: OscillatorNode | null = null;
  let gain: GainNode | null = null;

  function pulse() {
    if (stopped) return;
    osc = ctx.createOscillator();
    gain = ctx.createGain();
    osc.type = "sine";
    osc.frequency.value = 440;
    gain.gain.setValueAtTime(0, ctx.currentTime);
    gain.gain.linearRampToValueAtTime(0.06, ctx.currentTime + 0.15);
    gain.gain.linearRampToValueAtTime(0, ctx.currentTime + 0.85);
    osc.connect(gain);
    gain.connect(ctx.destination);
    osc.start(ctx.currentTime);
    osc.stop(ctx.currentTime + 1);
    osc.onended = () => {
      if (!stopped) setTimeout(pulse, 150);
    };
  }
  pulse();

  return () => {
    stopped = true;
    try { osc?.stop(); } catch { /* ignore */ }
    try { gain?.disconnect(); } catch { /* ignore */ }
  };
}

// ── Quip Generator ───────────────────────────────────────────────

const QUIPS = [
  "On it.",
  "Let me think.",
  "One moment.",
  "Working on that.",
  "Hmm, let me see.",
  "Give me a second.",
  "Processing.",
  "Thinking.",
];

let _quipIdx = 0;

/** The next quip, in rotation, to fill the silence during LLM inference. */
export function getQuip(): string {
  const q = QUIPS[_quipIdx % QUIPS.length];
  _quipIdx++;
  return q;
}

// ── Whisper Artefact Stripping ────────────────────────────────────

/** Regex patterns that Whisper inserts as artefacts in silent or noisy audio. */
const WHISPER_ARTIFACT_PATTERNS = [
  /^\s*\[.*?\]\s*$/,               // [BLANK_AUDIO], [silence], etc.
  /^\s*\(.*?\)\s*$/,               // (inaudible), (silence), etc.
  /^\s*♪.*$/,                      // Music notation
  /^\s*\.+\s*$/,                   // Just dots
];

/** Exact-match Whisper hallucinations, lowercase. */
const WHISPER_HALLUCINATIONS = new Set([
  "thank you", "thank you.", "thanks for watching", "thanks for watching.",
  "bye", "bye.", "okay", "okay.", "so", "um", "uh", "you",
  "the end", "the end.", "thanks",
]);

/** Known Whisper artefacts, plus anything of 2 chars or less and one word repeated ("uh uh uh"). */
export function isWhisperArtifact(text: string): boolean {
  const trimmed = text.trim();
  if (!trimmed || trimmed.length <= 2) return true;
  if (WHISPER_ARTIFACT_PATTERNS.some((re) => re.test(trimmed))) return true;
  if (WHISPER_HALLUCINATIONS.has(trimmed.toLowerCase())) return true;
  const words = trimmed.toLowerCase().split(/\s+/);
  if (words.length >= 2 && words.every((w) => w === words[0])) return true;
  return false;
}

// ── Enhanced Thought Filter ──────────────────────────────────

const THINK_OPEN = [/<think>/i, /<thought>/i, /<\|channel>thought/i, /<\|tool_call>/i];
const THINK_CLOSE = [/<\/think>/i, /<\/thought>/i, /<channel\|>/i, /<\/tool_call>/i, /<tool_call\|>/i];
const ORPHAN_SENTINELS = /<eos>|<\|eos\|>|<end_of_turn>/gi;

/** Strips thought and tool-call blocks and orphan sentinels from a token stream; returns [visible, inBlock]. */
export function filterThinkingFull(token: string, inBlock: boolean): [string, boolean] {
  const text = token.replace(ORPHAN_SENTINELS, "");
  let inside = inBlock;
  let result = "";
  let i = 0;

  while (i < text.length) {
    if (!inside) {
      let matched = false;
      for (const re of THINK_OPEN) {
        const sub = text.slice(i);
        const m = sub.match(re);
        if (m && m.index === 0) {
          inside = true;
          i += m[0].length;
          matched = true;
          break;
        }
      }
      if (!matched) { result += text[i]; i++; }
    } else {
      let matched = false;
      for (const re of THINK_CLOSE) {
        const sub = text.slice(i);
        const m = sub.match(re);
        if (m && m.index === 0) {
          inside = false;
          i += m[0].length;
          matched = true;
          break;
        }
      }
      if (!matched) i++;
    }
  }

  return [result, inside];
}
