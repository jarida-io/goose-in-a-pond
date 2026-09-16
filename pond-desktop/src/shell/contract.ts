// The desktop shell's IPC contract — the one file both sides of the bridge
// import, so the renderer, the preload and the main process cannot disagree
// about a name or a payload shape.
//
// This replaces Tauri's `invoke(cmd: string, ...)` / `listen(event: string, ...)`,
// whose signatures took a bare `string` and so could not tell a typo from a
// command. Here both are closed unions: `invoke("stop_voice_sesion")` is a
// compile error, not a runtime rejection.
//
// Two things are deliberately co-located rather than split:
//
//   * The runtime allowlists (`SHELL_COMMANDS`, `SHELL_EVENTS`) live beside the
//     types they mirror. The preload validates against them so a compromised
//     renderer cannot reach an arbitrary `ipcMain` channel, and `satisfies`
//     makes a typo in either array a compile error. `contract.test.ts` closes
//     the remaining gap — `satisfies` catches a misspelling but not an omission.
//
//   * `VoiceEndReason` is a closed set here even though the shell used to send
//     it as a bare string. It comes from the `end_reason` constants the voice
//     child driver classifies with, and making it a union is what lets the
//     renderer's reason handling be exhaustiveness-checked.

/**
 * How a voice session ended.
 *
 * These five are the documented contract -- three the child sends on its own
 * `exit` line, two the shell synthesises -- but the set is OPEN, not closed:
 * the child formats this field from a variable, so an unrecognised reason can
 * reach the renderer. `reason` is therefore typed as this union widened with
 * `string`, which keeps the literals as autocomplete while still admitting an
 * unknown one. The renderer is already tolerant of that (`isCleanExit`
 * defaults to abnormal, `endedErrorMessage` interpolates the raw string), and
 * typing it closed would claim a guarantee the wire does not make.
 */
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
  /**
   * Spawn pond-server if this shell owns it, or attach to a parent-managed one.
   * Resolves to the base URL that was settled on.
   */
  ensure_server_running: { args: void; result: string };
  /**
   * Start the voice child. `sessionId` is the chat session to resume, or null
   * for a fresh one. Resolves to the session id the child was given.
   */
  start_voice_session: { args: { sessionId: string | null }; result: string };
  /** Stop the voice child and release the microphone. */
  stop_voice_session: { args: void; result: void };
  /**
   * Open a URL in the user's real browser. Under Tauri this was
   * `@tauri-apps/plugin-shell`'s `open()`; the `window.open` fallback it used
   * is actively wrong in a desktop shell, where it opens an in-app window.
   */
  open_external: { args: { url: string }; result: void };
}

export type ShellCommand = keyof ShellCommands;

/** Events the shell pushes to the renderer, with their payload types. */
export interface ShellEvents {
  /** pond-server's reachability changed. */
  "server-status": boolean;
  /** The shell has begun starting pond-server. */
  "server-starting": void;
  /**
   * pond-server bound a port other than the one the shell assumed.
   *
   * `--port` is a start port for the server's bind_with_fallback, so a server
   * that finds 4000 taken binds 4001 and says nothing. Without this the
   * renderer keeps talking to a port nothing is listening on.
   */
  "server-url": string;
  /** The summon hotkey or tray item was used. */
  "desktop-summon": void;
  /** Toggle the Canvas section. */
  "canvas-toggle": void;
  /** Switch the app into voice mode. */
  "switch-to-voice": void;

  // The voice family. Every one of these is a 1:1 mapping of a line the voice
  // child wrote to stdout as NDJSON, except `voice-session-ended`, which the
  // shell synthesises when the child's stdout closes.
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

/**
 * Runtime allowlist for commands. The preload refuses anything not in here, so
 * the bridge exposes exactly this surface and not "whatever channel main
 * happens to have registered".
 */
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
