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
    getMatterStatus: vi.fn(),
    updateSettings: vi.fn(),
  },
}));

// A Matter light as the bridge registers it.
const matterLight: Device = {
  id: "matter-2",
  name: "Living Room Light",
  device_type: "light",
  is_online: true,
  last_seen: new Date().toISOString(),
  capabilities: ["power", "brightness"],
};

/** A contact sensor: nothing to drive, which is what the power button gates on. */
const contactSensor: Device = {
  id: "matter-5",
  name: "Contact Sensor",
  device_type: "sensor",
  is_online: true,
  capabilities: [],
};

const matterLock: Device = {
  id: "matter-9",
  name: "Front Door",
  device_type: "lock",
  is_online: true,
  capabilities: ["locked"],
};

/** `get_device_state`'s answer, in the shape `state_line` writes it. */
function stateSaying(deviceId: string, power: "on" | "off") {
  return { tool: "get_device_state", success: true, content: `${deviceId} is:\n    power: ${power}` };
}

const mocked = (fn: unknown) => fn as ReturnType<typeof vi.fn>;

beforeEach(() => {
  vi.clearAllMocks();
  // The Matter panel loads alongside the device list on every render.
  mocked(api.getMatterStatus).mockResolvedValue({
    enabled: true,
    url: "ws://127.0.0.1:5580/ws",
    state: "connected",
  });
  mocked(api.updateSettings).mockResolvedValue({});
});
afterEach(() => cleanup());

describe("Devices section — Matter devices", () => {
  it("shows a commissioned Matter device in the list", async () => {
    (api.listDevices as ReturnType<typeof vi.fn>).mockResolvedValue([matterLight]);

    render(<Devices />);

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

    // lucide-react renders `<svg class="lucide lucide-<name>">`.
    expect(container.querySelector(".lucide-lightbulb")).toBeTruthy();
    expect(container.querySelector(".lucide-lock")).toBeTruthy();
  });

  it("shows a switch icon for a Generic Switch, not the monitor fallback", async () => {
    (api.listDevices as ReturnType<typeof vi.fn>).mockResolvedValue([
      { id: "matter-6", name: "Generic Switch", device_type: "switch", is_online: true },
    ]);

    const { container } = render(<Devices />);
    await screen.findByText("Generic Switch");

    expect(container.querySelector(".lucide-toggle-left")).toBeTruthy();
    expect(container.querySelector(".lucide-monitor")).toBeNull();
  });

  it("shows a hub icon for a Matter bridge, not the monitor fallback", async () => {
    (api.listDevices as ReturnType<typeof vi.fn>).mockResolvedValue([
      { id: "matter-90", name: "Living Room Hub", device_type: "bridge", is_online: true },
    ]);

    const { container } = render(<Devices />);
    await screen.findByText("Living Room Hub");

    expect(container.querySelector(".lucide-router")).toBeTruthy();
    expect(container.querySelector(".lucide-monitor")).toBeNull();
  });

  it("shows a droplet for a water valve, not the monitor fallback", async () => {
    (api.listDevices as ReturnType<typeof vi.fn>).mockResolvedValue([
      { id: "matter-60", name: "Garden Valve", device_type: "valve", is_online: true },
    ]);

    const { container } = render(<Devices />);
    await screen.findByText("Garden Valve");

    expect(container.querySelector(".lucide-droplet")).toBeTruthy();
    expect(container.querySelector(".lucide-monitor")).toBeNull();
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

describe("Devices section — power button", () => {
  it("labels the button from what the device is, not from whether it is reachable", async () => {
    mocked(api.listDevices).mockResolvedValue([matterLight]);
    mocked(api.invokeTool).mockResolvedValue(stateSaying("matter-2", "off"));

    render(<Devices />);
    await screen.findByText("Living Room Light");

    await waitFor(() => expect(screen.getByText("Turn on")).toBeTruthy());
  });

  it("switches the device, and reads back rather than assuming", async () => {
    mocked(api.listDevices).mockResolvedValue([matterLight]);
    mocked(api.invokeTool).mockResolvedValue(stateSaying("matter-2", "on"));

    render(<Devices />);
    await screen.findByText("Living Room Light");
    await waitFor(() => expect(screen.getByText("Turn off")).toBeTruthy());

    mocked(api.invokeTool).mockClear();
    fireEvent.click(screen.getByText("Turn off"));

    await waitFor(() =>
      expect(api.invokeTool).toHaveBeenCalledWith({
        server: "giap-device-control",
        tool: "set_device_state",
        args: { device_id: "matter-2", power: false },
      }),
    );
    // Read back: a device that refused must not leave the card claiming otherwise.
    await waitFor(() =>
      expect(mocked(api.invokeTool).mock.calls.some((c) => c[0].tool === "get_device_state")).toBe(
        true,
      ),
    );
    expect(api.markDeviceOffline).not.toHaveBeenCalled();
  });

  it("offers no power button to a device that cannot be switched", async () => {
    mocked(api.listDevices).mockResolvedValue([contactSensor]);

    render(<Devices />);
    await screen.findByText("Contact Sensor");

    expect(screen.queryByText("Turn on")).toBeNull();
    expect(screen.queryByText("Turn off")).toBeNull();
    expect(api.invokeTool).not.toHaveBeenCalled();
  });

  it("moves reachability into Configure, named as what it is", async () => {
    // Manual reachability stays: with no wake/restart primitive it is the only way to age a device out.
    const offlineLight: Device = { ...matterLight, is_online: false };
    mocked(api.listDevices).mockResolvedValue([offlineLight]);
    mocked(api.markDeviceOnline).mockResolvedValue(undefined);

    render(<Devices />);
    await screen.findByText("Living Room Light");
    fireEvent.click(screen.getByText("Configure"));

    fireEvent.click(await screen.findByText("Mark online"));
    await waitFor(() => expect(api.markDeviceOnline).toHaveBeenCalledWith("matter-2"));
  });
});

describe("power state parsing", () => {
  it("reads the line shape `state_line` writes, and nothing else", async () => {
    const { powerStateOf } = await import("./Devices");

    expect(powerStateOf("matter-2 is:\n    power: on\n    brightness: 50%")).toBe(true);
    expect(powerStateOf("matter-2 is:\n    power: off")).toBe(false);
    // No report is not "off": guessing `false` would label every unreachable device "Turn on".
    expect(powerStateOf("matter-2 reports nothing about its state.")).toBeUndefined();
    expect(powerStateOf("matter-2 is:\n    brightness: 50%")).toBeUndefined();
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

  it("shows an online device as seen now, and an offline one as when it went quiet", async () => {
    // Online means vouched for now, so no heartbeat age; offline shows exactly that.
    const quiet = new Date(Date.now() - 3 * 60 * 60 * 1000).toISOString();
    mocked(api.listDevices).mockResolvedValue([
      { ...matterLight, is_online: true, last_seen: quiet },
      {
        id: "matter-7",
        name: "Old Sensor",
        device_type: "sensor",
        is_online: false,
        last_seen: quiet,
      },
    ]);
    mocked(api.getMatterStatus).mockResolvedValue({ enabled: true, state: "connected" });

    render(<Devices />);

    await waitFor(() => expect(screen.getByText("Living Room Light")).toBeTruthy());
    expect(screen.getByText("now")).toBeTruthy();
    expect(screen.getByText("3h ago")).toBeTruthy();
  });
});
