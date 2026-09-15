// Driving the voice child.
//
// Exactly one `pond-server chat --voice --json-events` may be alive at a time.
// It owns the microphone and the speaker for the whole session, in one
// process, which is the reason this architecture exists: the terminal voice
// loop is reliable because it has no transport boundaries between capture,
// inference and speech.
//
// This is a port of VoiceChatProcess from src-tauri/src/chat_process.rs. Most
// of the Rust's machinery was there because its readers were real OS threads
// -- Arc, Mutex, AtomicBool, a poisoned-lock helper -- and none of that
// survives, because both readers here are on the event loop. Two guards do
// survive, and they are the ones that encode real races rather than threading:
// a stale reader must not reap a newer session, and two lifecycle calls must
// not interleave.

import type { ChildProcess } from "node:child_process";
import { spawn as realSpawn } from "node:child_process";
import { createInterface } from "node:readline";
import { randomUUID } from "node:crypto";
import type { ShellEvent, ShellEvents } from "../../../src/shell/contract";
import {
  classifyLine,
  classifyEnd,
  sessionToJoin,
  STDERR_TAIL_LINES,
} from "./ndjson";
import {
  writePidfile,
  removePidfile,
  reapPidfileOrphan,
  realOrphanDeps,
  type OrphanDeps,
  VOICE_CHILD,
} from "../orphan";

/** How long to wait after closing stdin before escalating to a kill. */
export const GRACEFUL_STOP_MS = 3_000;

/** How often to check whether the child went away during that grace period. */
export const GRACEFUL_POLL_MS = 50;

export interface VoiceChildDeps {
  /** Where the pond-server binary is, or null if it could not be found. */
  resolveBinary(): string | null;
  /** Working directory for the child. Dev builds ground it at the repo root. */
  cwd?: string | undefined;
  /** Push an event to the renderer. */
  emit<E extends ShellEvent>(name: E, payload: ShellEvents[E]): void;
  spawn?: typeof realSpawn;
  newSessionId?: () => string;
  orphanDeps?: OrphanDeps;
  writePid?: (pid: number) => void;
  removePid?: () => void;
  gracefulStopMs?: number;
  pollMs?: number;
  log?: {
    info(message: string): void;
    warn(message: string): void;
    debug(message: string): void;
  };
}

/**
 * One live session. Every callback captures the object it belongs to, so a
 * stale reader can compare identity rather than consult shared state.
 *
 * This replaces the Rust's `AtomicU64` generation counter. The race is real
 * and the window is WIDER in Node: `Child::kill()` followed by `wait()` is
 * synchronous in Rust, so the slot was already clear when kill returned, but
 * `child.kill()` here only delivers a signal and `close` arrives an unbounded
 * number of ticks later. Object identity is a generation counter that cannot
 * be mis-incremented.
 */
interface Session {
  child: ChildProcess;
  id: string;
  stderrTail: string[];
  sawReady: boolean;
  cleanReason: string | null;
  /** Set once both stdout and the process itself have closed. */
  stdoutClosed: boolean;
  processClosed: boolean;
  exitCode: number | null;
  ended: boolean;
}

const noopLog = { info() {}, warn() {}, debug() {} };

export class VoiceChildProcess {
  private session: Session | null = null;

  /**
   * Serialises start and stop. `ipcMain.handle` callbacks interleave freely
   * across every `await`, and `stop()` awaits a three-second timer, so without
   * this a stop and a start can overlap into a double spawn.
   */
  private lifecycle: Promise<unknown> = Promise.resolve();

  constructor(private readonly deps: VoiceChildDeps) {}

  get isActive(): boolean {
    return this.session !== null;
  }

  get sessionId(): string | null {
    return this.session?.id ?? null;
  }

  private get log() {
    return this.deps.log ?? noopLog;
  }

  /** Run `fn` with the lifecycle lock held. */
  private serialise<T>(fn: () => Promise<T>): Promise<T> {
    const run = this.lifecycle.then(fn, fn);
    // Keep the chain alive whether `fn` resolves or rejects, so one failed
    // start does not wedge every later call.
    this.lifecycle = run.then(
      () => undefined,
      () => undefined,
    );
    return run;
  }

  /** Reap a child orphaned by a hard kill of a previous run. */
  cleanupOrphans(): void {
    reapPidfileOrphan(VOICE_CHILD, this.deps.orphanDeps ?? realOrphanDeps);
  }

  /**
   * Spawn the voice child and begin forwarding its output.
   *
   * `resume` is the chat session to continue, or null for a fresh one.
   * Resolves to the session id the child was actually given.
   */
  start(resume: string | null): Promise<string> {
    return this.serialise(async () => {
      if (this.session) {
        throw new Error("a voice session is already running");
      }
      this.cleanupOrphans();

      const binary = this.deps.resolveBinary();
      if (binary === null) {
        throw new Error(
          "No pond-server binary found. Stage one with `npm run stage:server`, " +
            "or set POND_SERVER_BIN.",
        );
      }

      const id = sessionToJoin(resume, this.deps.newSessionId ?? randomUUID);
      const spawnFn = this.deps.spawn ?? realSpawn;

      this.log.info(
        `spawning voice child: ${binary} chat --voice --json-events --session-id ${id}`,
      );

      // `--voice` replaced `--input whisper`. A staged sidecar older than that
      // rename dies on "unexpected argument" before emitting a single NDJSON
      // line -- the same symptom as a sidecar older than the database's
      // migrations, and the same fix: re-stage it. The stderr tail attached to
      // voice-session-ended carries the message that names the flag.
      const child = spawnFn(
        binary,
        ["chat", "--voice", "--json-events", "--session-id", id],
        {
          stdio: ["pipe", "pipe", "pipe"],
          ...(this.deps.cwd ? { cwd: this.deps.cwd } : {}),
        },
      );

      const session: Session = {
        child,
        id,
        stderrTail: [],
        sawReady: false,
        cleanReason: null,
        stdoutClosed: false,
        processClosed: false,
        exitCode: null,
        ended: false,
      };
      this.session = session;

      if (typeof child.pid === "number") {
        (
          this.deps.writePid ??
          ((pid: number) => writePidfile(VOICE_CHILD, pid))
        )(child.pid);
      }

      child.once("error", (e: Error) => {
        // Failure to spawn at all: there is no stdout to close, so drive the
        // end path directly rather than waiting for events that never come.
        this.log.warn(`voice child failed to spawn: ${e.message}`);
        session.stderrTail.push(e.message);
        session.stdoutClosed = true;
        session.processClosed = true;
        this.finish(session);
      });

      this.attachStderr(session);
      this.attachStdout(session);

      // "close", never "exit". `exit` fires when the process terminates, but
      // `close` fires only once its stdio has also closed -- and the last
      // stderr line is usually the fatal one, which is the entire point of the
      // ring buffer. Classifying on `exit` would routinely drop it.
      child.once("close", (code) => {
        session.exitCode = typeof code === "number" ? code : null;
        session.processClosed = true;
        this.finish(session);
      });

      return id;
    });
  }

  private attachStderr(session: Session): void {
    if (!session.child.stderr) return;
    const rl = createInterface({
      input: session.child.stderr,
      crlfDelay: Infinity,
    });
    rl.on("line", (line) => {
      if (line.trim() === "") return;
      this.log.debug(`[voice child stderr] ${line}`);
      // The ring is what makes a startup failure explicable. A child that dies
      // before `ready` writes its reason ONLY here: the child's human-facing
      // banner macro is compiled to a no-op under --json-events, so stdout
      // carries nothing at all.
      session.stderrTail.push(line);
      if (session.stderrTail.length > STDERR_TAIL_LINES)
        session.stderrTail.shift();
    });
  }

  private attachStdout(session: Session): void {
    if (!session.child.stdout) {
      session.stdoutClosed = true;
      return;
    }
    const rl = createInterface({
      input: session.child.stdout,
      crlfDelay: Infinity,
    });

    rl.on("line", (line) => {
      const result = classifyLine(line);
      if (!result.ok) {
        // A non-contract line is an ordinary event on this stream. Warn, never
        // crash, and read the next one.
        this.log.warn(
          `ignoring non-contract voice child line (${result.error}): ${line}`,
        );
        return;
      }
      const value = result.value;
      if (value.kind === "skip") return;
      if (value.kind === "exit") {
        // Held rather than emitted: the ended event goes out once, when stdout
        // actually closes.
        session.cleanReason = value.reason;
        return;
      }
      if (value.name === "voice-ready") session.sawReady = true;
      // A stale session's lines must not reach the renderer either.
      if (this.session !== session) return;
      this.deps.emit(value.name, value.payload);
    });

    rl.once("close", () => {
      session.stdoutClosed = true;
      this.finish(session);
    });
  }

  /**
   * Emit `voice-session-ended` once, after BOTH the stdout reader and the
   * process itself have closed.
   *
   * The Rust read stdout to EOF and then called `wait()`, sequentially on one
   * thread. Node gives two independent async signals, and joining them is not
   * optional: classifying on whichever arrives first yields an undefined exit
   * code on a fast exit, or a truncated stderr tail on a slow one.
   */
  private finish(session: Session): void {
    if (session.ended) return;
    if (!session.stdoutClosed || !session.processClosed) return;
    session.ended = true;

    // A newer session has taken over, so the slots belong to it now. This is
    // the rapid stop/start guard, and it is why `killNow` clears `this.session`
    // before killing.
    if (this.session !== session) {
      this.log.debug(
        `stale voice reader for session ${session.id} observed close after a newer session took over; not reaping`,
      );
      return;
    }

    const { reason, detail } = classifyEnd(
      session.cleanReason,
      session.sawReady,
      session.stderrTail,
    );
    if (detail !== null) {
      // At warn, not debug: this is the whole explanation for a voice mode
      // that will not start, and a debug-level stream is what hid it before.
      this.log.warn(
        `voice child failed to start; child stderr tail:\n${detail}`,
      );
    }

    this.session = null;
    (this.deps.removePid ?? (() => removePidfile(VOICE_CHILD)))();

    this.deps.emit("voice-session-ended", {
      code: session.exitCode,
      reason,
      session_id: session.id,
      detail,
    });
  }

  /**
   * Stop the child: close stdin, wait out the grace period, then kill.
   *
   * Closing stdin is the clean-exit signal -- the child sees EOF, breaks its
   * run loop and closes stdout. Worth knowing: in the shipped voice
   * configuration the child never actually reads stdin, so this always burns
   * the full grace period and escalates. That is a defect in the child, not
   * here, and it is ported faithfully rather than papered over.
   */
  stop(): Promise<void> {
    return this.serialise(async () => {
      const session = this.session;
      if (!session) return;

      session.child.stdin?.end();

      const deadline =
        Date.now() + (this.deps.gracefulStopMs ?? GRACEFUL_STOP_MS);
      const poll = this.deps.pollMs ?? GRACEFUL_POLL_MS;
      while (Date.now() < deadline) {
        if (this.session !== session) {
          this.log.info("voice child exited cleanly after stdin close");
          return;
        }
        await new Promise((r) => setTimeout(r, poll));
      }

      if (this.session === session) {
        this.log.warn(
          "voice child did not exit within the grace period; killing",
        );
        this.killNow();
      }
    });
  }

  /**
   * Kill the child immediately, synchronously enough to be called from a
   * process-exit handler.
   *
   * Clearing `this.session` FIRST is load-bearing: it is what makes the
   * pending close handler stale, so it cannot emit an end reason for a session
   * the user deliberately stopped, or tear down a newer one.
   */
  killNow(): void {
    const session = this.session;
    if (!session) return;
    this.session = null;
    try {
      session.child.kill("SIGKILL");
    } catch {
      // Already gone.
    }
    (this.deps.removePid ?? (() => removePidfile(VOICE_CHILD)))();
    this.log.info("voice child killed");
  }
}
