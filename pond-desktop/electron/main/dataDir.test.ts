import { describe, it, expect } from "vitest";
import { resolveDataDir, readRuntimePort } from "./dataDir";

// The shell has to agree with the server about where the data directory is,
// and Electron's own app.getPath("userData") does NOT agree with Rust's
// dirs::data_dir() on Linux. Getting this wrong means reading a port file that
// is not there and silently keeping a stale port.

describe("resolveDataDir", () => {
  it("honours POND_DATA_DIR, matching the server's own override", () => {
    expect(
      resolveDataDir({
        env: { POND_DATA_DIR: "/scratch/pond" },
        home: "/home/jo",
        platform: "linux",
      }),
    ).toBe("/scratch/pond");
  });

  it("ignores a blank override rather than resolving to nothing", () => {
    expect(
      resolveDataDir({
        env: { POND_DATA_DIR: "   " },
        home: "/home/jo",
        platform: "linux",
      }),
    ).toBe("/home/jo/.local/share/goose-in-a-pond");
  });

  // The trap: Electron would say ~/.config here, Rust says ~/.local/share.
  it("looks where the server actually writes on Linux, not where Electron keeps user data", () => {
    const dir = resolveDataDir({
      env: {},
      home: "/home/jo",
      platform: "linux",
    });
    expect(dir).toBe("/home/jo/.local/share/goose-in-a-pond");
    expect(dir).not.toContain(".config");
  });

  it("follows XDG_DATA_HOME when it is set", () => {
    expect(
      resolveDataDir({
        env: { XDG_DATA_HOME: "/xdg/data" },
        home: "/home/jo",
        platform: "linux",
      }),
    ).toBe("/xdg/data/goose-in-a-pond");
  });

  it("uses Application Support on macOS", () => {
    expect(
      resolveDataDir({ env: {}, home: "/Users/jo", platform: "darwin" }),
    ).toBe("/Users/jo/Library/Application Support/goose-in-a-pond");
  });

  it("uses APPDATA on Windows, falling back to the profile", () => {
    expect(
      resolveDataDir({
        env: { APPDATA: "C:\\Users\\jo\\AppData\\Roaming" },
        home: "C:\\Users\\jo",
        platform: "win32",
      }),
    ).toContain("goose-in-a-pond");
    expect(
      resolveDataDir({ env: {}, home: "/c/Users/jo", platform: "win32" }),
    ).toContain("goose-in-a-pond");
  });
});

describe("readRuntimePort", () => {
  it("reads the port the server wrote", () => {
    expect(readRuntimePort("4001")).toBe(4001);
    expect(readRuntimePort(" 4000\n")).toBe(4000);
  });

  it("rejects junk rather than building a malformed URL", () => {
    expect(readRuntimePort(null)).toBe(null);
    expect(readRuntimePort("")).toBe(null);
    expect(readRuntimePort("40a1")).toBe(null);
    expect(readRuntimePort("-1")).toBe(null);
    expect(readRuntimePort("4000 4001")).toBe(null);
  });

  it("rejects a port outside the TCP range", () => {
    expect(readRuntimePort("0")).toBe(null);
    expect(readRuntimePort("65536")).toBe(null);
    expect(readRuntimePort("65535")).toBe(65_535);
  });
});
