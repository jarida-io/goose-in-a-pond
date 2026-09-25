// Tray icon and menu; its tooltip is the only server status shown while the window is hidden.

import { Tray, Menu, nativeImage, app } from "electron";
import type { ShellEvent } from "../../src/shell/contract";

export interface TrayTargets {
  emit(event: ShellEvent): void;
  showWindow(): void;
  iconPath: string;
}

let tray: Tray | null = null;

export function createTray(t: TrayTargets): Tray {
  // createFromPath finds the @2x file beside it, so only the @1x path is named.
  const image = nativeImage.createFromPath(t.iconPath);
  if (image.isEmpty()) {
    // A packaging mistake; the tray itself would silently show a blank slot.
    console.warn(`[giap] tray icon missing or unreadable at ${t.iconPath}`);
  }
  // macOS re-tints a template image's alpha for the menu bar; an opaque image draws as a block.
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

  // Left-click shows the window (on macOS, in addition to opening the menu).
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
