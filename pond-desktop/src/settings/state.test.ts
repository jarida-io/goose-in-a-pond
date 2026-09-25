import { describe, it, expect } from "vitest";
import { diffSettings, foldServerState, settingsValueEquals } from "./state";

const SERVER_SETTINGS = {
  assistant_name: "Pond",
  user_name: "Jerry",
  prompt_style: "balanced",
  agent_memory_inject: false,
  weather_enabled: false,
  chat_provider: "llamafile",
  chat_model: "llama3.2",
  llm_provider: "llamafile",
  llm_temperature: 0.7,
  llm_max_tokens: 1024,
  agent_goose_mode: "auto",
  agent_max_turns: 10,
  agent_memory_limit: 5,
  weather_latitude: 0,
  weather_longitude: 0,
  retention_event_log_days: 30,
  retention_sensor_days: 7,
  retention_session_messages_keep: 100,
  retention_events_by_category: { motion: 14, doorbell: 30 },
  voice_wake_word: "goose",
  voice_wake_word_transcriptions: [] as string[],
};

/** A fresh deep copy, so no test can share a nested object with another. */
function serverSettings(overrides: Record<string, unknown> = {}) {
  return { ...structuredClone(SERVER_SETTINGS), ...overrides };
}

describe("diffSettings", () => {
  it("reports nothing when the object is untouched", () => {
    const server = serverSettings();
    expect(diffSettings(server, structuredClone(server))).toEqual({});
  });

  it("compares arrays and maps by value, not identity", () => {
    const baseline = serverSettings({
      voice_wake_word_transcriptions: ["goose"],
      retention_events_by_category: { motion: 14, doorbell: 30 },
    });
    // Same values, different object identities, and a different key order.
    const current = serverSettings({
      voice_wake_word_transcriptions: ["goose"],
      retention_events_by_category: { doorbell: 30, motion: 14 },
    });
    expect(diffSettings(baseline, current)).toEqual({});
  });

  it("reports an array or map that really changed", () => {
    const baseline = serverSettings({ voice_wake_word_transcriptions: ["goose"] });
    const current = serverSettings({ voice_wake_word_transcriptions: ["goose", "guse"] });
    expect(diffSettings(baseline, current)).toEqual({
      voice_wake_word_transcriptions: ["goose", "guse"],
    });
  });

  it("distinguishes null, undefined-in-baseline and a real value", () => {
    expect(diffSettings({ tool_model: null }, { tool_model: "qwen" })).toEqual({ tool_model: "qwen" });
    expect(diffSettings({ tool_model: "qwen" }, { tool_model: null })).toEqual({ tool_model: null });
    expect(diffSettings({}, { tool_model: null })).toEqual({ tool_model: null });
    expect(diffSettings({ tool_model: null }, {})).toEqual({});
  });

  it("does not confuse an array with an object", () => {
    expect(settingsValueEquals([], {})).toBe(false);
    expect(settingsValueEquals({ 0: "a" }, ["a"])).toBe(false);
  });
});

describe("foldServerState", () => {
  it("takes the server value for a field the user has not touched", () => {
    const baseline = { user_name: "Jerry", chat_model: "llama3.2" };
    const prev = { ...baseline };
    const server = { user_name: "Jerry", chat_model: "gemma-4" };
    expect(foldServerState(prev, baseline, server)).toEqual({
      user_name: "Jerry",
      chat_model: "gemma-4",
    });
  });

  it("keeps an unsaved local edit when the server disagrees", () => {
    const baseline = { user_name: "Jerry", chat_model: "llama3.2" };
    const prev = { ...baseline, user_name: "Ochieng" };
    const server = { user_name: "Jerry", chat_model: "gemma-4" };
    expect(foldServerState(prev, baseline, server)).toEqual({
      user_name: "Ochieng",
      chat_model: "gemma-4",
    });
  });
});
