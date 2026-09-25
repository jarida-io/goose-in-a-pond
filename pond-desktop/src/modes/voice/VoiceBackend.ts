// Voice I/O for the browser pipeline; WebVoiceBackend is its only implementation.

export type VoiceState = "idle" | "wait" | "recording" | "thinking" | "speaking" | "error";

export interface PipelineOpts {
  stripWakeWord?: string;
  sessionId?: string;
  authToken?: string;
  serverUrl: string;
  /** `settings.voice_thinking_tone_enabled`. Omitted means on: a turn can fire before settings load. */
  thinkingTone?: boolean;
}

export interface ResponseMeta {
  modelName: string;
  modelRole: string;
  completionTokens: number;
}

export interface ToolCallData {
  tool: string;
  data: Record<string, unknown>;
}

/** The orchestration hook sets the callbacks before calling any action method. */
export interface VoiceBackend {
  // ── Recording ──────────────────────────────────────────
  /** `authToken`/`sessionId` allow a speculative LLM request while silence is being confirmed. */
  recordWithVad(authToken?: string, sessionId?: string): Promise<Blob | null>;
  /** Cancel in-progress recording without sending. */
  abortRecording(): void;

  // ── Pipeline ───────────────────────────────────────────
  /** Full pipeline: transcribe → chat stream → sentence TTS. */
  runPipeline(wav: Blob, opts: PipelineOpts): Promise<void>;
  /** Barge-in: abort pipeline, stop TTS, stop thinking tone. */
  cancelPipeline(): void;

  // ── Wake word ──────────────────────────────────────────
  startWakeListener(word: string, variants: string[]): void;
  stopWakeListener(): void;

  // ── Audio feedback ─────────────────────────────────────
  playPing(): void;

  // ── Lifecycle ──────────────────────────────────────────
  destroy(): void;

  // ── Event callbacks (set by orchestration hook) ────────
  onAudioLevel: ((level: number) => void) | null;
  onStateChange: ((state: VoiceState) => void) | null;
  onTranscript: ((text: string) => void) | null;
  onAgentToken: ((token: string, done: boolean) => void) | null;
  onToolCall: ((data: ToolCallData) => void) | null;
  onError: ((msg: string) => void) | null;
  onSessionId: ((id: string) => void) | null;
  onResponseMeta: ((meta: ResponseMeta) => void) | null;
  onWakeDetected: ((wav: Blob) => void) | null;
  onWakeInterrupt: (() => void) | null;
  onDismissed: ((isExit: boolean) => void) | null;
}

// ── Factory ──────────────────────────────────────────────────

/**
 * Always the browser backend: the desktop shell runs voice through `useVoiceSession` instead.
 * Lazy-imported because it pulls in the whole Web Audio pipeline, which the desktop build never runs.
 */
export async function createVoiceBackend(_serverUrl: string): Promise<VoiceBackend> {
  const { WebVoiceBackend } = await import("./WebVoiceBackend");
  return new WebVoiceBackend(_serverUrl);
}
