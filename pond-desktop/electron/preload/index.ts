// Preload bridge. The two Sets below are the security boundary: only contract channels reach IPC.

import { contextBridge, ipcRenderer, type IpcRendererEvent } from "electron";
import { SHELL_COMMANDS, SHELL_EVENTS } from "../../src/shell/contract";

const commands = new Set<string>(SHELL_COMMANDS);
const events = new Set<string>(SHELL_EVENTS);

const ARG_PREFIX = "--giap-server-url=";
const serverUrl =
  process.argv.find((a) => a.startsWith(ARG_PREFIX))?.slice(ARG_PREFIX.length) ??
  "http://127.0.0.1:4000";

// PondApiClient reads this at module load, so it must exist before any page script runs.
contextBridge.exposeInMainWorld("__GIAP_SERVER_URL__", serverUrl);

contextBridge.exposeInMainWorld("giap", {
  serverUrl,

  invoke(command: string, args?: unknown): Promise<unknown> {
    if (!commands.has(command)) {
      return Promise.reject(new Error(`unknown shell command: ${command}`));
    }
    return ipcRenderer.invoke(`giap:${command}`, args);
  },

  /** Synchronous, so no event is lost between mount and registration; returns the unsubscribe. */
  listen(event: string, handler: (payload: unknown) => void): () => void {
    if (!events.has(event)) throw new Error(`unknown shell event: ${event}`);
    const channel = `giap:${event}`;
    const wrapped = (_e: IpcRendererEvent, payload: unknown) => handler(payload);
    ipcRenderer.on(channel, wrapped);
    return () => {
      ipcRenderer.off(channel, wrapped);
    };
  },
});
