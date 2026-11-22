import { describe, expect, it } from "vitest";
import type { ActivityEvent, PrivacySensitivity } from "../../api/types";
import { groupRepeats, rowTitle } from "./group";

function call(host: string, at: number, sensitivity: PrivacySensitivity = "internal"): ActivityEvent {
  return {
    timestamp: new Date(Date.UTC(2026, 10, 16, 10, 0, 60 - at)).toISOString(),
    category: "network",
    action: "egress.http",
    privacy_sensitivity: sensitivity,
    attributes: { host: { text: host }, tool: { text: "giap-music" }, latency_ms: { int: 40 + at } },
  };
}

describe("groupRepeats", () => {
  it("folds a consecutive run into one row that keeps the newest event", () => {
    const events = Array.from({ length: 24 }, (_, i) => call("api.spotify.com", i));
    const rows = groupRepeats(events);
    expect(rows).toHaveLength(1);
    expect(rows[0].count).toBe(24);
    expect(rows[0].event).toBe(events[0]);
    expect(rowTitle(rows[0])).toBe("Goose reached Spotify for music, 24 times");
  });

  it("does not fold across a different event, or across a privacy change", () => {
    const rows = groupRepeats([
      call("api.spotify.com", 0),
      call("api.spotify.com", 1),
      call("api.open-meteo.com", 2),
      call("api.spotify.com", 3),
      call("api.spotify.com", 4, "sensitive"),
    ]);
    expect(rows.map((r) => r.count)).toEqual([2, 1, 1, 1]);
    expect(rowTitle(rows[1])).toBe("Goose reached the weather service for music");
    expect(rowTitle(rows[2])).toBe("Goose reached Spotify for music");
  });

  it("returns nothing for nothing", () => {
    expect(groupRepeats([])).toEqual([]);
  });
});
