// Classifies the voice child's NDJSON stdout and its end reason. Pure and Electron-free on
// purpose, so it can be tested exhaustively.

import type { ShellEvent, ShellEvents } from "../../../src/shell/contract";

/** What a single line of the child's stdout turned out to be. */
export type LineClass =
  /** A line that maps onto a shell event, ready to forward to the renderer. */
  | { kind: "event"; name: ShellEvent; payload: ShellEvents[ShellEvent] }
  /** The child is exiting; held until stdout closes, then reported once as `voice-session-ended`. */
  | { kind: "exit"; reason: string }
  /** Blank line. Nothing to do. */
  | { kind: "skip" };

/** Returned, not thrown: malformed lines (banner, panic, partial write) are normal here. */
export type Classified =
  { ok: true; value: LineClass } | { ok: false; error: string };

/** Stderr lines kept for a startup failure's `detail`; the fatal line is almost always last. */
export const STDERR_TAIL_LINES = 20;

/** The default reason for an `exit` line that does not carry one. */
const DEFAULT_EXIT_REASON = "stdin_eof";

/** The child died after it had announced `ready`. */
const CRASHED = "crashed";

/** Died before `ready` (e.g. stale sidecar); the only reason with `detail`, taken from stderr. */
const FAILED_TO_START = "failed_to_start";

/** String field or "", like the child itself: half an event is still worth showing. */
function str(obj: Record<string, unknown>, key: string): string {
  const v = obj[key];
  return typeof v === "string" ? v : "";
}

export function classifyLine(line: string): Classified {
  const trimmed = line.trim();
  if (trimmed === "") return { ok: true, value: { kind: "skip" } };

  let parsed: unknown;
  try {
    parsed = JSON.parse(trimmed);
  } catch (e) {
    return { ok: false, error: `not valid JSON: ${(e as Error).message}` };
  }
  if (typeof parsed !== "object" || parsed === null || Array.isArray(parsed)) {
    return { ok: false, error: "missing string `event` field" };
  }

  const obj = parsed as Record<string, unknown>;
  const event = obj["event"];
  if (typeof event !== "string") {
    return { ok: false, error: "missing string `event` field" };
  }

  const ev = (name: ShellEvent, payload: unknown): Classified => ({
    ok: true,
    value: {
      kind: "event",
      name,
      payload: payload as ShellEvents[ShellEvent],
    },
  });

  switch (event) {
    // Prefix warm-up (warming, then ready/skipped/failed): the stretch before the mic listens.
    case "warmup":
      return ev("voice-warmup", str(obj, "state"));
    case "ready":
      return ev("voice-ready", { session_id: str(obj, "session_id") });
    // Contract: the payload is the raw state string, not an object.
    case "state":
      return ev("voice-state", str(obj, "state"));
    case "transcript":
      return ev("voice-transcript", { text: str(obj, "text") });
    case "token":
      return ev("voice-token", { content: str(obj, "content") });
    case "tool_call":
      return ev("voice-tool-call", {
        tool: str(obj, "tool"),
        id: str(obj, "id"),
      });
    case "tool_result":
      return ev("voice-tool-result", {
        tool: str(obj, "tool"),
        id: str(obj, "id"),
        content: str(obj, "content"),
      });
    case "turn_complete":
      return ev("voice-done", { session_id: str(obj, "session_id") });
    case "error":
      return ev("voice-error", { message: str(obj, "message") });
    // Not the unrelated legacy `audio-level` event; this one belongs to useVoiceSession.
    case "audio_level": {
      const rms = obj["rms"];
      return ev("voice-audio-level", {
        rms: typeof rms === "number" ? rms : 0,
      });
    }
    case "exit": {
      const reason = obj["reason"];
      return {
        ok: true,
        value: {
          kind: "exit",
          reason: typeof reason === "string" ? reason : DEFAULT_EXIT_REASON,
        },
      };
    }
    default:
      return { ok: false, error: `unknown event kind \`${event}\`` };
  }
}

/**
 * End reason once stdout closes: a child `exit` line means clean, else `sawReady` splits crashed
 * from failed_to_start. Only the latter gets `detail`: after ready, errors arrive as NDJSON.
 */
export function classifyEnd(
  cleanReason: string | null,
  sawReady: boolean,
  stderrTail: readonly string[],
): { reason: string; detail: string | null } {
  if (cleanReason !== null) return { reason: cleanReason, detail: null };
  if (sawReady) return { reason: CRASHED, detail: null };

  const joined = stderrTail.join("\n");
  // An all-blank tail reports null, not "": the renderer branches on absence.
  return {
    reason: FAILED_TO_START,
    detail: joined.trim() === "" ? null : joined,
  };
}

/** `resume` unless blank (the renderer also spells "no session" as ""), else a fresh id. */
export function sessionToJoin(
  resume: string | null | undefined,
  newId: () => string,
): string {
  if (typeof resume === "string" && resume.trim() !== "") return resume;
  return newId();
}
