// Global shortcuts. On macOS without Accessibility/Input Monitoring permission `register`
// still returns true but the shortcut never fires, so callers log rather than assume.

import { globalShortcut } from "electron";
import type { ShellEvent } from "../../src/shell/contract";

export const CANVAS_HOTKEY = "CommandOrControl+Shift+G";
export const SUMMON_HOTKEY = "CommandOrControl+Shift+V";

export interface HotkeyTargets {
  emit(event: ShellEvent): void;
  /** Bring the window forward; a summon from another app is useless without it. */
  focusWindow(): void;
  log(message: string): void;
}

export function registerHotkeys(t: HotkeyTargets): void {
  const bind = (accelerator: string, fn: () => void) => {
    const ok = globalShortcut.register(accelerator, fn);
    t.log(
      ok
        ? `registered global shortcut ${accelerator}`
        : `could not register global shortcut ${accelerator} (already taken?)`,
    );
  };

  bind(CANVAS_HOTKEY, () => {
    t.focusWindow();
    t.emit("canvas-toggle");
  });

  bind(SUMMON_HOTKEY, () => {
    t.focusWindow();
    t.emit("desktop-summon");
  });
}

export function unregisterHotkeys(): void {
  globalShortcut.unregisterAll();
}
