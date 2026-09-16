import { describe, it, expect } from "vitest";
import {
  classifyLine,
  classifyEnd,
  sessionToJoin,
  STDERR_TAIL_LINES,
  type LineClass,
} from "./ndjson";

// Ported from the 21 Rust tests that covered the same logic in
// src-tauri/src/chat_process.rs. Those never gated a merge -- src-tauri was in
// no CI job at all -- so this is the first time this behaviour is actually
// checked by anything.

/** Unwrap a line that must have classified as an event. */
function event(line: string): Extract<LineClass, { kind: "event" }> {
  const r = classifyLine(line);
  expect(r.ok, `expected a clean classification for: ${line}`).toBe(true);
  if (!r.ok) throw new Error("unreachable");
  expect(r.value.kind).toBe("event");
  return r.value as Extract<LineClass, { kind: "event" }>;
}

/** Unwrap a line that must have failed to classify. */
function failure(line: string): string {
  const r = classifyLine(line);
  expect(r.ok, `expected a rejection for: ${line}`).toBe(false);
  if (r.ok) throw new Error("unreachable");
  return r.error;
}

describe("classifyLine: the NDJSON to shell-event mapping", () => {
  it("maps warmup to voice-warmup with the raw state string", () => {
    const ev = event('{"event":"warmup","state":"warming"}');
    expect(ev.name).toBe("voice-warmup");
    expect(ev.payload).toBe("warming");
  });

  it("maps ready to voice-ready", () => {
    const ev = event('{"event":"ready","session_id":"abc-123"}');
    expect(ev.name).toBe("voice-ready");
    expect(ev.payload).toEqual({ session_id: "abc-123" });
  });

  it("gives voice-state a raw string payload, not an object", () => {
    for (const s of ["wait", "listen", "thinking", "speak"]) {
      const ev = event(`{"event":"state","state":"${s}"}`);
      expect(ev.name).toBe("voice-state");
      expect(ev.payload).toBe(s);
    }
  });

  it("maps transcript to voice-transcript", () => {
    const ev = event('{"event":"transcript","text":"turn on the lights"}');
    expect(ev.name).toBe("voice-transcript");
    expect(ev.payload).toEqual({ text: "turn on the lights" });
  });

  it("maps token to voice-token", () => {
    const ev = event('{"event":"token","content":"Sure"}');
    expect(ev.name).toBe("voice-token");
    expect(ev.payload).toEqual({ content: "Sure" });
  });

  it("maps tool_call to voice-tool-call", () => {
    const ev = event('{"event":"tool_call","tool":"giap__lights","id":"t1"}');
    expect(ev.name).toBe("voice-tool-call");
    expect(ev.payload).toEqual({ tool: "giap__lights", id: "t1" });
  });

  it("maps tool_result to voice-tool-result", () => {
    const ev = event(
      '{"event":"tool_result","tool":"giap__lights","id":"t1","content":"ok, done"}',
    );
    expect(ev.name).toBe("voice-tool-result");
    expect(ev.payload).toEqual({
      tool: "giap__lights",
      id: "t1",
      content: "ok, done",
    });
  });

  it("maps turn_complete to voice-done", () => {
    const ev = event('{"event":"turn_complete","session_id":"abc-123"}');
    expect(ev.name).toBe("voice-done");
    expect(ev.payload).toEqual({ session_id: "abc-123" });
  });

  it("maps error to voice-error", () => {
    const ev = event('{"event":"error","message":"mic busy"}');
    expect(ev.name).toBe("voice-error");
    expect(ev.payload).toEqual({ message: "mic busy" });
  });

  it("maps audio_level to voice-audio-level", () => {
    const ev = event('{"event":"audio_level","rms":0.42}');
    expect(ev.name).toBe("voice-audio-level");
    expect(ev.payload).toEqual({ rms: 0.42 });
  });

  // A missing field yields an empty payload field rather than rejecting the
  // line: half an event is still worth showing, and the renderer already
  // renders empty text as nothing.
  it("defaults missing string fields to empty rather than rejecting", () => {
    expect(event('{"event":"transcript"}').payload).toEqual({ text: "" });
    expect(event('{"event":"ready"}').payload).toEqual({ session_id: "" });
    expect(event('{"event":"state"}').payload).toBe("");
  });

  it("defaults a missing or non-numeric rms to zero", () => {
    expect(event('{"event":"audio_level"}').payload).toEqual({ rms: 0 });
    expect(event('{"event":"audio_level","rms":"loud"}').payload).toEqual({
      rms: 0,
    });
  });
});

describe("classifyLine: exit lines", () => {
  // The reader turns `exit` into voice-session-ended only once stdout closes,
  // so the reason is carried here rather than emitted.
  it("classifies exit as exit, never as an event", () => {
    for (const reason of ["stdin_eof", "dismissed", "error"]) {
      const r = classifyLine(`{"event":"exit","reason":"${reason}"}`);
      expect(r).toEqual({ ok: true, value: { kind: "exit", reason } });
    }
  });

  it("defaults an exit line with no reason to stdin_eof", () => {
    expect(classifyLine('{"event":"exit"}')).toEqual({
      ok: true,
      value: { kind: "exit", reason: "stdin_eof" },
    });
  });

  // The child formats this field from a variable, so an unrecognised reason is
  // possible and must pass through rather than being coerced.
  it("passes an unrecognised reason through unchanged", () => {
    const r = classifyLine('{"event":"exit","reason":"something_new"}');
    expect(r).toEqual({
      ok: true,
      value: { kind: "exit", reason: "something_new" },
    });
  });
});

describe("classifyLine: bad input is an error, never a throw", () => {
  it("skips blank lines", () => {
    expect(classifyLine("")).toEqual({ ok: true, value: { kind: "skip" } });
    expect(classifyLine("   ")).toEqual({ ok: true, value: { kind: "skip" } });
  });

  it("rejects a non-JSON line", () => {
    expect(failure("this is a banner line, not JSON")).toMatch(
      /not valid JSON/,
    );
  });

  it("rejects JSON with no event field", () => {
    expect(failure('{"state":"wait"}')).toMatch(/missing string `event`/);
  });

  it("rejects JSON whose event field is not a string", () => {
    expect(failure('{"event":42}')).toMatch(/missing string `event`/);
  });

  it("rejects a JSON value that is not an object", () => {
    expect(failure("[1,2,3]")).toMatch(/missing string `event`/);
    expect(failure('"just a string"')).toMatch(/missing string `event`/);
  });

  it("names an unknown event kind so the log says which", () => {
    expect(failure('{"event":"telemetry"}')).toMatch(
      /unknown event kind `telemetry`/,
    );
  });
});

describe("classifyEnd", () => {
  // The bug these cover: a sidecar staged weeks earlier was rejected by a
  // newer database, exited 1, and emitted zero NDJSON lines. The shell said
  // "crashed" with an exit code and dropped the one line that explained it,
  // because the child's banner macro is a no-op under --json-events.
  it("calls a child that never readied failed_to_start, carrying its stderr", () => {
    const { reason, detail } = classifyEnd(null, false, [
      "  Goose in a Pond 0.1.0 - voice",
      "Error: migration 29 was previously applied but is missing in the resolved migrations",
    ]);
    expect(reason).toBe("failed_to_start");
    expect(detail).toContain("migration 29");
  });

  it("calls a readied session that dies crashed, and attaches no stderr", () => {
    // After ready the child reports troubles as NDJSON error events, so stderr
    // here would duplicate a better signal with noise.
    expect(classifyEnd(null, true, ["some later log line"])).toEqual({
      reason: "crashed",
      detail: null,
    });
  });

  it("lets a clean exit line win over both", () => {
    // Sending exit IS the definition of a clean shutdown; a child that says so
    // before ever readying still exited cleanly, not fatally.
    expect(classifyEnd("stdin_eof", false, ["noise"])).toEqual({
      reason: "stdin_eof",
      detail: null,
    });
  });

  it("reports no detail rather than an empty string for a silent failure", () => {
    // The renderer branches on absence to choose its "without reporting a
    // reason" wording; "" would render as a message with nothing after it.
    expect(classifyEnd(null, false, ["   ", ""])).toEqual({
      reason: "failed_to_start",
      detail: null,
    });
    expect(classifyEnd(null, false, [])).toEqual({
      reason: "failed_to_start",
      detail: null,
    });
  });

  it("keeps the stderr tail bound at twenty lines", () => {
    expect(STDERR_TAIL_LINES).toBe(20);
  });
});

describe("sessionToJoin", () => {
  const FRESH = "generated-uuid";
  const newId = () => FRESH;

  it("resumes a real session id", () => {
    expect(sessionToJoin("abc-123", newId)).toBe("abc-123");
  });

  // The renderer has more than one way to spell "no session yet", and a blank
  // --session-id reaching the child names a session nothing can look up.
  it("treats every blank spelling as absent", () => {
    expect(sessionToJoin(null, newId)).toBe(FRESH);
    expect(sessionToJoin(undefined, newId)).toBe(FRESH);
    expect(sessionToJoin("", newId)).toBe(FRESH);
    expect(sessionToJoin("   ", newId)).toBe(FRESH);
    expect(sessionToJoin("\t\n", newId)).toBe(FRESH);
  });
});
