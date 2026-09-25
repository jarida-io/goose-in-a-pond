import { describe, it, expect, vi } from "vitest";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { readFileSync, rmSync, existsSync } from "node:fs";
import {
  cmdlineIsVoiceChild,
  cmdlineIsServerChild,
  SERVER_CHILD,
  readPidfile,
  pidIsChildOfKind,
  reapPidfileOrphan,
  pidfilePath,
  writePidfile,
  removePidfile,
  realOrphanDeps,
  type OrphanDeps,
  VOICE_CHILD,
} from "./orphan";

function deps(over: Partial<OrphanDeps> = {}): OrphanDeps {
  return {
    isAlive: vi.fn().mockReturnValue(false),
    commandLine: vi.fn().mockReturnValue(null),
    kill: vi.fn(),
    readFile: vi.fn().mockReturnValue(null),
    removeFile: vi.fn(),
    warn: vi.fn(),
    ...over,
  };
}

describe("cmdlineIsServerChild", () => {
  it("matches a pond-server sidecar", () => {
    // The exact production invocation.
    expect(cmdlineIsServerChild("/opt/app/pond-server serve --port 4000")).toBe(
      true,
    );
    // A macOS bundle sidecar path.
    expect(
      cmdlineIsServerChild(
        "/Applications/Goose In A Pond.app/Contents/Resources/pond-server serve --port 4000",
      ),
    ).toBe(true);
    // Not keyed on --port: a sidecar that fell back past 4000 is still ours.
    expect(cmdlineIsServerChild("/opt/app/pond-server serve --port 4001")).toBe(
      true,
    );
    expect(cmdlineIsServerChild("target/debug/pond-server serve")).toBe(true);
  });

  it("does not match the voice child or unrelated processes", () => {
    expect(
      cmdlineIsServerChild("/opt/app/pond-server chat --voice --json-events"),
    ).toBe(false);
    expect(cmdlineIsServerChild("/usr/bin/serve --port 4000")).toBe(false);
    expect(cmdlineIsServerChild("pond-server-helper serveries")).toBe(false);
    expect(cmdlineIsServerChild("")).toBe(false);
  });
});

describe("the two child kinds", () => {
  it("never claim each other's processes", () => {
    const sidecar = "/opt/app/pond-server serve --port 4000";
    const voice = "/opt/app/pond-server chat --voice --session-id abc";
    expect(SERVER_CHILD.matches(sidecar)).toBe(true);
    expect(VOICE_CHILD.matches(sidecar)).toBe(false);
    expect(VOICE_CHILD.matches(voice)).toBe(true);
    expect(SERVER_CHILD.matches(voice)).toBe(false);
  });

  it("keep their pidfiles apart, so reaping one never touches the other", () => {
    expect(pidfilePath(SERVER_CHILD)).not.toBe(pidfilePath(VOICE_CHILD));
    expect(pidfilePath(SERVER_CHILD).split("/").pop()).toMatch(
      /^giap-server-[A-Za-z0-9_]+\.pid$/,
    );
  });
});

describe("cmdlineIsVoiceChild", () => {
  it("matches a voice child", () => {
    // The exact production invocation.
    expect(
      cmdlineIsVoiceChild(
        "/opt/app/pond-server chat --voice --json-events --session-id abc",
      ),
    ).toBe(true);
    // A macOS bundle path.
    expect(
      cmdlineIsVoiceChild(
        "/Applications/Goose In A Pond.app/Contents/MacOS/pond-server chat --voice",
      ),
    ).toBe(true);
    // A pre-upgrade shell's flags still match: the matcher ignores flags.
    expect(
      cmdlineIsVoiceChild(
        "/opt/app/pond-server chat --input whisper --json-events --session-id abc",
      ),
    ).toBe(true);
  });

  it("does not match the dashboard server or unrelated processes", () => {
    // `serve` must never be reaped as a voice child.
    expect(cmdlineIsVoiceChild("/opt/app/pond-server serve --port 4000")).toBe(
      false,
    );
    // `chat` as a substring of another token is not the subcommand.
    expect(cmdlineIsVoiceChild("/usr/bin/pond-server-chatterbox serve")).toBe(
      false,
    );
    expect(cmdlineIsVoiceChild("/usr/bin/node /some/other/app.js")).toBe(false);
    expect(cmdlineIsVoiceChild("")).toBe(false);
    // `chat` without pond-server may be a reused pid running something else.
    expect(cmdlineIsVoiceChild("/usr/bin/irc-client chat")).toBe(false);
  });
});

describe("readPidfile", () => {
  it("parses a valid pid, including surrounding whitespace", () => {
    expect(readPidfile("  12345\n")).toBe(12345);
  });

  it("rejects junk rather than producing a wild kill target", () => {
    expect(readPidfile("not-a-pid")).toBe(null);
    expect(readPidfile("")).toBe(null);
    expect(readPidfile("   ")).toBe(null);
    expect(readPidfile(null)).toBe(null);
    expect(readPidfile("12.5")).toBe(null);
    expect(readPidfile("1e5")).toBe(null);
  });

  // kill(0) signals our whole process group and kill(-n) group n: the shell would kill itself.
  it("rejects zero and negative pids", () => {
    expect(readPidfile("0")).toBe(null);
    expect(readPidfile("  0  ")).toBe(null);
    expect(readPidfile("-1")).toBe(null);
    expect(readPidfile("-4242")).toBe(null);
  });
});

describe("pidIsChildOfKind", () => {
  it("confirms a live pond-server chat process", () => {
    const d = deps({
      isAlive: vi.fn().mockReturnValue(true),
      commandLine: vi
        .fn()
        .mockReturnValue("/opt/app/pond-server chat --voice\n"),
    });
    expect(pidIsChildOfKind(4242, VOICE_CHILD, d)).toBe(true);
  });

  it("reports a dead pid without spawning ps at all", () => {
    const d = deps({ isAlive: vi.fn().mockReturnValue(false) });
    expect(pidIsChildOfKind(4242, VOICE_CHILD, d)).toBe(false);
    // Stale pidfiles are the common case; spawning `ps` fails on a memory-pressured board.
    expect(d.commandLine).not.toHaveBeenCalled();
  });

  it("reports a live but unrelated process as not ours", () => {
    const d = deps({
      isAlive: vi.fn().mockReturnValue(true),
      commandLine: vi.fn().mockReturnValue("/usr/bin/node server.js"),
    });
    expect(pidIsChildOfKind(4242, VOICE_CHILD, d)).toBe(false);
  });

  it("stays indeterminate when the command line cannot be read", () => {
    const d = deps({
      isAlive: vi.fn().mockReturnValue(true),
      commandLine: vi.fn().mockReturnValue(null),
    });
    expect(pidIsChildOfKind(4242, VOICE_CHILD, d)).toBe(null);
  });

  it("stays indeterminate when the liveness check itself fails", () => {
    const d = deps({
      isAlive: vi.fn().mockImplementation(() => {
        throw new Error("EMFILE");
      }),
    });
    expect(pidIsChildOfKind(4242, VOICE_CHILD, d)).toBe(null);
  });

  it("never confirms a non-positive pid", () => {
    expect(pidIsChildOfKind(0, VOICE_CHILD, deps())).toBe(false);
    expect(pidIsChildOfKind(-1, VOICE_CHILD, deps())).toBe(false);
  });
});

describe("reapPidfileOrphan", () => {
  const PATH = "/tmp/giap-test.pid";

  it("does nothing when there is no pidfile", () => {
    const d = deps();
    reapPidfileOrphan(VOICE_CHILD, d, PATH);
    expect(d.kill).not.toHaveBeenCalled();
    expect(d.removeFile).not.toHaveBeenCalled();
  });

  it("kills a confirmed orphan and clears the file", () => {
    const d = deps({
      readFile: vi.fn().mockReturnValue("4242"),
      isAlive: vi.fn().mockReturnValue(true),
      commandLine: vi.fn().mockReturnValue("/opt/app/pond-server chat --voice"),
    });
    reapPidfileOrphan(VOICE_CHILD, d, PATH);
    expect(d.kill).toHaveBeenCalledWith(4242);
    expect(d.removeFile).toHaveBeenCalledWith(PATH);
  });

  it("clears a stale record without killing anything", () => {
    const d = deps({
      readFile: vi.fn().mockReturnValue("4242"),
      isAlive: vi.fn().mockReturnValue(false),
    });
    reapPidfileOrphan(VOICE_CHILD, d, PATH);
    expect(d.kill).not.toHaveBeenCalled();
    expect(d.removeFile).toHaveBeenCalledWith(PATH);
  });

  // The pidfile is the only record of an orphan that may still hold the microphone.
  it("keeps the pidfile when liveness is indeterminate", () => {
    const d = deps({
      readFile: vi.fn().mockReturnValue("4242"),
      isAlive: vi.fn().mockReturnValue(true),
      commandLine: vi.fn().mockReturnValue(null),
    });
    reapPidfileOrphan(VOICE_CHILD, d, PATH);
    expect(d.kill).not.toHaveBeenCalled();
    expect(d.removeFile).not.toHaveBeenCalled();
    expect(d.warn).toHaveBeenCalledWith(
      expect.stringContaining("retry recovery"),
    );
  });

  it("never kills on a malformed pidfile", () => {
    const d = deps({ readFile: vi.fn().mockReturnValue("0") });
    reapPidfileOrphan(VOICE_CHILD, d, PATH);
    expect(d.kill).not.toHaveBeenCalled();
  });
});

describe("the real pidfile primitives", () => {
  const PATH = join(tmpdir(), `giap-pidfile-test-${process.pid}.pid`);

  it("round-trips write, read and remove, and remove is idempotent", () => {
    rmSync(PATH, { force: true });
    writePidfile(VOICE_CHILD, 4242, PATH);
    expect(readPidfile(readFileSync(PATH, "utf8"))).toBe(4242);
    removePidfile(VOICE_CHILD, PATH);
    expect(existsSync(PATH)).toBe(false);
    // A second remove is a no-op, never a throw.
    expect(() => removePidfile(VOICE_CHILD, PATH)).not.toThrow();
  });

  it("scopes the path per user, with a filesystem-safe name", () => {
    const name = pidfilePath(VOICE_CHILD).split("/").pop() ?? "";
    expect(name).toMatch(/^giap-voice-child-[A-Za-z0-9_]+\.pid$/);
  });

  it("reports this very process as alive, and pid 1 as not ours", () => {
    expect(realOrphanDeps.isAlive(process.pid)).toBe(true);
    // pid 1 exists on every POSIX host but is launchd/init, never our child.
    expect(pidIsChildOfKind(1, VOICE_CHILD, realOrphanDeps)).toBe(false);
  });
});
