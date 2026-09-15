// Quitting, and the health loop that must stop when we do.
//
// Extracted from index.ts because both of these had defects that no test could
// reach while they lived inside the module that calls app.whenReady():
//
//   * The health loop kept no handle on its own timer and never consulted the
//     quitting flag, so a tick scheduled before quit could land AFTER the
//     server was shut down and start a fresh sidecar during teardown -- one
//     that nothing would ever shut down again.
//
//   * Only `before-quit` took the children down. The Tauri original ran the
//     same teardown on RunEvent::ExitRequested AND RunEvent::Exit; the port
//     kept the first and dropped the second, and `process.on("exit")` was
//     left killing the voice child alone.
//
// The split between releaseChildren and releaseUi is load-bearing: killing a
// child is a synchronous syscall and is legal from any exit hook, while the
// Electron APIs behind releaseUi are not, and calling them from
// `process.on("exit")` can throw AFTER the children are dead and mask the
// teardown that mattered.

/** A timer handle, opaque so tests can hand back whatever they like. */
type TimerHandle = unknown;

export interface HealthLoopDeps {
  healthCheck: () => Promise<boolean>;
  ensureRunning: () => Promise<string>;
  /** Report a transition to the renderer and the tray. */
  onStatus: (healthy: boolean) => void;
  /** Tell the renderer a recovery is under way. */
  onStarting: () => void;
  backoffSeconds: (consecutiveFailures: number) => number;
  log: { info(m: string): void; warn(m: string): void };
  setTimer?: (fn: () => void, ms: number) => TimerHandle;
  clearTimer?: (handle: TimerHandle) => void;
  /** Gap between checks while the server is answering. */
  healthyIntervalMs?: number;
}

export interface HealthLoop {
  /** Begin watching, after the usual first delay. */
  start(): void;
  /** Stop watching. Idempotent, and cancels any pending tick. */
  stop(): void;
}

const HEALTHY_INTERVAL_MS = 10_000;

/**
 * Watch the server and bring it back when it goes away.
 *
 * Backs off exponentially so a server that cannot start is not hammered, and
 * reports every transition -- when the window is hidden the tray tooltip is the
 * only place this state is visible.
 *
 * The stopped flag is re-checked after every await, not only at the top of a
 * tick: the loop awaits both healthCheck and ensureRunning, and a quit can land
 * in either gap.
 */
export function createHealthLoop(deps: HealthLoopDeps): HealthLoop {
  const setTimer = deps.setTimer ?? ((fn, ms) => setTimeout(fn, ms));
  const clearTimer =
    deps.clearTimer ?? ((h) => clearTimeout(h as NodeJS.Timeout));
  const healthyInterval = deps.healthyIntervalMs ?? HEALTHY_INTERVAL_MS;

  let failures = 0;
  let online: boolean | null = null;
  let stopped = false;
  let timer: TimerHandle = null;

  const schedule = (ms: number): void => {
    if (stopped) return;
    timer = setTimer(() => void tick(), ms);
  };

  const tick = async (): Promise<void> => {
    if (stopped) return;

    const healthy = await deps.healthCheck();
    if (stopped) return;

    if (healthy !== online) {
      online = healthy;
      deps.onStatus(healthy);
    }

    if (healthy) {
      failures = 0;
      schedule(healthyInterval);
      return;
    }

    failures += 1;
    const wait = deps.backoffSeconds(failures);
    deps.log.warn(
      `pond-server is unreachable (attempt ${failures}); retrying in ${wait}s`,
    );
    deps.onStarting();
    try {
      await deps.ensureRunning();
    } catch (e) {
      deps.log.warn(`recovery failed: ${(e as Error).message}`);
    }
    schedule(wait * 1_000);
  };

  return {
    start(): void {
      schedule(healthyInterval);
    },
    stop(): void {
      if (stopped) return;
      stopped = true;
      if (timer !== null) clearTimer(timer);
      timer = null;
      deps.log.info("health loop stopped");
    },
  };
}

export interface TeardownDeps {
  stopHealthLoop: () => void;
  killVoice: () => void;
  shutdownServer: () => void;
  /** Hotkeys, tray: Electron APIs, unsafe from a process exit hook. */
  releaseUi: () => void;
  log: { info(m: string): void; warn(m: string): void };
}

export interface Teardown {
  /**
   * Kill both children. Idempotent, synchronous, safe from any exit hook.
   *
   * The health loop is stopped FIRST so a queued tick cannot respawn the
   * sidecar we are about to kill, and the voice child dies before the server so
   * the microphone and speaker are released first.
   */
  releaseChildren(): void;
  /** Electron-only teardown. Only from before-quit or will-quit. */
  releaseUi(): void;
}

export function createTeardown(deps: TeardownDeps): Teardown {
  let childrenReleased = false;
  let uiReleased = false;

  return {
    releaseChildren(): void {
      if (childrenReleased) return;
      childrenReleased = true;
      deps.stopHealthLoop();
      deps.killVoice();
      deps.shutdownServer();
      deps.log.info("child processes released");
    },
    releaseUi(): void {
      if (uiReleased) return;
      uiReleased = true;
      deps.releaseUi();
    },
  };
}
