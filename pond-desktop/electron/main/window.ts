// The main window.
//
// One window, labelled `main`. The Tauri capabilities file also declared a
// `canvas` window, but nothing ever created it -- Canvas is a section inside
// the renderer, toggled by an event -- so it is not reproduced here.

import { BrowserWindow, screen, shell } from "electron";
import { join } from "node:path";
import { rendererEntryUrl } from "./protocol";
import {
  usableBounds,
  readState,
  writeState,
  stateFilePath,
} from "./windowState";

/** Below this the panel is a kiosk display, not a desktop. */
const SMALL_W = 1100;
const SMALL_H = 700;

/** The comfortable windowed size on a roomy desktop. */
const PREFERRED_W = 1280;
const PREFERRED_H = 860;

export interface Geometry {
  width: number;
  height: number;
  /** Borderless and fullscreen: the whole panel is usable UI. */
  kiosk: boolean;
}

/**
 * Size the window to the display it will open on.
 *
 * The case this exists for is the 7-inch 1024x600 panel on the Jetson: a fixed
 * 1280x860 window overflows it and the title bar and top rows are lost, and a
 * 600px minimum height cannot fit under a desktop's top bar. So on a small
 * panel we drop the chrome and take the whole screen; on a roomy desktop we
 * keep a normal window.
 */
export function windowGeometry(workArea: {
  width: number;
  height: number;
}): Geometry {
  const kiosk = workArea.width <= SMALL_W || workArea.height <= SMALL_H;
  return {
    width: Math.min(PREFERRED_W, workArea.width),
    height: Math.min(PREFERRED_H, workArea.height),
    kiosk,
  };
}

export interface CreateWindowOptions {
  preloadPath: string;
  serverUrl: string;
  /** Where to remember the window's position between runs. */
  userDataDir: string;
  /**
   * In dev, the Vite server to load instead of the built bundle, so HMR works.
   * Its origin (http://localhost:1420) is already in pond-server's CORS
   * allowlist, which is why that entry stays after the migration.
   */
  devServerUrl?: string | undefined;
  /** Called when the user closes the window, so the caller can hide-to-tray. */
  onCloseRequested(win: BrowserWindow): void;
}

export function createMainWindow(opts: CreateWindowOptions): BrowserWindow {
  const workArea = screen.getPrimaryDisplay().workAreaSize;
  const geom = windowGeometry(workArea);

  // A remembered position, but only if some display still covers it. On a
  // kiosk panel we ignore it entirely -- the window is fullscreen there and a
  // saved desktop position would be meaningless.
  const statePath = stateFilePath(opts.userDataDir);
  const remembered = geom.kiosk
    ? null
    : usableBounds(
        readState(statePath),
        screen.getAllDisplays().map((d) => d.bounds),
      );

  const win = new BrowserWindow({
    title: "Goose In A Pond",
    ...(remembered ? { x: remembered.x, y: remembered.y } : {}),
    width: remembered?.width ?? geom.width,
    height: remembered?.height ?? geom.height,
    minWidth: 360,
    minHeight: 480,
    center: remembered === null,
    show: false,
    frame: !geom.kiosk,
    fullscreen: geom.kiosk,
    webPreferences: {
      preload: opts.preloadPath,
      contextIsolation: true,
      sandbox: true,
      nodeIntegration: false,
      webSecurity: true,
      // PondApiClient reads window.__GIAP_SERVER_URL__ at module load, before
      // any of our code runs, so the preload must already know the URL. Tauri
      // injected it as a pre-load script; here it rides on argv.
      additionalArguments: [`--giap-server-url=${opts.serverUrl}`],
    },
  });

  // Show only once the first paint is ready, so there is no white flash.
  win.once("ready-to-show", () => win.show());

  // Any target=_blank or window.open goes to the real browser, never an in-app
  // window. Under Tauri the fallback path for opening a link was window.open,
  // which in a desktop shell is actively wrong rather than merely degraded.
  win.webContents.setWindowOpenHandler(({ url }) => {
    void shell.openExternal(url);
    return { action: "deny" };
  });

  void win.loadURL(opts.devServerUrl ?? rendererEntryUrl());

  // Save on the events that actually settle a new position, not on every
  // frame of a drag.
  const remember = () => {
    if (win.isDestroyed() || win.isMinimized() || win.isFullScreen()) return;
    writeState(statePath, win.getNormalBounds());
  };
  win.on("moved", remember);
  win.on("resized", remember);

  win.on("close", (e) => {
    // Record where it was before anything hides or destroys it.
    remember();
    e.preventDefault();
    opts.onCloseRequested(win);
  });

  return win;
}

/** Where the built renderer lives relative to the compiled main process. */
export function distRoot(appPath: string): string {
  return join(appPath, "dist");
}
