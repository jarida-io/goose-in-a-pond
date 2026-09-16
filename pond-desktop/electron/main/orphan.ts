// Reaping a child process that outlived the shell.
//
// Killing the app hard -- Force Quit, an OOM kill, a main-process crash --
// leaves the shell's children running. Nothing else cleans that up: spawned
// children are not killed when their parent dies on POSIX.
//
// Both children need this, for different reasons. The voice child, `pond-server
// chat`, holds the microphone and cannot notice on its own, because in the
// shipped voice configuration it never reads stdin and so never sees the closed
// pipe. The sidecar, `pond-server serve`, holds port 4000 -- and because the
// shell adopts any healthy server it finds there, an orphan is not merely
// leaked but INHERITED, silently, by every subsequent launch until something
// kills it.
//
// So we write the child's pid to a well-known per-user file at spawn, and on
// the next launch reap it -- but only after confirming the pid is alive AND
// still the kind of process we think it is, so a reused pid is never killed.
//
// This is a workaround for roughly twenty missing lines in the child (a
// stdin-EOF watcher racing its run loop). Once that exists, kill the shell
// during a live session and see whether the child exits on its own; if it
// does, this whole module can go. Recording that here so it is not kept
// forever by inertia.

import { execFileSync } from "node:child_process";
import { readFileSync, writeFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";

/** Injection seam, so every branch below is testable without a real process. */
export interface OrphanDeps {
  /** Is this pid alive? Signal 0 checks existence without delivering one. */
  isAlive(pid: number): boolean;
  /** The process's command line, or null if it could not be read. */
  commandLine(pid: number): string | null;
  kill(pid: number): void;
  readFile(path: string): string | null;
  removeFile(path: string): void;
  warn(message: string): void;
}

/**
 * Does this command line identify our voice child?
 *
 * Requires BOTH the `pond-server` binary token and the `chat` subcommand, so a
 * bare `pond-server serve` -- the dashboard server, which may well be running
 * -- is never mistaken for it. Keying on the binary and the subcommand and
 * never on the flags is what lets orphan recovery still reap a child spawned
 * by a version of the shell that used different flags.
 */
export function cmdlineIsVoiceChild(cmdline: string): boolean {
  return (
    cmdline.includes("pond-server") &&
    cmdline.split(/\s+/).some((tok) => tok === "chat")
  );
}

/**
 * Does this command line identify our pond-server sidecar?
 *
 * The mirror image of cmdlineIsVoiceChild, and for the same reason: the voice
 * child is also a `pond-server`, so matching the binary alone would have each
 * reaper killing the other's process. Keyed on the subcommand and never on
 * --port, so a sidecar that fell back past 4000 is still recognised as ours.
 */
export function cmdlineIsServerChild(cmdline: string): boolean {
  return (
    cmdline.includes("pond-server") &&
    cmdline.split(/\s+/).some((tok) => tok === "serve")
  );
}

/** One kind of child the shell spawns, and how to recognise it. */
export interface ChildKind {
  /** Filename token, so the two pidfiles cannot collide. */
  readonly slug: string;
  /** How this child is named in log lines. */
  readonly label: string;
  readonly matches: (cmdline: string) => boolean;
}

export const VOICE_CHILD: ChildKind = {
  slug: "voice-child",
  label: "voice child",
  matches: cmdlineIsVoiceChild,
};

export const SERVER_CHILD: ChildKind = {
  slug: "server",
  label: "pond-server sidecar",
  matches: cmdlineIsServerChild,
};

/**
 * A stable per-user token, used only to keep pidfiles from colliding between
 * accounts on a shared host. The temp dir is already user-private on most
 * platforms; this makes the isolation explicit.
 */
function perUserToken(): string {
  const uid = process.getuid?.();
  if (typeof uid === "number") return String(uid);
  const user = process.env["USER"] ?? process.env["USERNAME"] ?? "";
  if (user !== "") return user.replace(/[^A-Za-z0-9]/g, "_");
  return "default";
}

/** Path to a child's pidfile, scoped per user under the OS temp dir. */
export function pidfilePath(kind: ChildKind): string {
  return join(tmpdir(), `giap-${kind.slug}-${perUserToken()}.pid`);
}

/**
 * Parse a pid out of the pidfile's contents.
 *
 * Rejects anything that is not a positive integer. Zero and negatives matter
 * more than they look: `process.kill(0, sig)` signals the ENTIRE process
 * group, and a negative pid signals the group with that id -- so a truncated
 * or zero-filled pidfile would have the shell kill itself and everything it
 * spawned. The Rust this replaces parsed into an unsigned type and let "0"
 * through.
 */
export function readPidfile(contents: string | null): number | null {
  if (contents === null) return null;
  const trimmed = contents.trim();
  if (!/^\d+$/.test(trimmed)) return null;
  const pid = Number(trimmed);
  if (!Number.isSafeInteger(pid) || pid <= 0) return null;
  return pid;
}

/**
 * Classify a pid:
 *
 *   * `true`  -- alive, and its command line matches this kind. Ours, safe to
 *     kill.
 *   * `false` -- confirmed gone, or alive but unrelated (a reused pid). Never
 *     kill; the pidfile record is stale.
 *   * `null`  -- INDETERMINATE. We could not tell, so the caller must not
 *     treat it as dead: doing so destroys the only record of a child that may
 *     still be holding the microphone.
 *
 * The liveness check comes first and costs no subprocess, which matters
 * because the overwhelmingly common case is a stale pidfile naming a pid that
 * died long ago. The Rust ran `ps` unconditionally, including on a memory-
 * pressured board where spawning it is exactly what fails.
 */
export function pidIsChildOfKind(
  pid: number,
  kind: ChildKind,
  deps: OrphanDeps,
): boolean | null {
  if (pid <= 0) return false;

  let alive: boolean;
  try {
    alive = deps.isAlive(pid);
  } catch {
    return null;
  }
  if (!alive) return false;

  const cmdline = deps.commandLine(pid);
  if (cmdline === null) return null;
  const trimmed = cmdline.trim();
  return trimmed !== "" && kind.matches(trimmed);
}

/**
 * Read the pidfile and, if it names a live child of this kind, kill it and
 * clear the file.
 */
export function reapPidfileOrphan(
  kind: ChildKind,
  deps: OrphanDeps,
  path = pidfilePath(kind),
): void {
  const pid = readPidfile(deps.readFile(path));
  if (pid === null) return;

  switch (pidIsChildOfKind(pid, kind, deps)) {
    case true:
      deps.warn(
        `reaping orphaned ${kind.label} pid ${pid} from a previous run`,
      );
      deps.kill(pid);
      deps.removeFile(path);
      return;
    case false:
      // Dead, or a live-but-unrelated process. The record is stale, so clear it.
      deps.removeFile(path);
      return;
    default:
      // Liveness is unknown. Do NOT delete the pidfile: a real orphan may still
      // be holding the mic, and this is the only record of it. Keeping it lets
      // a later launch retry rather than orphaning the child permanently.
      deps.warn(
        `could not determine the status of ${kind.label} pidfile pid ${pid}; keeping the pidfile so a later launch can retry recovery`,
      );
  }
}

/** The real implementations, for the main process. */
export const realOrphanDeps: OrphanDeps = {
  isAlive(pid) {
    try {
      // Signal 0 performs the permission and existence checks without sending
      // anything. EPERM means it exists but is not ours -- still alive.
      process.kill(pid, 0);
      return true;
    } catch (e) {
      const code = (e as NodeJS.ErrnoException).code;
      if (code === "ESRCH") return false;
      if (code === "EPERM") return true;
      throw e;
    }
  },
  commandLine(pid) {
    try {
      // `ps -p <pid> -o command=` is portable across macOS and Linux.
      return execFileSync("ps", ["-p", String(pid), "-o", "command="], {
        encoding: "utf8",
        timeout: 5_000,
      });
    } catch {
      // Either `ps` could not be spawned, or it exited non-zero because the
      // process vanished between our liveness check and this call. Both are
      // indeterminate from here; the caller keeps the pidfile.
      return null;
    }
  },
  kill(pid) {
    try {
      process.kill(pid, "SIGKILL");
    } catch {
      // Already gone, or not ours. Nothing left to do either way.
    }
  },
  readFile(path) {
    try {
      return readFileSync(path, "utf8");
    } catch {
      return null;
    }
  },
  removeFile(path) {
    rmSync(path, { force: true });
  },
  warn(message) {
    console.warn(`[giap] ${message}`);
  },
};

/**
 * Record the live child's pid. Best effort: a failure only means orphan
 * recovery is unavailable, not that the session is broken.
 */
export function writePidfile(
  kind: ChildKind,
  pid: number,
  path = pidfilePath(kind),
): void {
  try {
    writeFileSync(path, String(pid), "utf8");
  } catch {
    // Best effort, as above.
  }
}

/** Clear the pid record. Idempotent. */
export function removePidfile(kind: ChildKind, path = pidfilePath(kind)): void {
  try {
    rmSync(path, { force: true });
  } catch {
    // Idempotent by contract.
  }
}
