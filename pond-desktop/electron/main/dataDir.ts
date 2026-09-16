// Where pond-server keeps its data, and the port it says it bound.
//
// This deliberately does NOT use Electron's app.getPath("userData"). That
// points at ~/Library/Application Support/<app name> on macOS and ~/.config
// on Linux, while the server uses Rust's dirs::data_dir(), which is
// ~/.local/share on Linux. Asking Electron would put us in the wrong directory
// on exactly the platform the Jetson runs.
//
// Mirrors default_data_dir() in crates/pond-server/src/main.rs: POND_DATA_DIR
// wins outright, otherwise the platform data dir joined with "goose-in-a-pond".

import { join } from "node:path";

/** The file pond-server writes its ACTUAL bound port to, after it binds. */
export const RUNTIME_PORT_FILE = ".runtime_api_port";

export interface DataDirEnv {
  env: NodeJS.ProcessEnv;
  home: string;
  platform: NodeJS.Platform;
}

/**
 * The directory pond-server stores its databases, logs and models in.
 *
 * Kept independent of Electron so it agrees with the server on every platform.
 */
export function resolveDataDir(opts: DataDirEnv): string {
  const override = opts.env["POND_DATA_DIR"];
  if (typeof override === "string" && override.trim() !== "") {
    return override.trim();
  }

  const app = "goose-in-a-pond";
  switch (opts.platform) {
    case "darwin":
      return join(opts.home, "Library", "Application Support", app);
    case "win32": {
      const appData = opts.env["APPDATA"];
      if (typeof appData === "string" && appData.trim() !== "") {
        return join(appData.trim(), app);
      }
      return join(opts.home, "AppData", "Roaming", app);
    }
    default: {
      // dirs::data_dir() on Linux is $XDG_DATA_HOME, defaulting to
      // ~/.local/share -- NOT ~/.config, which is where Electron would point.
      const xdg = opts.env["XDG_DATA_HOME"];
      if (typeof xdg === "string" && xdg.trim() !== "") {
        return join(xdg.trim(), app);
      }
      return join(opts.home, ".local", "share", app);
    }
  }
}

/**
 * Parse a port out of the runtime port file.
 *
 * Digits only and inside the TCP range, in the same spirit as readPidfile: a
 * truncated or junk file must not produce a URL we then talk to.
 */
export function readRuntimePort(contents: string | null): number | null {
  if (contents === null) return null;
  const trimmed = contents.trim();
  if (!/^\d+$/.test(trimmed)) return null;
  const port = Number(trimmed);
  if (!Number.isSafeInteger(port) || port < 1 || port > 65_535) return null;
  return port;
}
