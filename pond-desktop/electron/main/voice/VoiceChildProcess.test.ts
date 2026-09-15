import { describe, it, expect, vi } from "vitest";
import { EventEmitter } from "node:events";
import { PassThrough, Writable } from "node:stream";
import type { ChildProcess } from "node:child_process";
import { VoiceChildProcess, type VoiceChildDeps } from "./VoiceChildProcess";
import { STDERR_TAIL_LINES } from "./ndjson";

// A fake child, which is the whole reason this port is testable where the Rust
// was not. `chat_process.rs` reached straight for `Command::new`, so the two
// behaviours that mattered most -- the stale-reader guard and the stderr tail
// reaching the renderer -- had no test at all.

class FakeChild extends EventEmitter {
  stdout = new PassThrough();
  stderr = new PassThrough();
  stdinEnded = false;
  killed: NodeJS.Signals | null = null;
  pid = 4242;

  stdin = new Writable({
    write: (_c, _e, cb) => cb(),
    final: (cb) => {
      this.stdinEnded = true;
      cb();
    },
  });

  kill(signal?: NodeJS.Signals) {
    this.killed = signal ?? "SIGTERM";
    return true;
  }

  /** Write one NDJSON line to stdout. */
  say(line: string) {
    this.stdout.write(line + "\n");
  }

  logErr(line: string) {
    this.stderr.write(line + "\n");
  }

  /** Close stdio and then the process, in the order Node really uses. */
  async exit(code: number | null) {
    this.stdout.end();
    this.stderr.end();
    await tick();
    this.emit("close", code);
    await tick();
  }
}

/** Let the event loop drain readline's queued 'line' and 'close' events. */
function tick(times = 4): Promise<void> {
  let p = Promise.resolve();
  for (let i = 0; i < times; i++)
    p = p.then(() => new Promise((r) => setImmediate(r)));
  return p;
}

interface Harness {
  voice: VoiceChildProcess;
  emitted: Array<{ name: string; payload: unknown }>;
  children: FakeChild[];
  spawnArgs: Array<{ bin: string; args: string[] }>;
  removePid: ReturnType<typeof vi.fn>;
}

function harness(over: Partial<VoiceChildDeps> = {}): Harness {
  const emitted: Array<{ name: string; payload: unknown }> = [];
  const children: FakeChild[] = [];
  const spawnArgs: Array<{ bin: string; args: string[] }> = [];
  const removePid = vi.fn();

  const deps: VoiceChildDeps = {
    resolveBinary: () => "/opt/app/pond-server",
    emit: (name, payload) => emitted.push({ name, payload }),
    spawn: ((bin: string, args: string[]) => {
      spawnArgs.push({ bin, args });
      const c = new FakeChild();
      children.push(c);
      return c as unknown as ChildProcess;
    }) as unknown as VoiceChildDeps["spawn"],
    newSessionId: () => "fresh-uuid",
    writePid: vi.fn(),
    removePid,
    gracefulStopMs: 60,
    pollMs: 10,
    orphanDeps: {
      isAlive: () => false,
      commandLine: () => null,
      kill: () => {},
      readFile: () => null,
      removeFile: () => {},
      warn: () => {},
    },
    ...over,
  };

  return {
    voice: new VoiceChildProcess(deps),
    emitted,
    children,
    spawnArgs,
    removePid,
  };
}

describe("spawning", () => {
  it("invokes the child with the contract's exact argv", async () => {
    const h = harness();
    const id = await h.voice.start("abc-123");
    expect(id).toBe("abc-123");
    expect(h.spawnArgs[0]).toEqual({
      bin: "/opt/app/pond-server",
      args: ["chat", "--voice", "--json-events", "--session-id", "abc-123"],
    });
  });

  it("generates a session id when asked to resume a blank one", async () => {
    const h = harness();
    expect(await h.voice.start("   ")).toBe("fresh-uuid");
    expect(h.spawnArgs[0]?.args.at(-1)).toBe("fresh-uuid");
  });

  it("refuses to start a second session while one is running", async () => {
    const h = harness();
    await h.voice.start(null);
    await expect(h.voice.start(null)).rejects.toThrow(/already running/);
    expect(h.children).toHaveLength(1);
  });

  it("reports a missing binary rather than spawning", async () => {
    const h = harness({ resolveBinary: () => null });
    await expect(h.voice.start(null)).rejects.toThrow(/No pond-server binary/);
    expect(h.voice.isActive).toBe(false);
  });
});

describe("forwarding the child's output", () => {
  it("maps every NDJSON line onto its shell event", async () => {
    const h = harness();
    await h.voice.start("s1");
    const c = h.children[0]!;

    c.say('{"event":"warmup","state":"warming"}');
    c.say('{"event":"ready","session_id":"s1"}');
    c.say('{"event":"state","state":"listen"}');
    c.say('{"event":"transcript","text":"hello"}');
    c.say('{"event":"token","content":"Hi"}');
    await tick();

    expect(h.emitted.map((e) => e.name)).toEqual([
      "voice-warmup",
      "voice-ready",
      "voice-state",
      "voice-transcript",
      "voice-token",
    ]);
    expect(h.emitted[3]?.payload).toEqual({ text: "hello" });
  });

  it("does not forward a non-contract line, and keeps reading", async () => {
    const h = harness();
    await h.voice.start("s1");
    const c = h.children[0]!;

    c.say("Goose in a Pond 0.1.0 - voice");
    c.say('{"event":"token","content":"still here"}');
    await tick();

    expect(h.emitted).toHaveLength(1);
    expect(h.emitted[0]?.payload).toEqual({ content: "still here" });
  });

  it("holds the exit line back and reports it once, on close", async () => {
    const h = harness();
    await h.voice.start("s1");
    const c = h.children[0]!;

    c.say('{"event":"ready","session_id":"s1"}');
    c.say('{"event":"exit","reason":"dismissed"}');
    await tick();
    // Not emitted as its own event.
    expect(h.emitted.map((e) => e.name)).toEqual(["voice-ready"]);

    await c.exit(0);
    const ended = h.emitted.at(-1)!;
    expect(ended.name).toBe("voice-session-ended");
    expect(ended.payload).toEqual({
      code: 0,
      reason: "dismissed",
      session_id: "s1",
      detail: null,
    });
    expect(h.voice.isActive).toBe(false);
    expect(h.removePid).toHaveBeenCalled();
  });
});

describe("the stderr tail reaching the renderer", () => {
  // The bug this covers end to end: a sidecar staged weeks earlier was
  // rejected by a newer database, exited 1, and emitted zero NDJSON lines. The
  // shell said "crashed (code 1)" and dropped the one line that explained it.
  // The Rust tested classify_end in isolation but never that the tail actually
  // arrives in the payload, because it could not fake a child.
  it("carries the last twenty stderr lines as detail on a startup failure", async () => {
    const h = harness();
    await h.voice.start("s1");
    const c = h.children[0]!;

    for (let i = 1; i <= 25; i++) c.logErr(`log line ${i}`);
    c.logErr(
      "Error: migration 29 was previously applied but is missing in the resolved migrations",
    );
    await tick();
    await c.exit(1);

    const ended = h.emitted.at(-1)!.payload as {
      reason: string;
      detail: string;
    };
    expect(ended.reason).toBe("failed_to_start");

    const lines = ended.detail.split("\n");
    expect(lines).toHaveLength(STDERR_TAIL_LINES);
    // The fatal line is last, and it is the one the UI shows.
    expect(lines.at(-1)).toContain("migration 29");
    // The oldest lines fell out of the ring.
    expect(ended.detail).not.toContain("log line 1\n");
  });

  it("attaches no stderr once the session readied", async () => {
    const h = harness();
    await h.voice.start("s1");
    const c = h.children[0]!;

    c.say('{"event":"ready","session_id":"s1"}');
    c.logErr("some later log line");
    await tick();
    await c.exit(null);

    expect(h.emitted.at(-1)!.payload).toMatchObject({
      reason: "crashed",
      detail: null,
    });
  });

  // "close" fires only after stdio has closed; "exit" fires before. Binding to
  // the wrong one drops the fatal line, which is the whole point of the ring.
  it("waits for stdio to close before classifying, not just for the process", async () => {
    const h = harness();
    await h.voice.start("s1");
    const c = h.children[0]!;

    c.logErr("Error: the line that explains everything");
    // The process object reports exit before its pipes have drained.
    c.emit("exit", 1);
    await tick();
    expect(
      h.emitted.filter((e) => e.name === "voice-session-ended"),
    ).toHaveLength(0);

    await c.exit(1);
    expect((h.emitted.at(-1)!.payload as { detail: string }).detail).toContain(
      "the line that explains everything",
    );
  });
});

describe("the stale-reader guard", () => {
  // The race: stop() kills child A, A's close event is queued, a new start()
  // assigns session B, and only then does A's handler run. Without the guard
  // it clears B's state and reports B's session as ended.
  //
  // This window is WIDER in Node than in Rust, where Child::kill() followed by
  // wait() was synchronous and the slot was clear before kill returned.
  it("does not let a killed session's close event end a newer one", async () => {
    const h = harness();
    await h.voice.start("session-A");
    const a = h.children[0]!;
    a.say('{"event":"ready","session_id":"session-A"}');
    await tick();

    h.voice.killNow();
    expect(h.voice.isActive).toBe(false);

    const idB = await h.voice.start("session-B");
    expect(idB).toBe("session-B");
    const b = h.children[1]!;

    // Only now does the dead child's close arrive.
    await a.exit(null);

    expect(
      h.emitted.filter((e) => e.name === "voice-session-ended"),
    ).toHaveLength(0);
    expect(h.voice.isActive).toBe(true);
    expect(h.voice.sessionId).toBe("session-B");

    // And B still ends normally when it is B's turn.
    b.say('{"event":"exit","reason":"stdin_eof"}');
    await tick();
    await b.exit(0);
    const ended = h.emitted.filter((e) => e.name === "voice-session-ended");
    expect(ended).toHaveLength(1);
    expect(ended[0]!.payload).toMatchObject({ session_id: "session-B" });
  });

  it("does not forward a stale session's output to the renderer", async () => {
    const h = harness();
    await h.voice.start("session-A");
    const a = h.children[0]!;
    h.voice.killNow();
    await h.voice.start("session-B");

    a.say('{"event":"token","content":"from the dead session"}');
    await tick();

    expect(h.emitted.map((e) => e.name)).not.toContain("voice-token");
  });
});

describe("stopping", () => {
  it("closes stdin first and does not kill a child that goes quietly", async () => {
    const h = harness();
    await h.voice.start("s1");
    const c = h.children[0]!;

    const stopping = h.voice.stop();
    await tick();
    expect(c.stdinEnded).toBe(true);

    c.say('{"event":"exit","reason":"stdin_eof"}');
    await c.exit(0);
    await stopping;

    expect(c.killed).toBe(null);
    expect(h.voice.isActive).toBe(false);
  });

  // Faithful to the shipped behaviour: the child never reads stdin in voice
  // mode, so this path is the normal one, not the exception.
  it("escalates to SIGKILL when the child ignores stdin close", async () => {
    const h = harness();
    await h.voice.start("s1");
    const c = h.children[0]!;

    await h.voice.stop();

    expect(c.killed).toBe("SIGKILL");
    expect(h.voice.isActive).toBe(false);
  });

  it("is a no-op when no session is running", async () => {
    const h = harness();
    await expect(h.voice.stop()).resolves.toBeUndefined();
  });

  // ipcMain.handle callbacks interleave across every await, and stop() awaits a
  // timer, so without the lifecycle lock a stop and a start overlap and spawn
  // two children that both hold the microphone.
  it("serialises an overlapping stop and start into one child at a time", async () => {
    const h = harness();
    await h.voice.start("s1");

    const stopping = h.voice.stop();
    const starting = h.voice.start("s2");
    await Promise.all([stopping, starting]);

    expect(h.children).toHaveLength(2);
    expect(h.voice.sessionId).toBe("s2");
    expect(h.children[0]!.killed).toBe("SIGKILL");
  });
});

describe("a child that cannot be spawned at all", () => {
  it("reports failed_to_start with the spawn error as detail", async () => {
    const h = harness();
    await h.voice.start("s1");
    const c = h.children[0]!;

    c.emit("error", new Error("spawn EACCES"));
    await tick();

    const ended = h.emitted.at(-1)!;
    expect(ended.name).toBe("voice-session-ended");
    expect(ended.payload).toMatchObject({ reason: "failed_to_start" });
    expect((ended.payload as { detail: string }).detail).toContain("EACCES");
    expect(h.voice.isActive).toBe(false);
  });
});
