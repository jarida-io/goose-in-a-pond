// The application menu. macOS routes Cmd-C/V/X/A and Undo through Edit menu items, and
// setApplicationMenu replaces Electron's default, so the Edit role must be spelled out.

import { Menu, app, type MenuItemConstructorOptions } from "electron";
import { join } from "node:path";
import type { ShellEvent } from "../../src/shell/contract";

export interface MenuTargets {
  emit(event: ShellEvent): void;
}

/** Brand the About panel (credit per DESIGN.md section 8), replacing Electron's default. */
export function setAboutPanel(): void {
  app.setAboutPanelOptions({
    applicationName: "Goose In A Pond",
    applicationVersion: app.getVersion(),
    copyright: "Copyright (c) 2026 Jarida Open Source Community\nApache-2.0",
    credits:
      "Privacy-first, fully local AI smart home assistant.\nAll inference, voice and memory run on your own hardware.",
    iconPath: join(app.getAppPath(), "electron", "assets", "about.png"),
  });
}

export function buildMenuTemplate(
  t: MenuTargets,
): MenuItemConstructorOptions[] {
  const isMac = process.platform === "darwin";

  const appMenu: MenuItemConstructorOptions[] = isMac
    ? [
        {
          label: app.name,
          submenu: [
            { role: "about" },
            { type: "separator" },
            { role: "services" },
            { type: "separator" },
            { role: "hide" },
            { role: "hideOthers" },
            { role: "unhide" },
            { type: "separator" },
            { role: "quit" },
          ],
        },
      ]
    : [];

  return [
    ...appMenu,
    // The load-bearing one. Do not replace with hand-rolled accelerators.
    { role: "editMenu" },
    {
      label: "View",
      submenu: [
        {
          label: "Toggle Canvas",
          accelerator: "CommandOrControl+Shift+G",
          click: () => t.emit("canvas-toggle"),
        },
        {
          label: "Voice Mode",
          accelerator: "CommandOrControl+Shift+V",
          click: () => t.emit("switch-to-voice"),
        },
        { type: "separator" },
        { role: "reload" },
        { role: "toggleDevTools" },
        { type: "separator" },
        { role: "resetZoom" },
        { role: "zoomIn" },
        { role: "zoomOut" },
        { type: "separator" },
        { role: "togglefullscreen" },
      ],
    },
    { role: "windowMenu" },
  ];
}

export function installMenu(t: MenuTargets): void {
  Menu.setApplicationMenu(Menu.buildFromTemplate(buildMenuTemplate(t)));
}
