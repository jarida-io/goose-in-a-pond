// The application menu.
//
// The Edit menu is not decoration: macOS routes Cmd-C/V/X/A and Undo through
// the responder chain to menu items, so a window with no Edit menu has no
// working clipboard at all. The Tauri shell carried one for exactly this
// reason, with a comment saying so. Electron installs a correct default menu
// if you never touch it -- but the moment you call setApplicationMenu you
// replace the whole thing, so the roles have to be spelled out.
//
// Everything here is a role except the two View items that emit to the
// renderer. Roles are what make the standard items behave natively (and be
// translated) rather than being re-implemented badly.

import { Menu, app, type MenuItemConstructorOptions } from "electron";
import { join } from "node:path";
import type { ShellEvent } from "../../src/shell/contract";

export interface MenuTargets {
  emit(event: ShellEvent): void;
}

/**
 * Brand the standard About panel.
 *
 * Without this it shows Electron's default: the app name, the Electron and
 * Chromium versions, and nothing about the product. Credit is
 * "Jarida Open Source Community" per DESIGN.md section 8.
 */
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
