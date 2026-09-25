// Desktop-shell IPC contract, shared by renderer, preload and main. The preload enforces the
// allowlists below, so a compromised renderer cannot reach an arbitrary `ipcMain` channel.

/** How a voice session ended. Open set: the child formats it from a variable, so `reason`
 *  widens this union with `string` and the renderer must tolerate unknown values. */
export type VoiceEndReason =
  /** Stdin closed, which is the clean shutdown handshake. */
  | "stdin_eof"
  /** The child chose to end the session itself. */
  | "dismissed"
  /** The child reported a fatal error before exiting. */
  | "error"
  /** The child died after it had announced `ready`. */
  | "crashed"
  /** The child never reached `ready`; `detail` carries its stderr tail. */
  | "failed_to_start";

/** Commands the renderer may invoke on the desktop shell. */
export interface ShellCommands {
  /** Is pond-server answering /api/v1/health right now? */
  server_health: { args: void; result: boolean };
  /** Spawn pond-server if this shell owns it, else attach; resolves to the settled base URL. */
  ensure_server_running: { args: void; result: string };
  /** Start the voice child; `sessionId` resumes a chat (null = fresh). Resolves to the child's session id. */
  start_voice_session: { args: { sessionId: string | null }; result: string };
  /** Stop the voice child and release the microphone. */
  stop_voice_session: { args: void; result: void };
  /** Open a URL in the real browser (`window.open` in the shell opens an in-app window). */
  open_external: { args: { url: string }; result: void };
}

export type ShellCommand = keyof ShellCommands;

/** Events the shell pushes to the renderer, with their payload types. */
export interface ShellEvents {
  /** pond-server's reachability changed. */
  "server-status": boolean;
  /** The shell has begun starting pond-server. */
  "server-starting": void;
  /** pond-server bound another port: `--port` is only a start point for bind_with_fallback. */
  "server-url": string;
  /** The summon hotkey or tray item was used. */
  "desktop-summon": void;
  "canvas-toggle": void;
  "switch-to-voice": void;

  // Voice family: 1:1 with the child's stdout NDJSON lines, except the shell-made `voice-session-ended`.
  "voice-warmup": string;
  "voice-ready": { session_id: string };
  "voice-state": string;
  "voice-transcript": { text: string };
  "voice-token": { content: string };
  "voice-tool-call": { tool: string; id: string };
  "voice-tool-result": { tool: string; id: string; content: string };
  "voice-done": { session_id?: string };
  "voice-error": { message?: string } | string;
  "voice-audio-level": { rms: number };
  "voice-session-ended": {
    code: number | null;
    // Widened deliberately -- see the note on VoiceEndReason.
    reason: VoiceEndReason | (string & {});
    session_id?: string;
    detail?: string | null;
  };
}

export type ShellEvent = keyof ShellEvents;

/** Runtime command allowlist; the preload refuses anything else. */
export const SHELL_COMMANDS = [
  "server_health",
  "ensure_server_running",
  "start_voice_session",
  "stop_voice_session",
  "open_external",
] as const satisfies readonly ShellCommand[];

/** Runtime allowlist for events. Same reasoning as `SHELL_COMMANDS`. */
export const SHELL_EVENTS = [
  "server-status",
  "server-starting",
  "server-url",
  "desktop-summon",
  "canvas-toggle",
  "switch-to-voice",
  "voice-warmup",
  "voice-ready",
  "voice-state",
  "voice-transcript",
  "voice-token",
  "voice-tool-call",
  "voice-tool-result",
  "voice-done",
  "voice-error",
  "voice-audio-level",
  "voice-session-ended",
] as const satisfies readonly ShellEvent[];
