// The desktop shell.
//
// Replaces src-tauri/src/main.rs. What it owns: the pond-server sidecar's
// lifecycle, the voice child, one window, the tray, two global shortcuts, and
// the menu.
//
// Ordering that matters, and cost someone a debugging session in the Rust:
//
//   * The scheme is registered before `whenReady`, because
//     registerSchemesAsPrivileged has no effect afterwards.
//   * On quit the voice child is killed BEFORE the server is shut down, so the
//     microphone and speaker are released first.

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

/**
 * The port pond-server says it bound, and when it said so.
 *
 * Deliberately computed from the server's own data directory rather than
 * Electron's userData path, which points somewhere else entirely on Linux.
 */
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

/**
 * The voice child runs the same binary as the sidecar. Resolved through the
 * server's own lookup so there is one answer to "where is pond-server", not
 * two that can disagree.
 */
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

// One instance only. A second launch -- from the Dock, or from
// `pond-server serve --native` -- should surface the window we already have
// rather than start a second shell fighting for the same port and microphone.
if (!app.requestSingleInstanceLock()) {
  app.quit();
} else {
  app.on("second-instance", showWindow);

  void app.whenReady().then(async () => {
    serveRendererFrom(distRoot(app.getAppPath()));

    // Reap a voice child orphaned by a hard kill of a previous run; it would
    // still be holding the microphone.
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

    // Kick the server, but do not block the window on it: the startup screen
    // exists precisely to render while the server is still coming up.
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

  // Closing the window hides to the tray, so this must NOT quit -- on any
  // platform, because the tray is the app's resting state.
  app.on("window-all-closed", () => {});

  app.on("before-quit", () => {
    quitting = true;
    teardown.releaseChildren();
    teardown.releaseUi();
  });

  // The RunEvent::Exit analogue the Tauri port dropped. `app.exit()` and a
  // quit that skips before-quit both land here, and without it those paths
  // left a sidecar running.
  app.on("will-quit", () => {
    quitting = true;
    teardown.releaseChildren();
  });

  // Last resort. `kill()` is a synchronous syscall, so it is legal here, and
  // this is the path that runs when an uncaught exception takes the app down.
  // UI teardown is deliberately NOT here: the Electron calls it makes are not
  // safe this late, and throwing here would mask the kills that matter.
  process.on("exit", () => teardown.releaseChildren());

  // Ctrl-C on a dev run is the commonest way to orphan a sidecar, because it
  // reaches neither before-quit nor will-quit. `app.exit` rather than
  // `app.quit`, which a window handler can block and leave the process hung.
  for (const signal of ["SIGINT", "SIGTERM"] as const) {
    process.once(signal, () => {
      log.warn(`received ${signal}; releasing child processes`);
      teardown.releaseChildren();
      app.exit(0);
    });
  }
}
