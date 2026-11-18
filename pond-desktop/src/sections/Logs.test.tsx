import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { ActivityEvent } from "../api/types";

vi.mock("../api/PondApiClient", () => ({
  api: {
    listActivity: vi.fn(),
    getActivitySummary: vi.fn(),
  },
}));

import { api } from "../api/PondApiClient";
import { Logs } from "./Logs";

function spotify(i: number, sensitive = false): ActivityEvent {
  return {
    timestamp: new Date(Date.now() - i * 1000).toISOString(),
    category: "network",
    action: "egress.http",
    privacy_sensitivity: sensitive ? "sensitive" : "internal",
    attributes: { host: { text: "api.spotify.com" }, tool: { text: "giap-music" } },
  };
}

describe("Activity log", () => {
  beforeEach(() => {
    const events = [...Array.from({ length: 24 }, (_, i) => spotify(i)), spotify(30, true)];
    vi.mocked(api.listActivity).mockResolvedValue({ count: events.length, events });
    vi.mocked(api.getActivitySummary).mockResolvedValue({
      window: "day",
      since: new Date().toISOString(),
      total: 1000,
      truncated: true,
      by_category: { network: 1000 },
    });
  });
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("reads each run of events as one plain sentence with a count", async () => {
    render(<Logs />);
    expect(await screen.findByText("Goose reached Spotify for music, 24 times")).toBeTruthy();
    const rows = screen.getAllByTestId("activity-row");
    expect(rows).toHaveLength(2);
    expect(rows[0].getAttribute("title")).toContain("egress.http");
    expect(screen.queryByText(/Egress Http/)).toBeNull();
  });

  it("marks a capped summary as a floor and names the window it covers", async () => {
    render(<Logs />);
    expect((await screen.findByTestId("activity-total")).textContent).toBe("1000+");
    expect(screen.getByText("events in the last 24 hours")).toBeTruthy();
    expect(screen.getByTestId("activity-private-count").textContent)
      .toBe("1 of the 25 latest events hold private details");
  });

  it("bounds the list by the window the select names", async () => {
    render(<Logs />);
    await waitFor(() => expect(api.listActivity).toHaveBeenCalled());
    const daySince = Date.parse(vi.mocked(api.listActivity).mock.calls[0][0]!.since!);
    expect(Date.now() - daySince).toBeGreaterThan(23 * 3600 * 1000);
    expect(Date.now() - daySince).toBeLessThan(25 * 3600 * 1000);

    fireEvent.change(screen.getByLabelText("Time window"), { target: { value: "hour" } });
    await waitFor(() => expect(api.getActivitySummary).toHaveBeenLastCalledWith("hour"));
    const calls = vi.mocked(api.listActivity).mock.calls;
    const hourSince = Date.parse(calls[calls.length - 1][0]!.since!);
    expect(Date.now() - hourSince).toBeLessThan(61 * 60 * 1000);
    expect(await screen.findByText("events in the last hour")).toBeTruthy();
  });
});
