import { beforeEach, describe, expect, it, vi } from "vitest";
import { type AppState, reducer, type TranscriptMessage, type ContextCard } from "./reducer";

// ── Mock localStorage (pure function tests must not touch real storage) ───────

const storageMock = (() => {
  let store: Record<string, string> = {};
  return {
    getItem: vi.fn((k: string) => store[k] ?? null),
    setItem: vi.fn((k: string, v: string) => { store[k] = v; }),
    removeItem: vi.fn((k: string) => { delete store[k]; }),
    clear: () => { store = {}; },
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
  currentSpeaker: null,
};

function msg(overrides?: Partial<TranscriptMessage>): TranscriptMessage {
  return { id: 1, role: "user", text: "hello", timestamp: 0, ...overrides };
}

function card(overrides?: Partial<ContextCard>): ContextCard {
  return { id: 1, tool: "giap__weather", data: {}, timestamp_ms: 0, ...overrides };
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
    expect(storageMock.setItem).toHaveBeenCalledWith("giap-section", "settings");
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
    const next = reducer(BASE, { type: "SET_SERVER_URL", payload: "http://192.168.1.10:4000" });
    expect(next.serverUrl).toBe("http://192.168.1.10:4000");
    expect(storageMock.setItem).toHaveBeenCalledWith("giap-server-url", "http://192.168.1.10:4000");
  });
});

describe("reducer — session token", () => {
  it("SET_SESSION_TOKEN stores a token in localStorage", () => {
    const next = reducer(BASE, { type: "SET_SESSION_TOKEN", payload: "tok123" });
    expect(next.sessionToken).toBe("tok123");
    expect(storageMock.setItem).toHaveBeenCalledWith("giap-session-token", "tok123");
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
    const next = reducer(BASE, { type: "SET_VOICE_ERROR", payload: "mic unavailable" });
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
      s = reducer(s, { type: "APPEND_TRANSCRIPT", payload: msg({ id: i, text: `m${i}` }) });
    }
    expect(s.transcript).toHaveLength(50);

    // Adding one more should trim the oldest
    const next = reducer(s, { type: "APPEND_TRANSCRIPT", payload: msg({ id: 99, text: "new" }) });
    expect(next.transcript).toHaveLength(50);
    expect(next.transcript[0].text).toBe("m1");    // m0 was trimmed
    expect(next.transcript[49].text).toBe("new");
  });

  it("APPEND_AGENT_TOKEN appends token to last agent message", () => {
    const s = {
      ...BASE,
      transcript: [msg({ role: "agent", text: "Hello" })],
    };
    const next = reducer(s, { type: "APPEND_AGENT_TOKEN", payload: { token: " world", done: false } });
    expect(next.transcript[0].text).toBe("Hello world");
  });

  it("APPEND_AGENT_TOKEN is a no-op when transcript is empty", () => {
    const next = reducer(BASE, { type: "APPEND_AGENT_TOKEN", payload: { token: "x", done: false } });
    expect(next.transcript).toHaveLength(0);
  });

  it("APPEND_AGENT_TOKEN is a no-op when last message is not from agent", () => {
    const s = { ...BASE, transcript: [msg({ role: "user" })] };
    const next = reducer(s, { type: "APPEND_AGENT_TOKEN", payload: { token: "x", done: false } });
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
