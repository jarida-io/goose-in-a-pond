// Drives the single `pond-server chat --voice --json-events` child. It owns mic and speaker in
// one process so capture, inference and speech cross no transport boundary.

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
 * One live session. Callbacks capture it so a stale reader is detected by identity:
 * `close` can arrive any number of ticks after `child.kill()`.
 */
interface Session {
  child: ChildProcess;
  id: string;
  stderrTail: string[];
  sawReady: boolean;
  cleanReason: string | null;
  stdoutClosed: boolean;
  processClosed: boolean;
  exitCode: number | null;
  ended: boolean;
}

const noopLog = { info() {}, warn() {}, debug() {} };

export class VoiceChildProcess {
  private session: Session | null = null;

  /** Serialises start and stop, which otherwise interleave across awaits into a double spawn. */
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
    // Swallow both outcomes so one failed start does not wedge every later call.
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

  /** `resume`: chat session to continue, or null for a fresh one. Resolves to the id the child got. */
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

      // Sidecars staged before `--voice` existed die on "unexpected argument" with no NDJSON; re-stage.
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
        // Spawn failed: no stdout will ever close, so drive the end path directly.
        this.log.warn(`voice child failed to spawn: ${e.message}`);
        session.stderrTail.push(e.message);
        session.stdoutClosed = true;
        session.processClosed = true;
        this.finish(session);
      });

      this.attachStderr(session);
      this.attachStdout(session);

      // "close", not "exit": only close waits for stdio, so the fatal last stderr line is kept.
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
      // A child dying before `ready` explains itself only here: --json-events leaves stdout silent.
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
        // Non-contract lines are normal on this stream: warn, never crash.
        this.log.warn(
          `ignoring non-contract voice child line (${result.error}): ${line}`,
        );
        return;
      }
      const value = result.value;
      if (value.kind === "skip") return;
      if (value.kind === "exit") {
        // Held: the ended event goes out once, when stdout closes.
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
   * Emits `voice-session-ended` once both stdout and the process have closed: acting on the
   * first alone loses the exit code (fast exit) or the stderr tail (slow exit).
   */
  private finish(session: Session): void {
    if (session.ended) return;
    if (!session.stdoutClosed || !session.processClosed) return;
    session.ended = true;

    // A newer session owns the slots now; this is why `killNow` clears `this.session` first.
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
      // Warn, not debug: this is the only explanation for a voice mode that will not start.
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
   * Closes stdin (the clean-exit signal), waits out the grace period, then kills. The shipped
   * voice child never reads stdin, so this always escalates; that is a child defect.
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
   * Kills synchronously enough for a process-exit handler. Clearing `this.session` first makes the
   * pending close handler stale, so it neither reports this stop nor tears down a newer session.
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
