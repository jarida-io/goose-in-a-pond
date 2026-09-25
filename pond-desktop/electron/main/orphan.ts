// Reaps children a hard-killed shell left running: POSIX doesn't kill them with the parent.
// A pid is killed only if alive AND still our kind of process, so a reused pid is spared.
// TODO: drop this once the children exit on stdin EOF themselves (a missing watcher).

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

/** Our voice child: `pond-server` + `chat`. Flags are ignored: older shells used others. */
export function cmdlineIsVoiceChild(cmdline: string): boolean {
  return (
    cmdline.includes("pond-server") &&
    cmdline.split(/\s+/).some((tok) => tok === "chat")
  );
}

/** Our sidecar: `pond-server` + `serve`. Not keyed on --port, which may fall back past 4000. */
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

/** A stable per-user token, so pidfiles from different accounts on one host never collide. */
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
 * Parse a pidfile; positive integers only. `process.kill(0)` signals our whole process
 * group and a negative pid signals another group, so a junk file must never yield those.
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
 * `true`: alive and this kind, safe to kill. `false`: gone or a reused pid, record stale.
 * `null`: unknown, so keep the record. Liveness goes first: it spawns no `ps`.
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

/** Kill the child the pidfile names if it is alive and this kind, then clear the file. */
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
      // Unknown: keep the pidfile, the only record of an orphan that may hold the mic.
      deps.warn(
        `could not determine the status of ${kind.label} pidfile pid ${pid}; keeping the pidfile so a later launch can retry recovery`,
      );
  }
}

/** The real implementations, for the main process. */
export const realOrphanDeps: OrphanDeps = {
  isAlive(pid) {
    try {
      // Signal 0 only checks; EPERM means alive but not ours.
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
      // `ps` failed to spawn, or the pid vanished since the liveness check: indeterminate.
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

/** Record the child's pid. Best effort: failure only loses orphan recovery. */
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
