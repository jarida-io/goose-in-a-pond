// Classifying the voice child's stdout.
//
// `pond-server chat --voice --json-events` writes one JSON object per line.
// This module turns a line into either a shell event, an exit notice, or
// nothing -- and decides, when the child's stdout finally closes, what to call
// the way it ended.
//
// It deliberately imports nothing from Electron and touches no process state,
// because this is the part worth testing exhaustively and a test that needs an
// Electron app object is a test nobody runs. Everything here is a pure
// function over a string.

import type { ShellEvent, ShellEvents } from "../../../src/shell/contract";

/** What a single line of the child's stdout turned out to be. */
export type LineClass =
  /** A line that maps onto a shell event, ready to forward to the renderer. */
  | { kind: "event"; name: ShellEvent; payload: ShellEvents[ShellEvent] }
  /**
   * The child announced it is exiting. This is NOT forwarded as an event --
   * the reason is held until stdout actually closes and then reported once,
   * as `voice-session-ended`.
   */
  | { kind: "exit"; reason: string }
  /** Blank line. Nothing to do. */
  | { kind: "skip" };

/**
 * A line either classifies or it does not. Returned rather than thrown: a
 * malformed line is an ordinary event on this stream (the child can print a
 * banner, a panic, a partial write) and must never take the session down. The
 * caller logs `error` and reads the next line.
 */
export type Classified =
  { ok: true; value: LineClass } | { ok: false; error: string };

/**
 * How many trailing stderr lines to keep for a startup failure's `detail`.
 *
 * The child's stderr is a full tracing stream and only the tail matters -- the
 * fatal line is almost always last. Bounded so a long session cannot grow this
 * without limit.
 */
export const STDERR_TAIL_LINES = 20;

/** The default reason for an `exit` line that does not carry one. */
const DEFAULT_EXIT_REASON = "stdin_eof";

/** The child died after it had announced `ready`. */
const CRASHED = "crashed";

/**
 * The child died before `ready`, so no session ever existed. Distinct from
 * `crashed` because the causes differ in kind -- a binary that cannot run
 * against this machine's state: a stale sidecar, a failed migration, a missing
 * dylib -- and because the child's explanation is on stderr rather than in any
 * NDJSON line. This is the only end reason whose payload carries `detail`.
 */
const FAILED_TO_START = "failed_to_start";

/**
 * Read a string field, defaulting to "" when absent or not a string.
 *
 * Matching the child's own tolerance: a missing field yields an empty payload
 * field rather than rejecting the whole line, because half an event is still
 * worth showing and the renderer already renders empty text as nothing.
 */
function str(obj: Record<string, unknown>, key: string): string {
  const v = obj[key];
  return typeof v === "string" ? v : "";
}

/** Classify one line of the child's stdout. */
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
    // Prefix warm-up progress at session start: `warming` while the model
    // loads and the prompt prefix prefills, then ready/skipped/failed. The
    // child also speaks these transitions; this event lets the UI label the
    // stretch where the mic is not yet listening.
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
    // Distinct from the unrelated legacy `audio-level` event: this one belongs
    // exclusively to the voice-* family that useVoiceSession owns.
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
 * Decide the end reason for a child whose stdout closed, and what to say about
 * it.
 *
 * `cleanReason` is the reason from the child's own `exit` line, if it sent
 * one; its presence IS the definition of a clean shutdown. `sawReady`
 * distinguishes a session that ran and then died from one that never started.
 *
 * `detail` is populated only for a startup failure, and only from stderr:
 * after `ready` the child reports its own troubles as NDJSON `error` events,
 * so a stderr dump there would be noise duplicating a better signal.
 */
export function classifyEnd(
  cleanReason: string | null,
  sawReady: boolean,
  stderrTail: readonly string[],
): { reason: string; detail: string | null } {
  if (cleanReason !== null) return { reason: cleanReason, detail: null };
  if (sawReady) return { reason: CRASHED, detail: null };

  const joined = stderrTail.join("\n");
  // An all-blank tail must report absence, not "". The renderer branches on
  // absence to choose its "without reporting a reason" wording, and an empty
  // string would render as a message with nothing after it.
  return {
    reason: FAILED_TO_START,
    detail: joined.trim() === "" ? null : joined,
  };
}

/**
 * The session id to hand the child.
 *
 * Blank and whitespace-only are treated as absent rather than passed through:
 * the renderer has more than one way to spell "no session yet" (`null`, and a
 * field initialised to `""`), and a blank `--session-id` reaching the child
 * would name a session nothing can ever look up. Getting this wrong gave every
 * voice session an empty history, so the assistant could not refer to anything
 * said in the chat view a moment earlier -- which from the room reads as
 * unreliability, not as a bug.
 */
export function sessionToJoin(
  resume: string | null | undefined,
  newId: () => string,
): string {
  if (typeof resume === "string" && resume.trim() !== "") return resume;
  return newId();
}
