// Wiring the contract's commands to ipcMain.
//
// Every channel is prefixed `giap:` and every name comes from the shared
// contract, so a handler registered here and a command declared there cannot
// drift: the preload refuses anything not in SHELL_COMMANDS, and this file
// will not compile with a name outside it.

import { ipcMain, shell } from "electron";
import type { ShellCommand } from "../../src/shell/contract";
import type { ServerProcess } from "./serverProcess";
import type { VoiceChildProcess } from "./voice/VoiceChildProcess";

export interface IpcTargets {
  server: ServerProcess;
  voice: VoiceChildProcess;
}

/** Register a handler, with the channel name checked against the contract. */
function handle<C extends ShellCommand>(
  command: C,
  fn: (args: unknown) => Promise<unknown> | unknown,
): void {
  ipcMain.handle(`giap:${command}`, (_event, args) => fn(args));
}

export function registerIpc(t: IpcTargets): void {
  handle("server_health", () => t.server.healthCheck());

  handle("ensure_server_running", () => t.server.ensureRunning());

  handle("start_voice_session", (args) => {
    const sessionId =
      (args as { sessionId?: string | null } | undefined)?.sessionId ?? null;
    return t.voice.start(sessionId);
  });

  handle("stop_voice_session", () => t.voice.stop());

  handle("open_external", async (args) => {
    const url = (args as { url?: string } | undefined)?.url;
    if (typeof url !== "string") throw new Error("open_external needs a url");
    // Only ever hand the OS an http(s) URL. A renderer that has been
    // compromised should not be able to launch a local file or a custom
    // scheme handler through this.
    const parsed = new URL(url);
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") {
      throw new Error(`refusing to open a ${parsed.protocol} URL externally`);
    }
    await shell.openExternal(url);
  });
}
