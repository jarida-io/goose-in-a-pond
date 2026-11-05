// A dashboard or desktop app nobody has open is not a device that failed to check in.

import { render, screen, cleanup, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const listDevices = vi.fn();

vi.mock("../../api/PondApiClient", () => ({
  api: {
    listDevices: (...args: unknown[]) => listDevices(...args),
    listSchedules: () => Promise.resolve([]),
  },
}));

// hubDataStore is mocked: importing the real one fires a full dashboard load.
vi.mock("../state/hubDataStore", () => ({ useHomeData: () => ({ devicesAreReal: true }) }));

import { HomeAlerts } from "./HomeAlerts";

const LAST_SEEN = new Date(Date.now() - 3_600_000).toISOString();

beforeEach(() => {
  cleanup();
  listDevices.mockReset();
});

describe("the offline device alert", () => {
  it("stays quiet when the only quiet device is a closed dashboard or app", async () => {
    listDevices.mockResolvedValue([
      { id: "web", name: "Pond dashboard", device_type: "desktop", is_online: false, last_seen: LAST_SEEN },
      { id: "term", name: "Pond CLI", device_type: "cli", is_online: false, last_seen: LAST_SEEN },
    ]);
    const { container } = render(<HomeAlerts onNavigate={() => {}} />);

    await waitFor(() => expect(listDevices).toHaveBeenCalled());
    expect(container.querySelector(".halerts")).toBeNull();
    expect(screen.queryByText(/hasn't checked in/)).toBeNull();
  });

  it("still alerts for a phone that has gone quiet", async () => {
    listDevices.mockResolvedValue([
      { id: "web", name: "Pond dashboard", device_type: "desktop", is_online: false, last_seen: LAST_SEEN },
      { id: "phone", name: "Jerry's phone", device_type: "gotg", is_online: false, last_seen: LAST_SEEN },
    ]);
    render(<HomeAlerts onNavigate={() => {}} />);

    expect(await screen.findByText(/Jerry's phone hasn't checked in/)).toBeTruthy();
    expect(screen.queryByText(/Pond dashboard/)).toBeNull();
  });
});
