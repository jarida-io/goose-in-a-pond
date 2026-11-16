import { describe, expect, it } from "vitest";
import type { ActivityEvent, AttributeValue, EventCategory } from "../../api/types";
import { describeEvent, formatBytes, placeName } from "./describe";

type Attr = string | number | boolean;

function ev(action: string, attrs: Record<string, Attr> = {}, category: EventCategory = "system"): ActivityEvent {
  const attributes: Record<string, AttributeValue> = {};
  for (const [k, v] of Object.entries(attrs)) {
    attributes[k] =
      typeof v === "string" ? { text: v }
      : typeof v === "boolean" ? { bool: v }
      : Number.isInteger(v) ? { int: v } : { float: v };
  }
  return {
    timestamp: "2026-11-16T10:00:00Z",
    category,
    action,
    privacy_sensitivity: "internal",
    attributes,
  };
}

const title = (action: string, attrs: Record<string, Attr> = {}) => describeEvent(ev(action, attrs)).title;

describe("describeEvent", () => {
  it("names a known host and what the call was for", () => {
    const d = describeEvent(ev("egress.http", { host: "api.spotify.com", method: "GET", latency_ms: 80, tool: "giap-music", status: 200 }));
    expect(d.title).toBe("Goose reached Spotify for music");
    expect(d.detail).toBe("api.spotify.com");
    expect(title("egress.http", { host: "api.search.brave.com", tool: "search_web" }))
      .toBe("Goose reached the web search service for a web search");
    expect(title("egress.http", { host: "api.open-meteo.com", tool: "giap-weather__get_current_weather" }))
      .toBe("Goose reached the weather service for the weather");
  });

  it("falls back to the host name, with no repeated detail line", () => {
    const d = describeEvent(ev("egress.http", { host: "example.org" }));
    expect(d.title).toBe("Goose reached example.org");
    expect(d.detail).toBeUndefined();
  });

  it("says when a call failed", () => {
    expect(title("egress.http", { host: "api.spotify.com", tool: "giap-music", status: 503 }))
      .toBe("Goose tried to reach Spotify for music, but the request failed");
  });

  it("describes a refused call with its reason", () => {
    const d = describeEvent(ev("egress.denied", { host: "evil.example", network_mode: "local_only", reason: "local only mode" }));
    expect(d.title).toBe("Goose was kept from reaching evil.example");
    expect(d.detail).toBe("evil.example: local only mode");
  });

  it("describes a turn, a completion and a tool call", () => {
    expect(title("agent.turn", { tool_count: 0 })).toBe("Goose answered a message");
    expect(title("agent.turn", { tool_count: 1 })).toBe("Goose answered a message, using 1 tool");
    expect(title("agent.turn", { tool_count: 3 })).toBe("Goose answered a message, using 3 tools");
    const c = describeEvent(ev("inference.completion", { prompt_tokens: 900, completion_tokens: 40, model: "gemma-3" }));
    expect(c.title).toBe("Goose thought through a reply");
    expect(c.detail).toBe("gemma-3");
    expect(title("tool.call", { tool: "giap-knowledge__search_web" })).toBe("Goose searched the web");
    expect(title("tool.call", { tool: "save_memory" })).toBe("Goose saved a memory");
    expect(title("tool.call", { tool: "giap-x__make_toast" })).toBe("Goose used make toast");
  });

  it("describes the house: sensors, cameras, devices and presence", () => {
    expect(title("sensor.reading", { device_id: "living-room", sensor_type: "temperature", value: 21.5, unit: "C" }))
      .toBe("Temperature reading from living room: 21.5 C");
    expect(title("camera.event", { camera_id: "front_door", event_type: "motion" }))
      .toBe("The front door camera noticed motion");
    expect(title("device.state_changed", { device_id: "hall-lamp", key: "power" }))
      .toBe("Hall lamp changed its power");
    expect(title("presence.profile", { transition: "arrived", source: "phone" })).toBe("Someone arrived home");
    expect(title("presence.profile", { transition: "departed", source: "phone" })).toBe("Someone left home");
    expect(title("time.tick")).toBe("The Pond's clock reached a new hour");
  });

  it("describes a conversation's phases", () => {
    expect(title("session.lifecycle", { phase: "started" })).toBe("A conversation started");
    expect(title("session.lifecycle", { phase: "idle" })).toBe("A conversation went quiet");
    expect(title("session.lifecycle", { phase: "resumed" })).toBe("A conversation picked up again");
  });

  it("describes an access check by its verdict", () => {
    const base = { principal: "token:abc", action: "read", scope: "memory" };
    expect(title("security.audit", { ...base, ok: true, verdict: "allow" })).toBe("A paired device was allowed to read memory");
    expect(title("security.audit", { ...base, ok: false, verdict: "deny" })).toBe("A paired device was not allowed to read memory");
    expect(title("security.audit", { ...base, ok: true, verdict: "would_deny" }))
      .toBe("A paired device was allowed to read memory, though stricter rules would refuse it");
    expect(title("security.audit", { principal: "loopback", action: "identify_session", scope: "session", verdict: "allow" }))
      .toBe("This computer was allowed to identify session session");
  });

  it("describes pairing", () => {
    expect(title("auth.device_paired", { method: "cpace", device_name: "Ana's phone" })).toBe("Ana's phone was paired with the Pond");
    expect(title("auth.device_paired", { method: "cpace" })).toBe("A new device was paired with the Pond");
    expect(title("auth.device_resumed", { method: "cpace-device" })).toBe("A paired device reconnected to the Pond");
    const failed = describeEvent(ev("auth.pairing_verify_failed", { method: "cpace", rejection_reason: "wrong_code" }));
    expect(failed.title).toBe("A device could not be paired");
    expect(failed.detail).toBe("Wrong code");
    expect(title("auth.device_resume_failed", { method: "cpace-device", device_name: "Tablet" })).toBe("Tablet could not reconnect");
    expect(title("auth.spoken_code_issued", { attributed: true })).toBe("Goose read out a pairing code");
    expect(title("developer.debug_switched", { enabled: true })).toBe("Developer debugging was turned on");
    expect(title("developer.debug_switched", { enabled: false })).toBe("Developer debugging was turned off");
  });

  it("describes My Pond admin actions", () => {
    expect(title("admin.unlocked", { by_key: false })).toBe("My Pond was unlocked");
    expect(title("admin.unlocked", { by_key: true })).toBe("My Pond was unlocked with a recovery key");
    expect(title("admin.unlock_refused")).toBe("An attempt to unlock My Pond was refused");
    expect(title("admin.key_issued")).toBe("A My Pond recovery key was issued");
    expect(title("admin.key_revoked")).toBe("A My Pond recovery key was revoked");
    expect(title("admin.file_deleted", { area: "models", freed_bytes: 3 * 1024 * 1024 * 1024 })).toBe("A file was deleted from models, freeing 3.0 GB");
    expect(title("admin.cache_cleared", { cache: "kv_snapshots", freed_bytes: 52_428_800 })).toBe("The kv snapshots cache was cleared, freeing 50 MB");
    expect(title("admin.task_deleted", { task_id: "t1" })).toBe("A task was deleted");
    expect(title("admin.artifact_deleted", { task_id: "t1", artifact_id: "a1" })).toBe("A file from a task was deleted");
    const power = describeEvent(ev("admin.power_mode_set", { mode: "MAXN", reboot_required: true }));
    expect(power.title).toBe("Power mode was set to MAXN");
    expect(power.detail).toBe("Takes effect after a restart");
    expect(title("admin.restart_requested", { force: false })).toBe("A restart of the Pond was requested");
  });

  it("describes a settings change, and hides a redacted value", () => {
    expect(title("settings.changed", { key: "voice_tts_voice", label: "Voice", from: "af_heart", to: "bf_emma" }))
      .toBe("Setting changed: Voice from af_heart to bf_emma");
    expect(title("settings.changed", { key: "brave_api_key", label: "Web search key", redacted: true, from: "x", to: "y" }))
      .toBe("Web search key was changed");
    expect(title("settings.changed", { key: "home_city" })).toBe("Home city was changed");
  });

  it("never shows raw dotted text for an action it does not know", () => {
    const t = title("system.brand_new_thing");
    expect(t).toBe("System: brand new thing");
    expect(t).not.toContain(".");
    expect(title("egress.http_retry")).toBe("Egress: http retry");
  });

  it("keeps the raw action and attributes for hover", () => {
    const d = describeEvent(ev("egress.http", { host: "api.spotify.com", tool: "giap-music", status: 200 }));
    expect(d.technical).toBe("egress.http host=api.spotify.com status=200 tool=giap-music");
  });
});

describe("placeName", () => {
  it("matches a host exactly or as a subdomain, never as a lookalike", () => {
    expect(placeName("accounts.spotify.com")).toBe("Spotify");
    expect(placeName("geocoding-api.open-meteo.com")).toBe("the weather service");
    expect(placeName("notspotify.com")).toBe("notspotify.com");
  });
});

describe("formatBytes", () => {
  it("scales to the largest whole unit", () => {
    expect(formatBytes(512)).toBe("512 bytes");
    expect(formatBytes(1536)).toBe("1.5 KB");
    expect(formatBytes(20 * 1024 * 1024)).toBe("20 MB");
  });
});
