// Quitting, and the health loop that must stop when we do. releaseChildren (sync kills) is
// safe from any exit hook; releaseUi's Electron APIs are not, so keep them apart.

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
 * Watch the server and restart it with exponential backoff. `stopped` is re-checked after
 * every await: a quit can land during healthCheck or ensureRunning.
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
   * Kill both children; idempotent, sync, safe from any exit hook. Stops the health loop
   * first (no respawn), and kills voice before the server (mic and speaker free first).
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
