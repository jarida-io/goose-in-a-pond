import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  type AppState,
  reducer,
  buildInitialState,
  type TranscriptMessage,
  type ContextCard,
} from "./reducer";
import { isDesktopShell } from "../shell";

vi.mock("../shell", () => ({ isDesktopShell: vi.fn(() => false) }));

// ── Mock localStorage (pure function tests must not touch real storage) ───────

const storageMock = (() => {
  let store: Record<string, string> = {};
  return {
    getItem: vi.fn((k: string) => store[k] ?? null),
    setItem: vi.fn((k: string, v: string) => {
      store[k] = v;
    }),
    removeItem: vi.fn((k: string) => {
      delete store[k];
    }),
    clear: () => {
      store = {};
    },
  };
})();

vi.stubGlobal("localStorage", storageMock);

// ── Helpers ───────────────────────────────────────────────────────────────────

const BASE: AppState = {
  mode: "gui",
  section: "dashboard",
  serverOnline: false,
  serverStarting: false,
  serverUrl: "http://127.0.0.1:4000",
  sessionToken: null,
  sessionId: null,
  needsOnboarding: false,
  voiceState: "idle",
  voiceError: null,
  transcript: [],
  contextCards: [],
  voiceRequestId: 0,
  lastResponseMeta: null,
  scheduleToasts: [],
  latestScheduleResult: null,
  scheduleRuns: [],
  unreadRunCount: 0,
  debriefContext: null,
};

function msg(overrides?: Partial<TranscriptMessage>): TranscriptMessage {
  return { id: 1, role: "user", text: "hello", timestamp: 0, ...overrides };
}

function card(overrides?: Partial<ContextCard>): ContextCard {
  return {
    id: 1,
    tool: "giap__weather",
    data: {},
    timestamp_ms: 0,
    ...overrides,
  };
}

beforeEach(() => storageMock.clear());

// ── Tests ─────────────────────────────────────────────────────────────────────

describe("reducer — mode & section", () => {
  it("SET_MODE updates mode and persists to localStorage", () => {
    const next = reducer(BASE, { type: "SET_MODE", payload: "voice" });
    expect(next.mode).toBe("voice");
    expect(storageMock.setItem).toHaveBeenCalledWith("giap-mode", "voice");
  });

  it("SET_SECTION updates section and persists to localStorage", () => {
    const next = reducer(BASE, { type: "SET_SECTION", payload: "settings" });
    expect(next.section).toBe("settings");
    expect(storageMock.setItem).toHaveBeenCalledWith(
      "giap-section",
      "settings",
    );
  });
});

describe("buildInitialState — classic UI is the default landing surface", () => {
  it("defaults to the classic dashboard when nothing is stored", () => {
    expect(buildInitialState().section).toBe("dashboard");
  });
  it("never lands in the Goose Hub on launch (coerces a persisted 'hub' back to dashboard)", () => {
    localStorage.setItem("giap-section", "hub");
    expect(buildInitialState().section).toBe("dashboard");
  });
  it("preserves a persisted classic section across launches", () => {
    localStorage.setItem("giap-section", "settings");
    expect(buildInitialState().section).toBe("settings");
  });
  it("the explicit giap-force-hub opt-in bypasses the coercion", () => {
    localStorage.setItem("giap-section", "hub");
    localStorage.setItem("giap-force-hub", "1");
    expect(buildInitialState().section).toBe("hub");
  });
});

describe("reducer — server status", () => {
  it("SERVER_ONLINE marks online and clears starting flag", () => {
    const s = { ...BASE, serverStarting: true };
    const next = reducer(s, { type: "SERVER_ONLINE" });
    expect(next.serverOnline).toBe(true);
    expect(next.serverStarting).toBe(false);
  });

  it("SERVER_OFFLINE marks offline and clears starting flag", () => {
    const s = { ...BASE, serverOnline: true, serverStarting: true };
    const next = reducer(s, { type: "SERVER_OFFLINE" });
    expect(next.serverOnline).toBe(false);
    expect(next.serverStarting).toBe(false);
  });

  it("SERVER_STARTING sets serverStarting flag", () => {
    const next = reducer(BASE, { type: "SERVER_STARTING" });
    expect(next.serverStarting).toBe(true);
  });

  it("SET_SERVER_URL updates URL and persists to localStorage", () => {
    const next = reducer(BASE, {
      type: "SET_SERVER_URL",
      payload: "http://192.168.1.10:4000",
    });
    expect(next.serverUrl).toBe("http://192.168.1.10:4000");
    expect(storageMock.setItem).toHaveBeenCalledWith(
      "giap-server-url",
      "http://192.168.1.10:4000",
    );
  });
});

describe("reducer — session token", () => {
  it("SET_SESSION_TOKEN stores a token in localStorage", () => {
    const next = reducer(BASE, {
      type: "SET_SESSION_TOKEN",
      payload: "tok123",
    });
    expect(next.sessionToken).toBe("tok123");
    expect(storageMock.setItem).toHaveBeenCalledWith(
      "giap-session-token",
      "tok123",
    );
  });

  it("SET_SESSION_TOKEN with null removes token from localStorage", () => {
    const s = { ...BASE, sessionToken: "tok123" };
    const next = reducer(s, { type: "SET_SESSION_TOKEN", payload: null });
    expect(next.sessionToken).toBeNull();
    expect(storageMock.removeItem).toHaveBeenCalledWith("giap-session-token");
  });
});

describe("reducer — voice state", () => {
  it("SET_VOICE_STATE updates voiceState and clears voiceError", () => {
    const s = { ...BASE, voiceError: "oops" };
    const next = reducer(s, { type: "SET_VOICE_STATE", payload: "recording" });
    expect(next.voiceState).toBe("recording");
    expect(next.voiceError).toBeNull();
  });

  // Regression: every producer dispatches SET_VOICE_ERROR then
  // SET_VOICE_STATE("error"), so an unconditional reset here erased the reason
  // and the UI could only ever render the generic "Error" label.
  it("SET_VOICE_STATE('error') preserves the reason set just before it", () => {
    let s = reducer(BASE, {
      type: "SET_VOICE_ERROR",
      payload: "piper voice not found",
    });
    s = reducer(s, { type: "SET_VOICE_STATE", payload: "error" });
    expect(s.voiceState).toBe("error");
    expect(s.voiceError).toBe("piper voice not found");
  });

  it("leaving the error state clears the reason", () => {
    const errored = reducer(
      reducer(BASE, { type: "SET_VOICE_ERROR", payload: "boom" }),
      { type: "SET_VOICE_STATE", payload: "error" },
    );
    expect(errored.voiceError).toBe("boom");

    const recovered = reducer(errored, {
      type: "SET_VOICE_STATE",
      payload: "wait",
    });
    expect(recovered.voiceError).toBeNull();
  });

  it("SET_VOICE_STATE transitions to 'wait' (passive wake-word listening)", () => {
    const next = reducer(BASE, { type: "SET_VOICE_STATE", payload: "wait" });
    expect(next.voiceState).toBe("wait");
    expect(next.voiceError).toBeNull();
  });

  it("full wait → recording → thinking → speaking → idle cycle via reducer", () => {
    let s = reducer(BASE, { type: "SET_VOICE_STATE", payload: "wait" });
    expect(s.voiceState).toBe("wait");
    s = reducer(s, { type: "SET_VOICE_STATE", payload: "recording" });
    expect(s.voiceState).toBe("recording");
    s = reducer(s, { type: "SET_VOICE_STATE", payload: "thinking" });
    expect(s.voiceState).toBe("thinking");
    s = reducer(s, { type: "SET_VOICE_STATE", payload: "speaking" });
    expect(s.voiceState).toBe("speaking");
    s = reducer(s, { type: "SET_VOICE_STATE", payload: "idle" });
    expect(s.voiceState).toBe("idle");
  });

  it("SET_VOICE_ERROR sets voiceError", () => {
    const next = reducer(BASE, {
      type: "SET_VOICE_ERROR",
      payload: "mic unavailable",
    });
    expect(next.voiceError).toBe("mic unavailable");
  });
});

describe("reducer — transcript", () => {
  it("APPEND_TRANSCRIPT adds a message", () => {
    const next = reducer(BASE, { type: "APPEND_TRANSCRIPT", payload: msg() });
    expect(next.transcript).toHaveLength(1);
    expect(next.transcript[0].text).toBe("hello");
  });

  it("APPEND_TRANSCRIPT caps at 50 messages (trims oldest)", () => {
    // Build state with 50 messages
    let s = BASE;
    for (let i = 0; i < 50; i++) {
      s = reducer(s, {
        type: "APPEND_TRANSCRIPT",
        payload: msg({ id: i, text: `m${i}` }),
      });
    }
    expect(s.transcript).toHaveLength(50);

    // Adding one more should trim the oldest
    const next = reducer(s, {
      type: "APPEND_TRANSCRIPT",
      payload: msg({ id: 99, text: "new" }),
    });
    expect(next.transcript).toHaveLength(50);
    expect(next.transcript[0].text).toBe("m1"); // m0 was trimmed
    expect(next.transcript[49].text).toBe("new");
  });

  it("APPEND_AGENT_TOKEN appends token to last agent message", () => {
    const s = {
      ...BASE,
      transcript: [msg({ role: "agent", text: "Hello" })],
    };
    const next = reducer(s, {
      type: "APPEND_AGENT_TOKEN",
      payload: { token: " world", done: false },
    });
    expect(next.transcript[0].text).toBe("Hello world");
  });

  it("APPEND_AGENT_TOKEN is a no-op when transcript is empty", () => {
    const next = reducer(BASE, {
      type: "APPEND_AGENT_TOKEN",
      payload: { token: "x", done: false },
    });
    expect(next.transcript).toHaveLength(0);
  });

  it("APPEND_AGENT_TOKEN is a no-op when last message is not from agent", () => {
    const s = { ...BASE, transcript: [msg({ role: "user" })] };
    const next = reducer(s, {
      type: "APPEND_AGENT_TOKEN",
      payload: { token: "x", done: false },
    });
    expect(next.transcript[0].text).toBe("hello");
  });

  it("CLEAR_TRANSCRIPT empties transcript and context cards", () => {
    const s = {
      ...BASE,
      transcript: [msg()],
      contextCards: [card()],
    };
    const next = reducer(s, { type: "CLEAR_TRANSCRIPT" });
    expect(next.transcript).toHaveLength(0);
    expect(next.contextCards).toHaveLength(0);
  });
});

describe("reducer — context cards", () => {
  it("PUSH_CONTEXT_CARD appends a card", () => {
    const next = reducer(BASE, { type: "PUSH_CONTEXT_CARD", payload: card() });
    expect(next.contextCards).toHaveLength(1);
    expect(next.contextCards[0].tool).toBe("giap__weather");
  });

  it("UPDATE_CONTEXT_CARD merges data into the most-recent matching card", () => {
    const c1 = card({ id: 1, callId: "call-1", data: { id: "call-1" } });
    const c2 = card({ id: 2, callId: "call-2", data: { id: "call-2" } });
    const s = { ...BASE, contextCards: [c1, c2] };
    const next = reducer(s, {
      type: "UPDATE_CONTEXT_CARD",
      payload: { callId: "call-1", data: { result: "22C" } },
    });
    expect(next.contextCards[0].data).toMatchObject({
      id: "call-1",
      result: "22C",
    });
    // c2 is unchanged
    expect(next.contextCards[1].data).toEqual({ id: "call-2" });
  });

  it("UPDATE_CONTEXT_CARD is a no-op when no card matches and no tool is given", () => {
    const c = card({ id: 1, callId: "call-1" });
    const s = { ...BASE, contextCards: [c] };
    const next = reducer(s, {
      type: "UPDATE_CONTEXT_CARD",
      payload: { callId: "nonexistent", data: { result: "x" } },
    });
    expect(next).toBe(s); // same reference = no change
  });

  it("UPDATE_CONTEXT_CARD upserts an orphan result as a new card when a tool is provided", () => {
    const c = card({ id: 1, callId: "call-1" });
    const s = { ...BASE, contextCards: [c] };
    const next = reducer(s, {
      type: "UPDATE_CONTEXT_CARD",
      payload: {
        callId: "orphan",
        tool: "giap__news",
        data: { result: "headline" },
      },
    });
    // The orphan result surfaces as its own card rather than being dropped.
    expect(next.contextCards).toHaveLength(2);
    expect(next.contextCards[1]).toMatchObject({
      tool: "giap__news",
      callId: "orphan",
      data: { result: "headline" },
    });
    // The pre-existing card is untouched.
    expect(next.contextCards[0]).toBe(c);
  });

  it("CLEAR_CONTEXT_CARDS empties the card list", () => {
    const s = { ...BASE, contextCards: [card(), card({ id: 2 })] };
    const next = reducer(s, { type: "CLEAR_CONTEXT_CARDS" });
    expect(next.contextCards).toHaveLength(0);
  });
});

describe("reducer — voice activation", () => {
  it("VOICE_ACTIVATE increments the counter", () => {
    const s1 = reducer(BASE, { type: "VOICE_ACTIVATE" });
    expect(s1.voiceRequestId).toBe(1);
    const s2 = reducer(s1, { type: "VOICE_ACTIVATE" });
    expect(s2.voiceRequestId).toBe(2);
  });
});

describe("reducer — immutability", () => {
  it("returns the same reference for unknown action types", () => {
    // @ts-expect-error — testing unknown action
    const next = reducer(BASE, { type: "UNKNOWN_ACTION" });
    expect(next).toBe(BASE);
  });
});

// The shell knows which port its own sidecar bound; a persisted URL is at best
// stale. This matters for the upgrade path: anyone who hit the double-spawn bug
// has http://127.0.0.1:4001 saved here, and honouring it would keep the fixed
// build pointed at a port nothing is listening on.
describe("buildInitialState server URL precedence", () => {
  it("prefers the shell's injected URL over a stored one in the desktop app", () => {
    vi.mocked(isDesktopShell).mockReturnValue(true);
    storageMock.setItem("giap-server-url", "http://127.0.0.1:4001");
    expect(buildInitialState().serverUrl).not.toBe("http://127.0.0.1:4001");
  });

  it("still honours a stored URL in a plain browser, where a user typed it", () => {
    vi.mocked(isDesktopShell).mockReturnValue(false);
    storageMock.setItem("giap-server-url", "http://192.168.1.10:4000");
    expect(buildInitialState().serverUrl).toBe("http://192.168.1.10:4000");
  });
});
