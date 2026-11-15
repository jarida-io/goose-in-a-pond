// The dashboard and the desktop app are listed by whether they are open, not whether they are online.

import { render, screen, cleanup } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";

const listDevices = vi.fn();
const listRooms = vi.fn();

vi.mock("../../../api/PondApiClient", () => ({
  api: {
    listDevices: (...args: unknown[]) => listDevices(...args),
    listRooms: (...args: unknown[]) => listRooms(...args),
  },
}));

import { RoomsDetail } from "./Rooms";

afterEach(cleanup);

describe("the rooms list", () => {
  it("names each room's card by the room's name, never its id", async () => {
    const roomId = "8f14e45f-ceea-467e-a9b1-5f0b2c3d4e5f";
    listRooms.mockResolvedValue([
      { id: roomId, name: "Kitchen", created_at: "", guest_accessible: false },
    ]);
    listDevices.mockResolvedValue([
      { id: "lamp", name: "Lamp", device_type: "light", room_id: roomId, is_online: true },
      { id: "fan", name: "Fan", device_type: "fan", room_id: "a-room-since-deleted", is_online: true },
    ]);
    render(<RoomsDetail go={() => {}} />);

    expect(await screen.findByText("Kitchen")).toBeTruthy();
    expect(screen.getByText("Unassigned")).toBeTruthy();
    expect(screen.queryByText(roomId)).toBeNull();
    expect(screen.queryByText("a-room-since-deleted")).toBeNull();
  });

  it("words the dashboard and the app as open or not open, and a phone as offline", async () => {
    listRooms.mockResolvedValue([]);
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
