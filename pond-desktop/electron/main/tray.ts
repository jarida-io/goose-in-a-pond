// The system tray icon and its menu.
//
// Four items, matching the Tauri shell: Open, Summon, Canvas, Quit. The
// tooltip reflects whether pond-server is reachable, which is the only place
// that state is visible when the window is hidden.

import { Tray, Menu, nativeImage, app } from "electron";
import type { ShellEvent } from "../../src/shell/contract";

export interface TrayTargets {
  emit(event: ShellEvent): void;
  showWindow(): void;
  iconPath: string;
}

let tray: Tray | null = null;

export function createTray(t: TrayTargets): Tray {
  // createFromPath picks up the @2x file sitting beside this one on its own,
  // so only the @1x path is named.
  const image = nativeImage.createFromPath(t.iconPath);
  if (image.isEmpty()) {
    // Worth saying out loud rather than showing a blank slot: this is what a
    // packaging mistake looks like, and the tray is otherwise silent about it.
    console.warn(`[giap] tray icon missing or unreadable at ${t.iconPath}`);
  }
  // A template image carries shape only -- macOS discards the colour and
  // re-tints the alpha to suit the menu bar, so it stays legible in light
  // mode, dark mode and when highlighted. An opaque coloured image here draws
  // as a solid block.
  image.setTemplateImage(true);

  tray = new Tray(image);
  tray.setToolTip("Goose In A Pond");
  tray.setContextMenu(
    Menu.buildFromTemplate([
      { label: "Open", click: () => t.showWindow() },
      {
        label: "Summon",
        click: () => {
          t.showWindow();
          t.emit("desktop-summon");
        },
      },
      {
        label: "Canvas",
        click: () => {
          t.showWindow();
          t.emit("canvas-toggle");
        },
      },
      { type: "separator" },
      { label: "Quit", click: () => app.quit() },
    ]),
  );

  // Left-click shows the window, matching the Tauri behaviour. On macOS a
  // left-click opens the context menu by default, so this is additive.
  tray.on("click", () => t.showWindow());
  return tray;
}

/** Reflect server reachability in the tooltip. */
export function setTrayStatus(online: boolean): void {
  tray?.setToolTip(
    online ? "Goose In A Pond" : "Goose In A Pond - server offline",
  );
}

export function destroyTray(): void {
  tray?.destroy();
  tray = null;
}
