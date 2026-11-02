// Drives the single `pond-server chat --voice --json-events` child. It owns mic and speaker in
// one process so capture, inference and speech cross no transport boundary. Where the server
// already listens in the room, it attaches to that instead and spawns nothing.

import type { ChildProcess } from "node:child_process";
import { spawn as realSpawn } from "node:child_process";
import { createInterface } from "node:readline";
import { randomUUID } from "node:crypto";
import type { ShellEvent, ShellEvents } from "../../../src/shell/contract";
import {
  classifyLine,
  classifyEnd,
  encodeCommand,
  sessionToJoin,
  STDERR_TAIL_LINES,
  type VoiceChildCommand,
} from "./ndjson";
import {
  writePidfile,
  removePidfile,
  reapPidfileOrphan,
  realOrphanDeps,
  type OrphanDeps,
  VOICE_CHILD,
} from "../orphan";
import type { RoomEvents, RoomStatus, RoomVoiceClient } from "./roomClient";

/** How long to wait after `stop` and stdin close before escalating to a kill. */
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
  /** The server's room voice; with it available, a session attaches rather than spawns. */
  room?: RoomVoiceClient;
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

/** A session attached to the server's room voice. Callbacks capture it, as with `Session`. */
interface Attached {
  id: string;
  events: RoomEvents | null;
  sawReady: boolean;
  /** Set once the shell asked it to stop, so the stream's end is not reported as a crash. */
  stopping: boolean;
  ended: boolean;
}

const noopLog = { info() {}, warn() {}, debug() {} };

export class VoiceChildProcess {
  private session: Session | null = null;

  private attached: Attached | null = null;

  /** Serialises start and stop, which otherwise interleave across awaits into a double spawn. */
  private lifecycle: Promise<unknown> = Promise.resolve();

  constructor(private readonly deps: VoiceChildDeps) {}

  get isActive(): boolean {
    return this.session !== null || this.attached !== null;
  }

  get sessionId(): string | null {
    return this.session?.id ?? this.attached?.id ?? null;
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
      if (this.session || this.attached) {
        throw new Error("a voice session is already running");
      }
      const room = this.deps.room;
      const status = room ? await room.status() : null;
      if (room && status?.available) return this.attach(room, status);
      if (room) {
        this.log.info(
          `the server's room voice is ${status ? `unavailable (${status.unavailable_reason})` : "not reachable"}; spawning a voice child`,
        );
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

      // A write to a child that already died fails as EPIPE here; unhandled, it would crash main.
      child.stdin?.on("error", (e: Error) => {
        this.log.debug(`voice child stdin closed under a write: ${e.message}`);
      });

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

  /** Listen through the server's room voice; the room keeps its own session. */
  private async attach(room: RoomVoiceClient, status: RoomStatus): Promise<string> {
    const on = status.enabled ? status : ((await room.setEnabled(true)) ?? status);
    const attached: Attached = {
      id: on.session_id ?? status.session_id ?? "",
      events: null,
      sawReady: false,
      stopping: false,
      ended: false,
    };
    this.attached = attached;
    this.log.info(`attached to the server's room voice (session ${attached.id || "starting"})`);
    attached.events = room.events(
      (line) => this.onRoomLine(attached, line),
      (error) => this.onRoomEnd(attached, error),
    );
    // A room already listening has sent its ready line; one still starting sends it soon.
    if (on.session_id) {
      attached.sawReady = true;
      this.deps.emit("voice-ready", { session_id: on.session_id });
      if (on.state !== "off" && on.state !== "starting") this.deps.emit("voice-state", on.state);
    }
    return attached.id;
  }

  private onRoomLine(attached: Attached, line: string): void {
    if (this.attached !== attached) return;
    const result = classifyLine(line);
    if (!result.ok) {
      this.log.warn(`ignoring non-contract room voice line (${result.error}): ${line}`);
      return;
    }
    const value = result.value;
    if (value.kind === "skip") return;
    if (value.kind === "exit") {
      // The room's loop ended on its own: this session is over, as a child's would be.
      if (!attached.stopping) this.endAttached(attached, value.reason);
      return;
    }
    if (value.name === "voice-ready") {
      attached.sawReady = true;
      attached.id = (value.payload as { session_id: string }).session_id || attached.id;
    }
    this.deps.emit(value.name, value.payload);
  }

  private onRoomEnd(attached: Attached, error: string | null): void {
    if (attached.stopping || this.attached !== attached) return;
    if (error) this.log.warn(`room voice events ended: ${error}`);
    this.endAttached(attached, classifyEnd(null, attached.sawReady, []).reason);
  }

  private endAttached(attached: Attached, reason: string): void {
    if (attached.ended) return;
    attached.ended = true;
    attached.events?.close();
    if (this.attached !== attached) return;
    this.attached = null;
    this.deps.emit("voice-session-ended", {
      code: null,
      reason,
      session_id: attached.id,
      detail: null,
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

  /** False when stdin is already closed. A write the pipe then refuses surfaces on its `error`. */
  private send(session: Session, command: VoiceChildCommand): boolean {
    const stdin = session.child.stdin;
    if (!stdin || stdin.destroyed || stdin.writableEnded) return false;
    stdin.write(encodeCommand(command));
    return true;
  }

  /**
   * Asks the child to stop speaking and end its turn. Not serialised: it changes no lifecycle
   * state, and during a start or stop there is no turn to end.
   */
  interrupt(): void {
    if (this.attached) {
      void this.deps.room?.interrupt();
      return;
    }
    const session = this.session;
    if (!session || !this.send(session, "interrupt")) {
      this.log.debug("no voice turn to interrupt");
    }
  }

  /**
   * Sends `stop` and closes stdin, then waits out the grace period. The kill after it is for a
   * child that is stuck, or a sidecar staged before the child read stdin.
   */
  stop(): Promise<void> {
    return this.serialise(async () => {
      const attached = this.attached;
      if (attached) {
        attached.stopping = true;
        attached.events?.close();
        await this.deps.room?.setEnabled(false);
        this.endAttached(attached, "stopped");
        return;
      }
      const session = this.session;
      if (!session) return;

      this.send(session, "stop");
      session.child.stdin?.end();

      const deadline =
        Date.now() + (this.deps.gracefulStopMs ?? GRACEFUL_STOP_MS);
      const poll = this.deps.pollMs ?? GRACEFUL_POLL_MS;
      while (Date.now() < deadline) {
        if (this.session !== session) {
          this.log.info("voice child exited cleanly after stop");
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
    const attached = this.attached;
    if (attached) {
      // The room is serve's: on quit only this app's listening ends.
      this.attached = null;
      attached.stopping = true;
      attached.events?.close();
      return;
    }
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
