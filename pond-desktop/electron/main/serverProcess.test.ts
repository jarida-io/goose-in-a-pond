import { describe, it, expect, vi } from "vitest";
import {
  resolveServerUrl,
  resolveServerBinary,
  recoveryBackoffSeconds,
  ServerProcess,
  SPAWNED_POLL_ATTEMPTS,
  PARENT_MANAGED_POLL_ATTEMPTS,
  type ServerDeps,
} from "./serverProcess";
import { type OrphanDeps } from "./orphan";

describe("resolveServerUrl", () => {
  it("uses port 4000 and self-managed mode when GIAP_SERVER_PORT is unset", () => {
    expect(resolveServerUrl(undefined)).toEqual({
      url: "http://127.0.0.1:4000",
      parentManaged: false,
    });
  });

  it("adopts the parent's port and refuses to spawn", () => {
    expect(resolveServerUrl("8080")).toEqual({
      url: "http://127.0.0.1:8080",
      parentManaged: true,
    });
  });

  it("treats a blank value as unset", () => {
    expect(resolveServerUrl("").parentManaged).toBe(false);
    expect(resolveServerUrl("   ").parentManaged).toBe(false);
  });
});

describe("resolveServerBinary", () => {
  const base = {
    isPackaged: false,
    resourcesPath: "/App.app/Contents/Resources",
    repoRoot: "/repo",
    platform: "darwin" as NodeJS.Platform,
  };

  it("prefers POND_SERVER_BIN, even inside a packaged app", () => {
    const path = resolveServerBinary({
      ...base,
      isPackaged: true,
      override: "/custom/pond-server",
      exists: (p) => p === "/custom/pond-server",
    });
    expect(path).toBe("/custom/pond-server");
  });

  it("ignores an override that does not exist", () => {
    const path = resolveServerBinary({
      ...base,
      override: "/gone/pond-server",
      exists: (p) => p === "/repo/target/release/pond-server",
    });
    expect(path).toBe("/repo/target/release/pond-server");
  });

  it("looks only in Resources when packaged", () => {
    const seen: string[] = [];
    const path = resolveServerBinary({
      ...base,
      isPackaged: true,
      exists: (p) => {
        seen.push(p);
        return p === "/App.app/Contents/Resources/pond-server";
      },
    });
    expect(path).toBe("/App.app/Contents/Resources/pond-server");
    expect(seen).toEqual(["/App.app/Contents/Resources/pond-server"]);
  });

  it("returns null rather than a dev path when packaged and absent", () => {
    expect(
      resolveServerBinary({ ...base, isPackaged: true, exists: () => false }),
    ).toBe(null);
  });

  it("prefers a release build over a debug one in dev", () => {
    const path = resolveServerBinary({ ...base, exists: () => true });
    expect(path).toBe("/repo/target/release/pond-server");
  });

  it("falls back to the staged sidecar in dev", () => {
    const path = resolveServerBinary({
      ...base,
      exists: (p) => p === "/repo/pond-desktop/resources/pond-server",
    });
    expect(path).toBe("/repo/pond-desktop/resources/pond-server");
  });

  it("uses the .exe name on Windows", () => {
    const path = resolveServerBinary({
      ...base,
      platform: "win32",
      exists: (p) => p.endsWith("pond-server.exe"),
    });
    expect(path).toContain("pond-server.exe");
  });

  it("returns null when nothing is anywhere", () => {
    expect(resolveServerBinary({ ...base, exists: () => false })).toBe(null);
  });
});

describe("recoveryBackoffSeconds", () => {
  it("starts at five seconds and doubles", () => {
    expect(recoveryBackoffSeconds(1)).toBe(5);
    expect(recoveryBackoffSeconds(2)).toBe(10);
    expect(recoveryBackoffSeconds(3)).toBe(20);
  });

  it("caps at five minutes so a dead server is not hammered forever", () => {
    expect(recoveryBackoffSeconds(7)).toBe(300);
    expect(recoveryBackoffSeconds(50)).toBe(300);
  });

  it("is zero when nothing has failed", () => {
    expect(recoveryBackoffSeconds(0)).toBe(0);
    expect(recoveryBackoffSeconds(-1)).toBe(0);
  });
});

function deps(over: Partial<ServerDeps> = {}): ServerDeps {
  return {
    lookup: {
      isPackaged: false,
      resourcesPath: "/res",
      repoRoot: "/repo",
      platform: "darwin",
      exists: () => true,
    },
    env: {},
    fetchFn: vi
      .fn()
      .mockRejectedValue(new Error("refused")) as unknown as typeof fetch,
    spawnFn: vi
      .fn()
      .mockReturnValue({ exitCode: null, kill: vi.fn() }) as never,
    sleep: () => Promise.resolve(),
    // Required: unstubbed, the suite reads the real pidfile and can SIGKILL your dev pond-server.
    orphanDeps: stubOrphanDeps(),
    writePid: vi.fn(),
    removePid: vi.fn(),
    ...over,
  };
}

/** An OrphanDeps that touches nothing real and finds no orphan. */
function stubOrphanDeps(): OrphanDeps {
  return {
    isAlive: vi.fn().mockReturnValue(false),
    commandLine: vi.fn().mockReturnValue(null),
    kill: vi.fn(),
    readFile: vi.fn().mockReturnValue(null),
    removeFile: vi.fn(),
    warn: vi.fn(),
  };
}

/** A child that can actually end; stdio is "inherit", so there are no streams to fake. */
class FakeChild {
  exitCode: number | null = null;
  signalCode: NodeJS.Signals | null = null;
  readonly pid = 4242;
  readonly signals: (NodeJS.Signals | undefined)[] = [];
  /** Set when kill() should NOT end the process, modelling a wedged child. */
  ignoreSignals = false;

  kill = (signal?: NodeJS.Signals): boolean => {
    this.signals.push(signal);
    if (this.ignoreSignals && signal !== "SIGKILL") return true;
    this.signalCode = signal ?? "SIGTERM";
    return true;
  };

  /** End the child the way a normal exit would. */
  exit(code: number): void {
    this.exitCode = code;
  }
}

/** A spawnFn that hands out the given children in order. */
function spawnsInOrder(...children: FakeChild[]) {
  let n = 0;
  return vi.fn(() => children[Math.min(n++, children.length - 1)]) as never;
}

/** A fetch that refuses `failures` times, then reports healthy. */
function fetchHealthyAfter(failures: number) {
  let n = 0;
  return vi.fn(async () => {
    if (n++ < failures) throw new Error("refused");
    return { ok: true } as Response;
  }) as unknown as typeof fetch;
}

describe("ServerProcess", () => {
  it("attaches to an already-healthy server without spawning", async () => {
    const d = deps({
      fetchFn: vi.fn().mockResolvedValue({ ok: true }) as never,
    });
    const s = new ServerProcess(d);
    expect(await s.ensureRunning()).toBe("http://127.0.0.1:4000");
    expect(d.spawnFn).not.toHaveBeenCalled();
  });

  it("spawns with the serve argv when nothing is listening", async () => {
    const d = deps({ fetchFn: fetchHealthyAfter(1) });
    await new ServerProcess(d).ensureRunning();
    expect(d.spawnFn).toHaveBeenCalledWith(
      "/repo/target/release/pond-server",
      ["serve", "--port", "4000"],
      expect.objectContaining({ cwd: "/repo" }),
    );
  });

  // Two pond-servers fighting for port 4000 show up as a blank window, not an error.
  it("never spawns in parent-managed mode", async () => {
    const d = deps({
      env: { GIAP_SERVER_PORT: "8080" },
      fetchFn: fetchHealthyAfter(3),
    });
    const s = new ServerProcess(d);
    expect(s.parentManaged).toBe(true);
    expect(await s.ensureRunning()).toBe("http://127.0.0.1:8080");
    expect(d.spawnFn).not.toHaveBeenCalled();
  });

  it("gives a server it spawned itself the same cold-start budget as a parent-managed one", async () => {
    expect(SPAWNED_POLL_ATTEMPTS).toBe(PARENT_MANAGED_POLL_ATTEMPTS);

    // 100 refusals is 50 s: inside the budget, far past a 10 s one.
    const parent = deps({
      env: { GIAP_SERVER_PORT: "8080" },
      fetchFn: fetchHealthyAfter(100),
    });
    await expect(new ServerProcess(parent).ensureRunning()).resolves.toBe(
      "http://127.0.0.1:8080",
    );

    const own = deps({ fetchFn: fetchHealthyAfter(100) });
    await expect(new ServerProcess(own).ensureRunning()).resolves.toBe(
      "http://127.0.0.1:4000",
    );
  });

  it("stops waiting as soon as a server it spawned exits, rather than polling out the budget", async () => {
    const child = new FakeChild();
    const d = deps({ spawnFn: spawnsInOrder(child) });
    const s = new ServerProcess(d);
    const run = s.ensureRunning();
    child.exit(2);
    await expect(run).rejects.toThrow(
      /exited before it was ready \(exit code 2\)/,
    );
  });

  it("names the budget it actually waited in the error it throws", async () => {
    const d = deps({ fetchFn: fetchHealthyAfter(Number.MAX_SAFE_INTEGER) });
    await expect(new ServerProcess(d).ensureRunning()).rejects.toThrow(
      new RegExp(`within ${(SPAWNED_POLL_ATTEMPTS * 500) / 1_000} s`),
    );
  });

  it("kills a live but unhealthy child rather than spawning a rival for its port", async () => {
    const wedged = new FakeChild();
    const replacement = new FakeChild();
    const d = deps({
      fetchFn: fetchHealthyAfter(Number.MAX_SAFE_INTEGER),
      spawnFn: spawnsInOrder(wedged, replacement),
    });
    const s = new ServerProcess(d);

    await expect(s.ensureRunning()).rejects.toThrow(/did not become healthy/);
    expect(d.spawnFn).toHaveBeenCalledTimes(1);

    // The health loop's next tick must replace the wedged child, not abandon it.
    await expect(s.ensureRunning()).rejects.toThrow(/did not become healthy/);
    expect(wedged.signals).toContain("SIGTERM");
    expect(d.spawnFn).toHaveBeenCalledTimes(2);
  });

  it("adopts the server it already spawned once that server answers", async () => {
    const child = new FakeChild();
    const d = deps({
      fetchFn: fetchHealthyAfter(Number.MAX_SAFE_INTEGER),
      spawnFn: spawnsInOrder(child),
    });
    const s = new ServerProcess(d);
    await expect(s.ensureRunning()).rejects.toThrow(/did not become healthy/);

    // Now it answers, so the next call must neither kill nor spawn.
    const healthy = new ServerProcess(
      deps({
        fetchFn: vi.fn().mockResolvedValue({ ok: true }) as never,
        spawnFn: d.spawnFn,
      }),
    );
    await healthy.ensureRunning();
    expect(d.spawnFn).toHaveBeenCalledTimes(1);
    expect(child.signals).toHaveLength(0);
  });

  it("escalates to SIGKILL when a wedged child ignores SIGTERM", async () => {
    const wedged = new FakeChild();
    wedged.ignoreSignals = true;
    const d = deps({
      fetchFn: fetchHealthyAfter(Number.MAX_SAFE_INTEGER),
      spawnFn: spawnsInOrder(wedged, new FakeChild()),
    });
    const s = new ServerProcess(d);
    await expect(s.ensureRunning()).rejects.toThrow();
    await expect(s.ensureRunning()).rejects.toThrow();
    expect(wedged.signals).toContain("SIGTERM");
    expect(wedged.signals).toContain("SIGKILL");
  });

  it("treats a child killed by a signal as ended, not as a live child to kill again", async () => {
    const killed = new FakeChild();
    const d = deps({
      fetchFn: fetchHealthyAfter(Number.MAX_SAFE_INTEGER),
      spawnFn: spawnsInOrder(killed, new FakeChild()),
    });
    const s = new ServerProcess(d);
    await expect(s.ensureRunning()).rejects.toThrow();

    killed.signalCode = "SIGKILL";
    killed.signals.length = 0;
    await expect(s.ensureRunning()).rejects.toThrow();
    expect(killed.signals).toHaveLength(0);
  });

  it("clears a child that exited on its own without killing anything", async () => {
    const exited = new FakeChild();
    const d = deps({
      fetchFn: fetchHealthyAfter(Number.MAX_SAFE_INTEGER),
      spawnFn: spawnsInOrder(exited, new FakeChild()),
    });
    const s = new ServerProcess(d);
    await expect(s.ensureRunning()).rejects.toThrow();

    exited.exit(0);
    exited.signals.length = 0;
    await expect(s.ensureRunning()).rejects.toThrow();
    expect(exited.signals).toHaveLength(0);
    expect(d.spawnFn).toHaveBeenCalledTimes(2);
  });

  it("reaps a sidecar orphaned by a previous run before asking whether one is listening", async () => {
    const order: string[] = [];
    const orphanDeps = stubOrphanDeps();
    orphanDeps.readFile = vi.fn(() => {
      order.push("reap");
      return null;
    });
    const d = deps({
      orphanDeps,
      fetchFn: vi.fn(async () => {
        order.push("health");
        return { ok: true } as Response;
      }) as unknown as typeof fetch,
    });
    await new ServerProcess(d).ensureRunning();
    expect(order).toEqual(["reap", "health"]);
  });

  it("never reaps in parent-managed mode, because the parent is itself a pond-server serve", async () => {
    const orphanDeps = stubOrphanDeps();
    const d = deps({
      orphanDeps,
      env: { GIAP_SERVER_PORT: "8080" },
      fetchFn: vi.fn().mockResolvedValue({ ok: true }) as never,
    });
    await new ServerProcess(d).ensureRunning();
    expect(orphanDeps.readFile).not.toHaveBeenCalled();
  });

  // A recovery ten minutes in must not kill the server we adopted at startup.
  it("reaps only once, however many recoveries run", async () => {
    const orphanDeps = stubOrphanDeps();
    const d = deps({ orphanDeps, fetchFn: fetchHealthyAfter(1) });
    const s = new ServerProcess(d);
    await s.ensureRunning();
    await s.ensureRunning();
    await s.ensureRunning();
    expect(orphanDeps.readFile).toHaveBeenCalledTimes(1);
  });

  it("records the spawned pid so a later launch can reap it", async () => {
    const child = new FakeChild();
    const d = deps({
      fetchFn: fetchHealthyAfter(1),
      spawnFn: spawnsInOrder(child),
    });
    await new ServerProcess(d).ensureRunning();
    expect(d.writePid).toHaveBeenCalledWith(child.pid);
  });

  it("clears the pid record when it shuts the server down", async () => {
    const d = deps({
      fetchFn: fetchHealthyAfter(1),
      spawnFn: spawnsInOrder(new FakeChild()),
    });
    const s = new ServerProcess(d);
    await s.ensureRunning();
    s.shutdown();
    expect(d.removePid).toHaveBeenCalled();
  });

  it("adopts the port pond-server actually bound when it fell back past 4000", async () => {
    const onUrlChanged = vi.fn();
    const d = deps({
      fetchFn: vi.fn(async (input: string) => {
        // Only 4001 answers, the way a fallback bind behaves.
        if (String(input).includes("4001")) return { ok: true } as Response;
        throw new Error("refused");
      }) as unknown as typeof fetch,
      spawnFn: spawnsInOrder(new FakeChild()),
      readPortFile: () => ({ port: 4001, mtimeMs: Date.now() + 1_000 }),
      onUrlChanged,
    });
    const s = new ServerProcess(d);
    expect(await s.ensureRunning()).toBe("http://127.0.0.1:4001");
    expect(s.url).toBe("http://127.0.0.1:4001");
    expect(onUrlChanged).toHaveBeenCalledWith("http://127.0.0.1:4001");
  });

  // A port file from yesterday's run must never outrank today's spawn.
  it("ignores a port file written before the server it just started", async () => {
    const d = deps({
      fetchFn: fetchHealthyAfter(1),
      spawnFn: spawnsInOrder(new FakeChild()),
      readPortFile: () => ({ port: 4001, mtimeMs: 0 }),
    });
    const s = new ServerProcess(d);
    expect(await s.ensureRunning()).toBe("http://127.0.0.1:4000");
  });

  it("stays put when the port file names the port it already assumed", async () => {
    const onUrlChanged = vi.fn();
    const d = deps({
      fetchFn: fetchHealthyAfter(1),
      spawnFn: spawnsInOrder(new FakeChild()),
      readPortFile: () => ({ port: 4000, mtimeMs: Date.now() + 1_000 }),
      onUrlChanged,
    });
    await new ServerProcess(d).ensureRunning();
    expect(onUrlChanged).not.toHaveBeenCalled();
  });

  it("refuses to spawn once the shell has begun quitting", async () => {
    const d = deps({ fetchFn: fetchHealthyAfter(1) });
    const s = new ServerProcess(d);
    s.shutdown();
    await expect(s.ensureRunning()).rejects.toThrow(/quitting/);
    expect(d.spawnFn).not.toHaveBeenCalled();
  });

  it("reports a missing binary rather than spawning nothing quietly", async () => {
    const d = deps({ lookup: { ...deps().lookup, exists: () => false } });
    await expect(new ServerProcess(d).ensureRunning()).rejects.toThrow(
      /no binary found/,
    );
  });

  it("serialises concurrent recovery into a single spawn", async () => {
    const d = deps({ fetchFn: fetchHealthyAfter(1) });
    const s = new ServerProcess(d);
    await Promise.all([
      s.ensureRunning(),
      s.ensureRunning(),
      s.ensureRunning(),
    ]);
    expect(d.spawnFn).toHaveBeenCalledTimes(1);
  });

  it("keeps serving later calls after one fails", async () => {
    const d = deps({ lookup: { ...deps().lookup, exists: () => false } });
    const s = new ServerProcess(d);
    await expect(s.ensureRunning()).rejects.toThrow();
    await expect(s.ensureRunning()).rejects.toThrow();
  });

  it("kills only a server it spawned itself", async () => {
    const kill = vi.fn();
    const d = deps({
      fetchFn: fetchHealthyAfter(1),
      spawnFn: vi.fn().mockReturnValue({ exitCode: null, kill }) as never,
    });
    const s = new ServerProcess(d);
    await s.ensureRunning();
    s.shutdown();
    expect(kill).toHaveBeenCalled();

    const parent = new ServerProcess(
      deps({
        env: { GIAP_SERVER_PORT: "8080" },
        fetchFn: vi.fn().mockResolvedValue({ ok: true }) as never,
      }),
    );
    await parent.ensureRunning();
    expect(() => parent.shutdown()).not.toThrow();
  });

  it("treats a non-ok health response as unhealthy", async () => {
    const d = deps({
      fetchFn: vi.fn().mockResolvedValue({ ok: false }) as never,
    });
    expect(await new ServerProcess(d).healthCheck()).toBe(false);
  });
});
