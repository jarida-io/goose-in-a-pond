// The dashboard and the desktop app are listed by whether they are open, not whether they are online.

import { render, screen, cleanup } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

const listDevices = vi.fn();

vi.mock("../../../api/PondApiClient", () => ({
  api: { listDevices: (...args: unknown[]) => listDevices(...args) },
}));

import { RoomsDetail } from "./Rooms";

afterEach(cleanup);

describe("the rooms list", () => {
  it("words the dashboard and the app as open or not open, and a phone as offline", async () => {
    const quiet = new Date(Date.now() - 3_600_000).toISOString();
    listDevices.mockResolvedValue([
      { id: "web", name: "Pond dashboard", device_type: "desktop", is_online: false, last_seen: quiet },
      { id: "app", name: "Pond Desktop", device_type: "desktop", is_online: true, last_seen: quiet },
      { id: "phone", name: "Jerry's phone", device_type: "gotg", is_online: false, last_seen: quiet },
    ]);
    render(<RoomsDetail go={() => {}} />);

    expect(await screen.findByText("desktop · Not open")).toBeTruthy();
    expect(screen.getByText("desktop · Open")).toBeTruthy();
    expect(screen.getByText("gotg · offline")).toBeTruthy();
  });
});
