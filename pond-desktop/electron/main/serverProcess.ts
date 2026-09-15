// Finding, starting and watching pond-server.
//
// A port of src-tauri/src/process.rs. Three things here are load-bearing and
// each has cost someone a debugging session before:
//
//   * Parent-managed mode. `pond-server serve --native` launches this shell
//     and pins the port via GIAP_SERVER_PORT. The parent has already bound the
//     socket, so spawning our own would either fail or fight for it, and the
//     window comes up blank -- "the app launches but it shows nothing".
//
//   * One patience budget, two ways to stop waiting. Both paths time the same
//     binary doing the same cold start -- loading face recognition, Whisper
//     and TTS routinely takes over a minute -- so both get the same wall
//     clock. What differs is how the wait can END: a server we spawned
//     ourselves can fail fast, because we hold its ChildProcess and can watch
//     it die, where a parent-managed one can only ever time out. Do not give
//     the self-spawned path a shorter clock -- that is what produced the
//     two-server bug the next bullet exists to prevent.
//
//   * At most one live child, ever. `this.child` is never overwritten while
//     the process it names is alive. A spawn that times out leaves a RUNNING
//     child behind, and before this was enforced the next recovery reassigned
//     the field and orphaned it: one launch, two servers, and the first one
//     surviving the quit because shutdown() could only see the second. The
//     assignment goes through adoptChild(), which throws rather than let that
//     happen again.
//
//   * Recovery is serialised. The startup probe and the periodic health check
//     both call ensureRunning, and without the lock they race into two
//     children fighting for one port. The lock is also why a live child seen
//     at the top of ensureRunningInner has ALREADY spent its full budget --
//     there is no "wait a bit longer" case to write.

import { join } from "node:path";
import { existsSync } from "node:fs";
import { spawn, type ChildProcess } from "node:child_process";
import {
  reapPidfileOrphan,
  writePidfile,
  removePidfile,
  realOrphanDeps,
  SERVER_CHILD,
  type OrphanDeps,
} from "./orphan";

export const DEFAULT_PORT = 4000;
const HEALTH_TIMEOUT_MS = 2_000;
const POLL_MS = 500;

/**
 * Attempts when we spawned the server ourselves: 120 seconds.
 *
 * The same budget a parent-managed server gets, because it is the same binary
 * doing the same cold start. This wait can still end early -- see
 * `ensureRunningInner`, which gives up the moment the child exits.
 */
export const SPAWNED_POLL_ATTEMPTS = 240;

/** Attempts when a parent owns the server: 120 seconds. */
export const PARENT_MANAGED_POLL_ATTEMPTS = 240;

/**
 * How long a wedged child gets to honour SIGTERM before it is SIGKILLed.
 *
 * Matches the voice driver's GRACEFUL_STOP_MS. SIGTERM first, deliberately:
 * pond-server flushes the SQLite WAL on the way out.
 */
export const KILL_GRACE_MS = 3_000;

/**
 * Decide the server URL and whether a parent owns it.
 *
 * An empty string counts as unset, matching the Rust: an exported-but-blank
 * variable must not put us into parent-managed mode with a malformed URL.
 */
export function resolveServerUrl(port: string | undefined): {
  url: string;
  parentManaged: boolean;
} {
  if (typeof port === "string" && port.trim() !== "") {
    return { url: `http://127.0.0.1:${port.trim()}`, parentManaged: true };
  }
  return { url: `http://127.0.0.1:${DEFAULT_PORT}`, parentManaged: false };
}

export interface BinaryLookup {
  /** POND_SERVER_BIN, kept because dev and tests both rely on it. */
  override?: string | undefined;
  isPackaged: boolean;
  /** Electron's process.resourcesPath -- Contents/Resources inside a bundle. */
  resourcesPath: string;
  /** Repo root, for the dev build. */
  repoRoot: string;
  platform: NodeJS.Platform;
  exists?: (path: string) => boolean;
}

/**
 * Locate the pond-server binary.
 *
 * Three branches, two of which are KNOWN rather than probed. The Rust had to
 * guess -- it walked the running executable's siblings first specifically so a
 * packaged app could not fall back to a stray `binaries/` folder in the cwd --
 * because it had no reliable way to ask whether it was packaged. `app.isPackaged`
 * is a hard boolean, so that whole heuristic goes.
 */
export function resolveServerBinary(opts: BinaryLookup): string | null {
  const exists = opts.exists ?? existsSync;
  const name = opts.platform === "win32" ? "pond-server.exe" : "pond-server";

  // Highest priority so the dev flow keeps working even inside a packaged app.
  if (opts.override && opts.override.trim() !== "" && exists(opts.override)) {
    return opts.override;
  }

  if (opts.isPackaged) {
    const bundled = join(opts.resourcesPath, name);
    return exists(bundled) ? bundled : null;
  }

  for (const candidate of [
    join(opts.repoRoot, "target", "release", name),
    join(opts.repoRoot, "target", "debug", name),
    join(opts.repoRoot, "pond-desktop", "resources", name),
  ]) {
    if (exists(candidate)) return candidate;
  }
  return null;
}

/**
 * How long to wait before the next recovery attempt, in seconds.
 *
 * Exponential from 5 seconds, capped at 5 minutes, so a server that cannot
 * start does not get hammered forever.
 */
export function recoveryBackoffSeconds(consecutiveFailures: number): number {
  const BASE = 5;
  const MAX = 300;
  if (consecutiveFailures <= 0) return 0;
  const shift = Math.min(consecutiveFailures - 1, 6);
  return Math.min(BASE * 2 ** shift, MAX);
}

export interface ServerDeps {
  lookup: Omit<BinaryLookup, "override">;
  env?: NodeJS.ProcessEnv;
  fetchFn?: typeof fetch;
  spawnFn?: typeof spawn;
  sleep?: (ms: number) => Promise<void>;
  log?: { info(m: string): void; warn(m: string): void };
  /**
   * The runtime port file, with the time it was written.
   *
   * Seam rather than a path so tests never touch the real data directory.
   */
  readPortFile?: () => { port: number; mtimeMs: number } | null;
  /** Called when the bound port turns out not to be the one we assumed. */
  onUrlChanged?: (url: string) => void;
  /** Orphan-reaping seams. Tests MUST stub these; see the note in the tests. */
  orphanDeps?: OrphanDeps;
  writePid?: (pid: number) => void;
  removePid?: () => void;
}

const noopLog = { info() {}, warn() {} };

export class ServerProcess {
  private child: ChildProcess | null = null;
  private recovery: Promise<unknown> = Promise.resolve();
  /** Set by shutdown(). Once true this instance never spawns again. */
  private stopped = false;
  /** Orphan reaping is once per run; see cleanupOrphans. */
  private reapedOrphans = false;
  private currentUrl: string;
  readonly parentManaged: boolean;

  constructor(private readonly deps: ServerDeps) {
    const env = deps.env ?? process.env;
    const resolved = resolveServerUrl(env["GIAP_SERVER_PORT"]);
    this.currentUrl = resolved.url;
    this.parentManaged = resolved.parentManaged;
  }

  /**
   * Where the server is, as far as we know.
   *
   * Not readonly: `--port` is a START port for the Rust's bind_with_fallback,
   * so a server that finds 4000 taken binds 4001 and says nothing. Assuming
   * 4000 in that case points the UI at a server that is not there.
   */
  get url(): string {
    return this.currentUrl;
  }

  private get log() {
    return this.deps.log ?? noopLog;
  }

  private sleep(ms: number): Promise<void> {
    return this.deps.sleep
      ? this.deps.sleep(ms)
      : new Promise((r) => setTimeout(r, ms));
  }

  /** Is pond-server answering right now? */
  async healthCheck(url = this.url): Promise<boolean> {
    const doFetch = this.deps.fetchFn ?? fetch;
    try {
      const res = await doFetch(`${url}/api/v1/health`, {
        signal: AbortSignal.timeout(HEALTH_TIMEOUT_MS),
      });
      return res.ok;
    } catch {
      return false;
    }
  }

  /**
   * Make sure pond-server is reachable, spawning it if this shell owns it.
   *
   * Serialised, so the startup probe and the periodic health check cannot race
   * into two children fighting for one port.
   */
  ensureRunning(): Promise<string> {
    const run = this.recovery.then(
      () => this.ensureRunningInner(),
      () => this.ensureRunningInner(),
    );
    this.recovery = run.then(
      () => undefined,
      () => undefined,
    );
    return run;
  }

  private async ensureRunningInner(): Promise<string> {
    if (this.stopped) {
      throw new Error("the shell is quitting; not starting pond-server");
    }

    // BEFORE the health check, and this ordering is the second half of the
    // bug: an orphan from a previous run is healthy on 4000, so checking first
    // means adopting it and never reaping it at all.
    this.cleanupOrphans();

    if (await this.healthCheck()) {
      this.log.info(`connected to an existing pond-server at ${this.url}`);
      return this.url;
    }

    if (this.parentManaged) {
      // The parent bound the socket; we must not spawn. Just wait, patiently.
      this.log.info(
        `parent-managed pond-server detected; waiting for ${this.url}`,
      );
      for (let i = 0; i < PARENT_MANAGED_POLL_ATTEMPTS; i++) {
        await this.sleep(POLL_MS);
        if (await this.healthCheck()) {
          this.log.info(`parent pond-server is ready at ${this.url}`);
          return this.url;
        }
      }
      throw new Error(
        `Parent-managed pond-server at ${this.url} did not become healthy within 120 s`,
      );
    }

    this.reapExitedChild();
    // A child still here after the reap is one whose entire budget was spent
    // by the call that spawned it -- the recovery lock guarantees that. It is
    // wedged, not slow, so it is replaced rather than waited on again. Not
    // killing it here is what put two servers on this machine.
    if (this.child) await this.killWedgedChild();

    const binary = resolveServerBinary({
      ...this.deps.lookup,
      override: (this.deps.env ?? process.env)["POND_SERVER_BIN"],
    });
    if (binary === null) {
      throw new Error(
        `No pond-server running at ${this.url} and no binary found. ` +
          "Stage one with `npm run stage:server`, or set POND_SERVER_BIN.",
      );
    }

    this.log.info(`spawning pond-server from ${binary}`);
    const spawnFn = this.deps.spawnFn ?? spawn;
    // Grounding the child's cwd at the repo root in dev is what lets the
    // GooseAdapter inside it resolve extension paths like
    // extensions/music/src/server.ts regardless of where the shell was
    // started. A no-op in a packaged app, where those paths are absolute.
    const spawnedAt = Date.now();
    const child = spawnFn(binary, ["serve", "--port", String(DEFAULT_PORT)], {
      stdio: "inherit",
      ...(this.deps.lookup.isPackaged
        ? {}
        : { cwd: this.deps.lookup.repoRoot }),
    });
    this.adoptChild(child);
    // Recorded so the NEXT launch can reap this process if we are killed
    // hard enough that no quit path runs.
    if (typeof child.pid === "number") this.writePid(child.pid);

    for (let i = 0; i < SPAWNED_POLL_ATTEMPTS; i++) {
      await this.sleep(POLL_MS);
      if (await this.healthCheck()) {
        this.log.info(`spawned pond-server is ready at ${this.url}`);
        return this.url;
      }
      if (this.adoptBoundPort(spawnedAt)) {
        this.log.info(`spawned pond-server is ready at ${this.url}`);
        return this.url;
      }
      // Fail fast on a child that died rather than serving out the budget.
      // A bad argument or a port it cannot bind should surface in seconds,
      // naming the exit status, not two minutes later as "not healthy".
      const gone = this.exitDescription();
      if (gone !== null) {
        this.child = null;
        throw new Error(
          `Spawned pond-server exited before it was ready (${gone})`,
        );
      }
      if (this.stopped) {
        throw new Error("pond-server startup abandoned; the shell is quitting");
      }
    }
    throw new Error(
      `Spawned pond-server did not become healthy within ${this.budgetSeconds()} s`,
    );
  }

  /**
   * Reap a sidecar orphaned by a hard kill of a previous run.
   *
   * Once per run, deliberately: a later recovery must not kill a server we
   * legitimately adopted minutes ago. Never in parent-managed mode -- the
   * parent there is `pond-server serve --native`, whose command line matches
   * the sidecar predicate exactly, so a stale pidfile plus a reused pid could
   * have us kill our own parent.
   */
  private cleanupOrphans(): void {
    if (this.reapedOrphans || this.parentManaged) return;
    this.reapedOrphans = true;
    reapPidfileOrphan(SERVER_CHILD, this.deps.orphanDeps ?? realOrphanDeps);
  }

  /**
   * Adopt the port the server actually bound, if it is not the one we assumed.
   *
   * Only ever consulted while OUR child is starting, and only for a file
   * written after we spawned it -- a port file left by yesterday's run must
   * never outrank today's spawn. The port still has to answer before we move.
   */
  private adoptBoundPort(spawnedAt: number): boolean {
    const read = this.deps.readPortFile;
    if (!read) return false;

    const file = read();
    if (file === null || file.mtimeMs < spawnedAt) return false;

    const candidate = `http://127.0.0.1:${file.port}`;
    if (candidate === this.currentUrl) return false;

    this.log.warn(
      `pond-server bound port ${file.port} rather than ${DEFAULT_PORT}; adopting it`,
    );
    this.currentUrl = candidate;
    this.deps.onUrlChanged?.(candidate);
    return true;
  }

  private writePid(pid: number): void {
    (this.deps.writePid ?? ((p: number) => writePidfile(SERVER_CHILD, p)))(pid);
  }

  private removePid(): void {
    (this.deps.removePid ?? (() => removePidfile(SERVER_CHILD)))();
  }

  /** Seconds a self-spawned server is given, derived so the message cannot drift. */
  private budgetSeconds(): number {
    return (SPAWNED_POLL_ATTEMPTS * POLL_MS) / 1_000;
  }

  /**
   * How the child ended, or null while it is still running.
   *
   * Both halves matter: a child killed by a signal leaves `exitCode` null and
   * sets `signalCode`, so testing the exit code alone reports a SIGKILLed
   * server as live forever.
   */
  private exitDescription(): string | null {
    const child = this.child;
    if (!child) return null;
    if (child.signalCode !== null && child.signalCode !== undefined) {
      return `killed by ${child.signalCode}`;
    }
    if (child.exitCode !== null && child.exitCode !== undefined) {
      return `exit code ${child.exitCode}`;
    }
    return null;
  }

  /**
   * Take ownership of a freshly spawned child.
   *
   * The single assignment point for `this.child`, and it refuses to overwrite a
   * live one. That is the whole invariant: an orphaned sidecar is invisible to
   * shutdown() and gets adopted by the NEXT launch's health check, so it can
   * outlive several runs of the app.
   */
  private adoptChild(proc: ChildProcess): void {
    if (this.child !== null) {
      throw new Error("refusing to spawn a second pond-server over a live one");
    }
    this.child = proc;
  }

  /** Drop a child that has already ended. Kills nothing. */
  private reapExitedChild(): void {
    const gone = this.exitDescription();
    if (gone !== null) {
      this.log.warn(`the pond-server we spawned had already ended (${gone})`);
      this.child = null;
    }
  }

  /**
   * Terminate a child that is alive but never became healthy.
   *
   * SIGTERM, a grace period, then SIGKILL. Clears `this.child` unconditionally:
   * a process we could not confirm dead must still not stay referenced, or it
   * blocks every future recovery through adoptChild().
   */
  private async killWedgedChild(): Promise<void> {
    const child = this.child;
    if (!child) return;
    this.log.warn(
      `pond-server is running but never became healthy within ${this.budgetSeconds()} s; replacing it`,
    );

    try {
      child.kill("SIGTERM");
    } catch {
      // Already gone.
    }

    const rounds = Math.ceil(KILL_GRACE_MS / POLL_MS);
    for (let i = 0; i < rounds; i++) {
      if (this.exitDescription() !== null) {
        this.child = null;
        this.log.info("the wedged pond-server exited after SIGTERM");
        return;
      }
      await this.sleep(POLL_MS);
    }

    this.log.warn("the wedged pond-server ignored SIGTERM; sending SIGKILL");
    try {
      child.kill("SIGKILL");
    } catch {
      // Already gone.
    }
    await this.sleep(POLL_MS);
    if (this.exitDescription() === null) {
      this.log.warn(
        "could not confirm the wedged pond-server died; releasing it anyway",
      );
    }
    this.child = null;
  }

  /**
   * Kill the server we spawned and refuse to start another.
   *
   * Idempotent, synchronous, and safe from a process exit hook. "Refuse to
   * start another" is deliberate and safe only because quit is the sole
   * caller: a health-loop tick or a renderer request that lands mid-teardown
   * must not spawn a sidecar nobody will ever shut down. Never touches a
   * parent-managed server.
   */
  shutdown(): void {
    this.stopped = true;
    if (!this.child) return;
    try {
      this.child.kill("SIGTERM");
    } catch {
      // Already gone.
    }
    this.child = null;
    this.removePid();
    this.log.info("pond-server shut down");
  }
}
