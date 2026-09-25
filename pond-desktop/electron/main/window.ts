// The one main window; Canvas is a renderer section, not a second window.

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

/** Sizes the window to its display; small panels (the Jetson's 1024x600) go chromeless fullscreen. */
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
  /** Dev-only Vite server (HMR); pond-server's CORS allowlist must keep http://localhost:1420. */
  devServerUrl?: string | undefined;
  /** Called when the user closes the window, so the caller can hide-to-tray. */
  onCloseRequested(win: BrowserWindow): void;
}

export function createMainWindow(opts: CreateWindowOptions): BrowserWindow {
  const workArea = screen.getPrimaryDisplay().workAreaSize;
  const geom = windowGeometry(workArea);

  // Remembered position only if a display still covers it; ignored on a kiosk (fullscreen).
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
      // PondApiClient reads __GIAP_SERVER_URL__ at module load, so the preload needs it from argv.
      additionalArguments: [`--giap-server-url=${opts.serverUrl}`],
    },
  });

  // Show only once the first paint is ready, so there is no white flash.
  win.once("ready-to-show", () => win.show());

  // target=_blank and window.open go to the real browser, never an in-app window.
  win.webContents.setWindowOpenHandler(({ url }) => {
    void shell.openExternal(url);
    return { action: "deny" };
  });

  void win.loadURL(opts.devServerUrl ?? rendererEntryUrl());

  // Save on "moved"/"resized" (settled), not on every frame of a drag.
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
