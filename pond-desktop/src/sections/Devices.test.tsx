import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor, cleanup, fireEvent } from "@testing-library/react";
import { Devices } from "./Devices";
import { api } from "../api/PondApiClient";
import type { Device } from "../api/types";

// ── Mock PondApiClient ────────────────────────────────────────────────────────

vi.mock("../api/PondApiClient", () => ({
  api: {
    listDevices: vi.fn(),
    registerDevice: vi.fn(),
    unregisterDevice: vi.fn(),
    invokeTool: vi.fn(),
    markDeviceOnline: vi.fn(),
    markDeviceOffline: vi.fn(),
    updateDevice: vi.fn(),
  },
}));

// A commissioned Matter light, as the bridge registers it: stable `matter-<id>`
// id, a cluster-inferred `device_type`, online.
const matterLight: Device = {
  id: "matter-2",
  name: "Living Room Light",
  device_type: "light",
  is_online: true,
  last_seen: new Date().toISOString(),
};

const matterLock: Device = {
  id: "matter-9",
  name: "Front Door",
  device_type: "lock",
  is_online: true,
};

beforeEach(() => {
  vi.clearAllMocks();
});
afterEach(() => cleanup());

describe("Devices section — Matter devices", () => {
  it("shows a commissioned Matter device in the list", async () => {
    (api.listDevices as ReturnType<typeof vi.fn>).mockResolvedValue([matterLight]);

    render(<Devices />);

    // The device appears by name, and its cluster-inferred type is shown.
    expect(await screen.findByText("Living Room Light")).toBeTruthy();
    expect(screen.getByText("light")).toBeTruthy();
  });

  it("renders type-appropriate icons for Matter device types", async () => {
    (api.listDevices as ReturnType<typeof vi.fn>).mockResolvedValue([
      matterLight,
      matterLock,
    ]);

    const { container } = render(<Devices />);
    await screen.findByText("Living Room Light");

    // lucide-react renders `<svg class="lucide lucide-<name>">`, so a light gets
    // the bulb and a lock gets the lock — not the generic monitor fallback.
    expect(container.querySelector(".lucide-lightbulb")).toBeTruthy();
    expect(container.querySelector(".lucide-lock")).toBeTruthy();
  });

  it("shows a phone icon for the GOTG mobile companion, not a computer", async () => {
    (api.listDevices as ReturnType<typeof vi.fn>).mockResolvedValue([
      { id: "phone-1", name: "Emmanuel's Phone", device_type: "gotg", is_online: true },
    ]);

    const { container } = render(<Devices />);
    await screen.findByText("Emmanuel's Phone");

    expect(container.querySelector(".lucide-smartphone")).toBeTruthy();
    expect(container.querySelector(".lucide-monitor")).toBeNull();
  });
});

describe("Devices section — power toggle", () => {
  it("turning off an online device calls markDeviceOffline, not the MCP tool", async () => {
    (api.listDevices as ReturnType<typeof vi.fn>).mockResolvedValue([matterLight]);
    (api.markDeviceOffline as ReturnType<typeof vi.fn>).mockResolvedValue(undefined);

    render(<Devices />);
    await screen.findByText("Living Room Light");

    fireEvent.click(screen.getByText("Turn off"));

    await waitFor(() => expect(api.markDeviceOffline).toHaveBeenCalledWith("matter-2"));
    expect(api.invokeTool).not.toHaveBeenCalled();
  });

  it("turning on an offline device calls markDeviceOnline, not the MCP tool", async () => {
    const offlineLight: Device = { ...matterLight, is_online: false };
    (api.listDevices as ReturnType<typeof vi.fn>).mockResolvedValue([offlineLight]);
    (api.markDeviceOnline as ReturnType<typeof vi.fn>).mockResolvedValue(undefined);

    render(<Devices />);
    await screen.findByText("Living Room Light");

    fireEvent.click(screen.getByText("Turn on"));

    await waitFor(() => expect(api.markDeviceOnline).toHaveBeenCalledWith("matter-2"));
    expect(api.invokeTool).not.toHaveBeenCalled();
  });
});

describe("Devices section — Configure modal", () => {
  it("opens editable, pre-filled with the device's current name/hostname/room", async () => {
    const withHostname: Device = { ...matterLight, hostname: "light.local", room: "Living Room" };
    (api.listDevices as ReturnType<typeof vi.fn>).mockResolvedValue([withHostname]);

    render(<Devices />);
    await screen.findByText("Living Room Light");

    fireEvent.click(screen.getByText("Configure"));

    const nameInput = await screen.findByDisplayValue("Living Room Light");
    expect(nameInput.tagName).toBe("INPUT");
    expect(screen.getByDisplayValue("light.local")).toBeTruthy();
    expect(screen.getByDisplayValue("Living Room")).toBeTruthy();
  });

  it("saving edited fields calls updateDevice with the new values", async () => {
    (api.listDevices as ReturnType<typeof vi.fn>).mockResolvedValue([matterLight]);
    (api.updateDevice as ReturnType<typeof vi.fn>).mockResolvedValue({
      ...matterLight,
      name: "Renamed Light",
      room: "Kitchen",
    });

    render(<Devices />);
    await screen.findByText("Living Room Light");
    fireEvent.click(screen.getByText("Configure"));

    const nameInput = await screen.findByDisplayValue("Living Room Light");
    fireEvent.change(nameInput, { target: { value: "Renamed Light" } });

    const roomInput = screen.getByPlaceholderText("Living Room");
    fireEvent.change(roomInput, { target: { value: "Kitchen" } });

    fireEvent.click(screen.getByText("Save"));

    await waitFor(() =>
      expect(api.updateDevice).toHaveBeenCalledWith("matter-2", {
        name: "Renamed Light",
        hostname: undefined,
        room: "Kitchen",
      }),
    );
  });
});
