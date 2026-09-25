// Finding, starting and watching pond-server. Invariants: never spawn when a parent owns the
// server, at most one live child, and recovery is serialised.

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
 * 120 s, the parent-managed budget too: same binary, same cold start (often over a minute).
 * Don't shorten it; a timed-out spawn leaves a running child. It still ends early on exit.
 */
export const SPAWNED_POLL_ATTEMPTS = 240;

/** Attempts when a parent owns the server: 120 seconds. */
export const PARENT_MANAGED_POLL_ATTEMPTS = 240;

/**
 * SIGTERM grace before SIGKILL, as the voice driver's GRACEFUL_STOP_MS. SIGTERM first:
 * pond-server flushes the SQLite WAL on the way out.
 */
export const KILL_GRACE_MS = 3_000;

/** The server URL, and whether a parent owns it; a blank port counts as unset, as in the Rust. */
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

/** Locate pond-server: the override, else the bundle when packaged, else a repo build. */
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

/** Recovery delay in seconds: exponential from 5, capped at 5 minutes. */
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
  /** The runtime port file and its mtime; a seam so tests never touch the real data dir. */
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

  /** Where the server is, as far as we know; `--port` is only a start port (bind_with_fallback). */
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

  /** Ensure pond-server is reachable, spawning it if we own it. Serialised: no racing spawns. */
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

    // Before the health check: a healthy orphan on 4000 would otherwise be adopted, never reaped.
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
    // Any child still here spent its whole budget (the lock ensures it): wedged, so kill it.
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
    // In dev, cwd = repo root so Goose resolves relative extension paths (extensions/music/...).
    const spawnedAt = Date.now();
    const child = spawnFn(binary, ["serve", "--port", String(DEFAULT_PORT)], {
      stdio: "inherit",
      ...(this.deps.lookup.isPackaged
        ? {}
        : { cwd: this.deps.lookup.repoRoot }),
    });
    this.adoptChild(child);
    // So the next launch can reap it if we die without running any quit path.
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
      // Fail fast, naming the exit status, if the child died instead of serving out the budget.
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
   * Reap a sidecar orphaned by a hard kill; once per run, so an adopted server survives. Never
   * when parent-managed: `pond-server serve --native` matches the sidecar predicate too.
   */
  private cleanupOrphans(): void {
    if (this.reapedOrphans || this.parentManaged) return;
    this.reapedOrphans = true;
    reapPidfileOrphan(SERVER_CHILD, this.deps.orphanDeps ?? realOrphanDeps);
  }

  /**
   * Adopt the port our starting child actually bound. Only a port file written after the spawn
   * counts (never a stale one), and the port must answer first.
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

  /** How the child ended, or null if running; a signalled child has a null `exitCode`. */
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
   * The only assignment of `this.child`; refuses to overwrite a live one, whose orphan
   * shutdown() couldn't see and later launches would adopt.
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

  /** SIGTERM, grace, SIGKILL; always clears `this.child`, or adoptChild() blocks all recovery. */
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
   * Kill our server and never spawn again (quit is the only caller). Idempotent, sync, safe
   * from an exit hook; a parent-managed server is never touched.
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
