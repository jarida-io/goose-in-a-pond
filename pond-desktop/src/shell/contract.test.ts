import { describe, it, expect, afterEach, vi } from "vitest";
import {
  SHELL_COMMANDS,
  SHELL_EVENTS,
  type ShellCommand,
  type ShellEvent,
} from "./contract";
import { isDesktopShell, invoke, listen } from "./index";

// `satisfies` catches a typo but not an omission (a command the preload would refuse at runtime);
// these mapped types force every key, and the length checks below compare them with the arrays.
const EVERY_COMMAND: Record<ShellCommand, true> = {
  server_health: true,
  ensure_server_running: true,
  start_voice_session: true,
  stop_voice_session: true,
  open_external: true,
};

const EVERY_EVENT: Record<ShellEvent, true> = {
  "server-status": true,
  "server-starting": true,
  "server-url": true,
  "desktop-summon": true,
  "canvas-toggle": true,
  "switch-to-voice": true,
  "voice-warmup": true,
  "voice-ready": true,
  "voice-state": true,
  "voice-transcript": true,
  "voice-token": true,
  "voice-tool-call": true,
  "voice-tool-result": true,
  "voice-done": true,
  "voice-error": true,
  "voice-audio-level": true,
  "voice-session-ended": true,
};

describe("the shell contract", () => {
  it("allowlists every declared command at runtime", () => {
    expect([...SHELL_COMMANDS].sort()).toEqual(
      Object.keys(EVERY_COMMAND).sort(),
    );
  });

  it("allowlists every declared event at runtime", () => {
    expect([...SHELL_EVENTS].sort()).toEqual(Object.keys(EVERY_EVENT).sort());
  });

  it("has no duplicate entries", () => {
    expect(new Set(SHELL_COMMANDS).size).toBe(SHELL_COMMANDS.length);
    expect(new Set(SHELL_EVENTS).size).toBe(SHELL_EVENTS.length);
  });

  // Pinned so a new voice event without a `useVoiceSession` listener fails here, not as a dropped frame.
  it("carries exactly eleven voice events", () => {
    const voice = SHELL_EVENTS.filter((e) => e.startsWith("voice-"));
    expect(voice).toHaveLength(11);
  });
});

describe("outside the desktop shell", () => {
  afterEach(() => {
    delete window.giap;
  });

  it("reports that there is no shell", () => {
    expect(isDesktopShell()).toBe(false);
  });

  it("rejects invoke rather than throwing, and names the command", async () => {
    await expect(invoke("server_health")).rejects.toThrow(/server_health/);
  });

  // Browser callers register unconditionally, with no isDesktopShell() guard.
  it("makes listen a no-op that returns a callable unlisten", () => {
    const unlisten = listen("server-status", () => {
      throw new Error("must not fire");
    });
    expect(() => unlisten()).not.toThrow();
  });
});

describe("inside the desktop shell", () => {
  afterEach(() => {
    delete window.giap;
  });

  function fakeBridge() {
    return {
      serverUrl: "http://127.0.0.1:4000",
      invoke: vi.fn().mockResolvedValue(true),
      listen: vi.fn().mockReturnValue(() => {}),
    };
  }

  it("reports the shell once the bridge is installed", () => {
    window.giap = fakeBridge();
    expect(isDesktopShell()).toBe(true);
  });

  it("forwards a void-argument command with no second argument", async () => {
    const bridge = fakeBridge();
    window.giap = bridge;
    await invoke("server_health");
    expect(bridge.invoke).toHaveBeenCalledWith("server_health");
  });

  it("forwards a command's arguments unchanged", async () => {
    const bridge = fakeBridge();
    window.giap = bridge;
    await invoke("start_voice_session", { sessionId: null });
    expect(bridge.invoke).toHaveBeenCalledWith("start_voice_session", {
      sessionId: null,
    });
  });

  it("hands back the bridge's own unlisten function", () => {
    const bridge = fakeBridge();
    const unlisten = vi.fn();
    bridge.listen.mockReturnValue(unlisten);
    window.giap = bridge;

    const returned = listen("voice-ready", () => {});
    expect(bridge.listen).toHaveBeenCalledWith(
      "voice-ready",
      expect.any(Function),
    );
    returned();
    expect(unlisten).toHaveBeenCalledTimes(1);
  });
});
