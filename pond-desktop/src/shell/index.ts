// The renderer's only door to the desktop-shell bridge. Outside the shell (browser, Vitest,
// Playwright) `listen()` is a no-op and `invoke()` rejects rather than throwing.

import type {
  ShellCommand,
  ShellCommands,
  ShellEvent,
  ShellEvents,
} from "./contract";

/** A tuple, so commands taking `void` get no second argument rather than `undefined`. */
type ArgsFor<C extends ShellCommand> = ShellCommands[C]["args"] extends void
  ? []
  : [ShellCommands[C]["args"]];

/** The object the preload publishes on `window.giap`. */
export interface DesktopShellApi {
  /** pond-server base URL; also mirrored to `window.__GIAP_SERVER_URL__` for PondApiClient. */
  readonly serverUrl: string;

  invoke<C extends ShellCommand>(
    command: C,
    ...args: ArgsFor<C>
  ): Promise<ShellCommands[C]["result"]>;

  /** Synchronous: returns the unsubscribe function directly. */
  listen<E extends ShellEvent>(
    event: E,
    handler: (payload: ShellEvents[E]) => void,
  ): () => void;
}

declare global {
  interface Window {
    giap?: DesktopShellApi;
  }
}

export function isDesktopShell(): boolean {
  return typeof window !== "undefined" && window.giap !== undefined;
}

/** Rejects outside the shell; code that may run in a browser checks `isDesktopShell()` first. */
export function invoke<C extends ShellCommand>(
  command: C,
  ...args: ArgsFor<C>
): Promise<ShellCommands[C]["result"]> {
  const shell = window.giap;
  if (!shell) {
    return Promise.reject(
      new Error(`shell command "${command}" called outside the desktop shell`),
    );
  }
  return shell.invoke(command, ...args);
}

/** Registers synchronously (no mount-to-registration gap); a no-op outside the shell. */
export function listen<E extends ShellEvent>(
  event: E,
  handler: (payload: ShellEvents[E]) => void,
): () => void {
  return window.giap?.listen(event, handler) ?? (() => {});
}

export type {
  ShellCommand,
  ShellCommands,
  ShellEvent,
  ShellEvents,
  VoiceEndReason,
} from "./contract";
