import { describe, it, expect, vi } from "vitest";
import {
  createHealthLoop,
  createTeardown,
  type HealthLoopDeps,
} from "./lifecycle";

/** Timers fired by hand, not vi's fake timers, so a quit can land in the gap after an await. */
function manualTimers() {
  const pending = new Map<number, () => void>();
  let next = 1;
  return {
    cleared: [] as number[],
    setTimer(fn: () => void): number {
      const id = next++;
      pending.set(id, fn);
      return id;
    },
    clearTimer(handle: unknown): void {
      this.cleared.push(handle as number);
      pending.delete(handle as number);
    },
    /** Fire every timer currently queued, then let microtasks drain. */
    async fire(): Promise<void> {
      const due = [...pending.entries()];
      pending.clear();
      for (const [, fn] of due) fn();
      await Promise.resolve();
      await Promise.resolve();
      await Promise.resolve();
    },
    get queued(): number {
      return pending.size;
    },
  };
}

function loopDeps(over: Partial<HealthLoopDeps> = {}) {
  const timers = manualTimers();
  const deps: HealthLoopDeps = {
    healthCheck: vi.fn().mockResolvedValue(true),
    ensureRunning: vi.fn().mockResolvedValue("http://127.0.0.1:4000"),
    onStatus: vi.fn(),
    onStarting: vi.fn(),
    backoffSeconds: () => 5,
    log: { info() {}, warn() {} },
    setTimer: (fn) => timers.setTimer(fn),
    clearTimer: (h) => timers.clearTimer(h),
    ...over,
  };
  return { deps, timers };
}

describe("createHealthLoop", () => {
  it("reports a transition to the tray and the renderer only when it changes", async () => {
    const { deps, timers } = loopDeps();
    const loop = createHealthLoop(deps);
    loop.start();
    await timers.fire();
    await timers.fire();
    expect(deps.onStatus).toHaveBeenCalledTimes(1);
    expect(deps.onStatus).toHaveBeenCalledWith(true);
  });

  it("tries to recover a server that stops answering", async () => {
    const { deps, timers } = loopDeps({
      healthCheck: vi.fn().mockResolvedValue(false),
    });
    const loop = createHealthLoop(deps);
    loop.start();
    await timers.fire();
    expect(deps.ensureRunning).toHaveBeenCalledTimes(1);
    expect(deps.onStarting).toHaveBeenCalled();
  });

  it("stops scheduling once it has been stopped, so a queued tick cannot respawn the server", async () => {
    const { deps, timers } = loopDeps({
      healthCheck: vi.fn().mockResolvedValue(false),
    });
    const loop = createHealthLoop(deps);
    loop.start();
    loop.stop();
    await timers.fire();
    expect(deps.ensureRunning).not.toHaveBeenCalled();
    expect(timers.queued).toBe(0);
  });

  it("cancels the tick it already had pending", () => {
    const { deps, timers } = loopDeps();
    const loop = createHealthLoop(deps);
    loop.start();
    loop.stop();
    expect(timers.cleared).toHaveLength(1);
  });

  it("abandons a tick when the shell quits mid-check, rather than recovering into a teardown", async () => {
    let quit = () => {};
    const { deps, timers } = loopDeps({
      healthCheck: vi.fn(async () => {
        quit();
        return false;
      }),
    });
    const loop = createHealthLoop(deps);
    quit = () => loop.stop();
    loop.start();
    await timers.fire();
    expect(deps.ensureRunning).not.toHaveBeenCalled();
  });

  it("is safe to stop twice", () => {
    const { deps } = loopDeps();
    const loop = createHealthLoop(deps);
    loop.start();
    loop.stop();
    expect(() => loop.stop()).not.toThrow();
  });
});

function teardownDeps() {
  const order: string[] = [];
  return {
    order,
    deps: {
      stopHealthLoop: vi.fn(() => void order.push("health-loop")),
      killVoice: vi.fn(() => void order.push("voice")),
      shutdownServer: vi.fn(() => void order.push("server")),
      releaseUi: vi.fn(() => void order.push("ui")),
      log: { info() {}, warn() {} },
    },
  };
}

describe("createTeardown", () => {
  it("stops the health loop, then the voice child, then the server", () => {
    const { deps, order } = teardownDeps();
    createTeardown(deps).releaseChildren();
    expect(order).toEqual(["health-loop", "voice", "server"]);
  });

  // before-quit, will-quit and the exit hook can all fire on one quit.
  it("releases the children once however many quit paths fire", () => {
    const { deps } = teardownDeps();
    const t = createTeardown(deps);
    t.releaseChildren();
    t.releaseChildren();
    t.releaseChildren();
    expect(deps.shutdownServer).toHaveBeenCalledTimes(1);
  });

  it("kills both children without touching the UI, for the exit hook", () => {
    const { deps } = teardownDeps();
    createTeardown(deps).releaseChildren();
    expect(deps.killVoice).toHaveBeenCalled();
    expect(deps.shutdownServer).toHaveBeenCalled();
    expect(deps.releaseUi).not.toHaveBeenCalled();
  });

  it("releases the UI once, separately from the children", () => {
    const { deps } = teardownDeps();
    const t = createTeardown(deps);
    t.releaseUi();
    t.releaseUi();
    expect(deps.releaseUi).toHaveBeenCalledTimes(1);
    expect(deps.shutdownServer).not.toHaveBeenCalled();
  });
});
