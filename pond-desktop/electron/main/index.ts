// The desktop shell. Ordering: the scheme is registered before `whenReady` (no effect after),
// and on quit the voice child dies before the server so the mic and speaker free first.

import { app, BrowserWindow } from "electron";
import { join, resolve } from "node:path";
import { readFileSync, statSync } from "node:fs";
import { homedir } from "node:os";
import { registerAppScheme, serveRendererFrom } from "./protocol";
import { createMainWindow, distRoot } from "./window";
import {
  ServerProcess,
  recoveryBackoffSeconds,
  resolveServerBinary,
} from "./serverProcess";
import { VoiceChildProcess } from "./voice/VoiceChildProcess";
import { registerIpc } from "./ipc";
import { installMenu, setAboutPanel } from "./menu";
import { createTray, setTrayStatus, destroyTray } from "./tray";
import { registerHotkeys, unregisterHotkeys } from "./hotkeys";
import { createHealthLoop, createTeardown } from "./lifecycle";
import { resolveDataDir, readRuntimePort, RUNTIME_PORT_FILE } from "./dataDir";
import type { ShellEvent, ShellEvents } from "../../src/shell/contract";

const log = {
  info: (m: string) => console.log(`[giap] ${m}`),
  warn: (m: string) => console.warn(`[giap] ${m}`),
  debug: (m: string) => {
    if (process.env["GIAP_DEBUG"]) console.debug(`[giap] ${m}`);
  },
};

let win: BrowserWindow | null = null;
/** Set on the way out, so the close handler stops hiding and lets us quit. */
let quitting = false;

/** The repo root, used only in dev to ground the sidecar's cwd. */
const repoRoot = resolve(app.getAppPath(), "..");

function emit<E extends ShellEvent>(name: E, payload?: ShellEvents[E]): void {
  if (!win || win.isDestroyed()) return;
  win.webContents.send(`giap:${name}`, payload);
}

/** The port pond-server says it bound, and when; read from the server's data dir, not userData. */
function readPortFile(): { port: number; mtimeMs: number } | null {
  const file = join(
    resolveDataDir({
      env: process.env,
      home: homedir(),
      platform: process.platform,
    }),
    RUNTIME_PORT_FILE,
  );
  try {
    const port = readRuntimePort(readFileSync(file, "utf8"));
    if (port === null) return null;
    return { port, mtimeMs: statSync(file).mtimeMs };
  } catch {
    // Not written yet, or unreadable. The caller keeps its assumed port.
    return null;
  }
}

const server = new ServerProcess({
  lookup: {
    isPackaged: app.isPackaged,
    resourcesPath: process.resourcesPath,
    repoRoot,
    platform: process.platform,
  },
  readPortFile,
  onUrlChanged: (url) => emit("server-url", url),
  log,
});

const voice = new VoiceChildProcess({
  resolveBinary: () => serverBinaryForVoice(),
  cwd: app.isPackaged ? undefined : repoRoot,
  emit,
  log,
});

/** The voice child runs the sidecar's binary, found by the same lookup so they can't disagree. */
function serverBinaryForVoice(): string | null {
  return resolveServerBinary({
    override: process.env["POND_SERVER_BIN"],
    isPackaged: app.isPackaged,
    resourcesPath: process.resourcesPath,
    repoRoot,
    platform: process.platform,
  });
}

function showWindow(): void {
  if (!win || win.isDestroyed()) return;
  if (!win.isVisible()) win.show();
  if (win.isMinimized()) win.restore();
  win.focus();
}

const healthLoop = createHealthLoop({
  healthCheck: () => server.healthCheck(),
  ensureRunning: () => server.ensureRunning(),
  onStatus: (healthy) => {
    emit("server-status", healthy);
    setTrayStatus(healthy);
  },
  onStarting: () => emit("server-starting"),
  backoffSeconds: recoveryBackoffSeconds,
  log,
});

const teardown = createTeardown({
  stopHealthLoop: () => healthLoop.stop(),
  killVoice: () => voice.killNow(),
  shutdownServer: () => server.shutdown(),
  releaseUi: () => {
    unregisterHotkeys();
    destroyTray();
  },
  log,
});

// Must happen before the app is ready.
registerAppScheme();

// One instance: a second launch surfaces this window instead of fighting for port and mic.
if (!app.requestSingleInstanceLock()) {
  app.quit();
} else {
  app.on("second-instance", showWindow);

  void app.whenReady().then(async () => {
    serveRendererFrom(distRoot(app.getAppPath()));

    // Reap a voice child orphaned by a hard kill; it would still hold the microphone.
    voice.cleanupOrphans();

    setAboutPanel();
    installMenu({ emit });
    registerIpc({ server, voice });

    win = createMainWindow({
      preloadPath: join(__dirname, "../preload/index.cjs"),
      serverUrl: server.url,
      userDataDir: app.getPath("userData"),
      devServerUrl: process.env["GIAP_DEV_SERVER"],
      onCloseRequested: (w) => {
        if (quitting) {
          w.destroy();
          return;
        }
        w.hide();
      },
    });

    createTray({
      emit,
      showWindow,
      iconPath: join(
        app.getAppPath(),
        "electron",
        "assets",
        "trayTemplate.png",
      ),
    });

    registerHotkeys({ emit, focusWindow: showWindow, log: log.info });

    // Don't block the window on the server: the startup screen renders while it comes up.
    server
      .ensureRunning()
      .then((url) => log.info(`pond-server ready at ${url}`))
      .catch((e: Error) => log.warn(`pond-server did not start: ${e.message}`));

    healthLoop.start();
  });

  app.on("activate", () => {
    if (BrowserWindow.getAllWindows().length === 0) return;
    showWindow();
  });

  // Must not quit on any platform: closing the window hides to the tray.
  app.on("window-all-closed", () => {});

  app.on("before-quit", () => {
    quitting = true;
    teardown.releaseChildren();
    teardown.releaseUi();
  });

  // `app.exit()` and quits that skip before-quit land here; release children or a sidecar survives.
  app.on("will-quit", () => {
    quitting = true;
    teardown.releaseChildren();
  });

  // Last resort, e.g. an uncaught exception: sync `kill()` is legal here. No UI teardown:
  // Electron calls aren't safe this late, and a throw would mask the kills.
  process.on("exit", () => teardown.releaseChildren());

  // Ctrl-C reaches neither before-quit nor will-quit. `app.exit`, not `app.quit`, which a
  // window handler can block.
  for (const signal of ["SIGINT", "SIGTERM"] as const) {
    process.once(signal, () => {
      log.warn(`received ${signal}; releasing child processes`);
      teardown.releaseChildren();
      app.exit(0);
    });
  }
}
