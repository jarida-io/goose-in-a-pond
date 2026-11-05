import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor, cleanup, fireEvent, within } from "@testing-library/react";
import { Devices } from "./Devices";
import { powerStateOf } from "./devicePower";
import { api } from "../api/PondApiClient";
import { ConfirmProvider } from "../components/shared";
import type { Device, Room } from "../api/types";

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
    listRooms: vi.fn(),
    createRoom: vi.fn(),
    renameRoom: vi.fn(),
    deleteRoom: vi.fn(),
    setRoomGuestAccess: vi.fn(),
  },
}));

function renderDevices() {
  return render(
    <ConfirmProvider>
      <Devices />
    </ConfirmProvider>,
  );
}

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

const livingRoom: Room = {
  id: "room-living",
  name: "Living Room",
  created_at: new Date().toISOString(),
  guest_accessible: false,
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
  // Most tests don't care about rooms; those that do override this.
  mocked(api.listRooms).mockResolvedValue([]);
});
afterEach(() => cleanup());

describe("Devices section — Matter devices", () => {
  it("shows a commissioned Matter device in the list", async () => {
    (api.listDevices as ReturnType<typeof vi.fn>).mockResolvedValue([matterLight]);

    renderDevices();

    expect(await screen.findByText("Living Room Light")).toBeTruthy();
    expect(screen.getByText("light")).toBeTruthy();
  });

  it("renders type-appropriate icons for Matter device types", async () => {
    (api.listDevices as ReturnType<typeof vi.fn>).mockResolvedValue([
      matterLight,
      matterLock,
    ]);

    const { container } = renderDevices();
    await screen.findByText("Living Room Light");

    // lucide-react renders `<svg class="lucide lucide-<name>">`.
    expect(container.querySelector(".lucide-lightbulb")).toBeTruthy();
    expect(container.querySelector(".lucide-lock")).toBeTruthy();
  });

  it("shows a switch icon for a Generic Switch, not the monitor fallback", async () => {
    (api.listDevices as ReturnType<typeof vi.fn>).mockResolvedValue([
      { id: "matter-6", name: "Generic Switch", device_type: "switch", is_online: true },
    ]);

    const { container } = renderDevices();
    await screen.findByText("Generic Switch");

    expect(container.querySelector(".lucide-toggle-left")).toBeTruthy();
    expect(container.querySelector(".lucide-monitor")).toBeNull();
  });

  it("shows a hub icon for a Matter bridge, not the monitor fallback", async () => {
    (api.listDevices as ReturnType<typeof vi.fn>).mockResolvedValue([
      { id: "matter-90", name: "Living Room Hub", device_type: "bridge", is_online: true },
    ]);

    const { container } = renderDevices();
    await screen.findByText("Living Room Hub");

    expect(container.querySelector(".lucide-router")).toBeTruthy();
    expect(container.querySelector(".lucide-monitor")).toBeNull();
  });

  it("shows a droplet for a water valve, not the monitor fallback", async () => {
    (api.listDevices as ReturnType<typeof vi.fn>).mockResolvedValue([
      { id: "matter-60", name: "Garden Valve", device_type: "valve", is_online: true },
    ]);

    const { container } = renderDevices();
    await screen.findByText("Garden Valve");

    expect(container.querySelector(".lucide-droplet")).toBeTruthy();
    expect(container.querySelector(".lucide-monitor")).toBeNull();
  });

  it("shows a phone icon for the GOTG mobile companion, not a computer", async () => {
    (api.listDevices as ReturnType<typeof vi.fn>).mockResolvedValue([
      { id: "phone-1", name: "Emmanuel's Phone", device_type: "gotg", is_online: true },
    ]);

    const { container } = renderDevices();
    await screen.findByText("Emmanuel's Phone");

    expect(container.querySelector(".lucide-smartphone")).toBeTruthy();
    expect(container.querySelector(".lucide-monitor")).toBeNull();
  });
});

describe("Devices section — power button", () => {
  it("labels the button from what the device is, not from whether it is reachable", async () => {
    mocked(api.listDevices).mockResolvedValue([matterLight]);
    mocked(api.invokeTool).mockResolvedValue(stateSaying("matter-2", "off"));

    renderDevices();
    await screen.findByText("Living Room Light");

    await waitFor(() => expect(screen.getByText("Turn on")).toBeTruthy());
  });

  it("switches the device, and reads back rather than assuming", async () => {
    mocked(api.listDevices).mockResolvedValue([matterLight]);
    mocked(api.invokeTool).mockResolvedValue(stateSaying("matter-2", "on"));

    renderDevices();
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

    renderDevices();
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

    renderDevices();
    await screen.findByText("Living Room Light");
    fireEvent.click(screen.getByText("Configure"));

    fireEvent.click(await screen.findByText("Mark online"));
    await waitFor(() => expect(api.markDeviceOnline).toHaveBeenCalledWith("matter-2"));
  });
});

describe("power state parsing", () => {
  it("reads the line shape `state_line` writes, and nothing else", () => {
    expect(powerStateOf("matter-2 is:\n    power: on\n    brightness: 50%")).toBe(true);
    expect(powerStateOf("matter-2 is:\n    power: off")).toBe(false);
    // No report is not "off": guessing `false` would label every unreachable device "Turn on".
    expect(powerStateOf("matter-2 reports nothing about its state.")).toBeUndefined();
    expect(powerStateOf("matter-2 is:\n    brightness: 50%")).toBeUndefined();
  });
});

describe("Devices section — Configure modal", () => {
  it("opens editable, pre-filled with the device's current name/hostname/room", async () => {
    const withHostname: Device = {
      ...matterLight,
      hostname: "light.local",
      room_id: livingRoom.id,
    };
    mocked(api.listDevices).mockResolvedValue([withHostname]);
    mocked(api.listRooms).mockResolvedValue([livingRoom]);

    renderDevices();
    await screen.findByText("Living Room Light");

    fireEvent.click(screen.getByText("Configure"));

    const nameInput = await screen.findByDisplayValue("Living Room Light");
    expect(nameInput.tagName).toBe("INPUT");
    expect(screen.getByDisplayValue("light.local")).toBeTruthy();
    // The room picker is a <select>, pre-selected to the device's current room.
    const roomSelect = screen.getByDisplayValue("Living Room") as HTMLSelectElement;
    expect(roomSelect.tagName).toBe("SELECT");
  });

  it("saving edited fields calls updateDevice with the new room_id", async () => {
    const kitchen: Room = { ...livingRoom, id: "room-kitchen", name: "Kitchen" };
    mocked(api.listDevices).mockResolvedValue([matterLight]);
    mocked(api.listRooms).mockResolvedValue([livingRoom, kitchen]);
    mocked(api.updateDevice).mockResolvedValue({
      ...matterLight,
      name: "Renamed Light",
      room_id: kitchen.id,
    });

    renderDevices();
    await screen.findByText("Living Room Light");
    fireEvent.click(screen.getByText("Configure"));

    const nameInput = await screen.findByDisplayValue("Living Room Light");
    fireEvent.change(nameInput, { target: { value: "Renamed Light" } });

    const roomSelect = screen.getByDisplayValue("Unassigned") as HTMLSelectElement;
    fireEvent.change(roomSelect, { target: { value: kitchen.id } });

    fireEvent.click(screen.getByText("Save"));

    await waitFor(() =>
      expect(api.updateDevice).toHaveBeenCalledWith("matter-2", {
        name: "Renamed Light",
        hostname: undefined,
        room_id: kitchen.id,
      }),
    );
  });

  it("clearing the room picker back to Unassigned sends room_id: null", async () => {
    const assigned: Device = { ...matterLight, room_id: livingRoom.id };
    mocked(api.listDevices).mockResolvedValue([assigned]);
    mocked(api.listRooms).mockResolvedValue([livingRoom]);
    mocked(api.updateDevice).mockResolvedValue({ ...assigned, room_id: null });

    renderDevices();
    await screen.findByText("Living Room Light");
    fireEvent.click(screen.getByText("Configure"));

    const roomSelect = await screen.findByDisplayValue("Living Room") as HTMLSelectElement;
    fireEvent.change(roomSelect, { target: { value: "" } });
    fireEvent.click(screen.getByText("Save"));

    await waitFor(() =>
      expect(api.updateDevice).toHaveBeenCalledWith(
        "matter-2",
        expect.objectContaining({ room_id: null }),
      ),
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

    renderDevices();

    await waitFor(() => expect(screen.getByText("Living Room Light")).toBeTruthy());
    expect(screen.getByText("now")).toBeTruthy();
    expect(screen.getByText("3h ago")).toBeTruthy();
  });
});

describe("Devices section — rooms", () => {
  it("groups devices under their room, and unassigned devices under 'Unassigned'", async () => {
    const kitchen: Room = { ...livingRoom, id: "room-kitchen", name: "Kitchen" };
    mocked(api.listRooms).mockResolvedValue([livingRoom, kitchen]);
    mocked(api.listDevices).mockResolvedValue([
      { ...matterLight, room_id: livingRoom.id },
      { ...contactSensor, room_id: null },
    ]);

    renderDevices();

    expect(await screen.findByText("Living Room")).toBeTruthy();
    expect(screen.getByText("Kitchen")).toBeTruthy();
    expect(screen.getByText("Unassigned")).toBeTruthy();
    // Kitchen has no devices placed in it.
    expect(screen.getByText("No devices in this room yet.")).toBeTruthy();
  });

  it("with no named rooms, every device falls under a single Unassigned section", async () => {
    mocked(api.listRooms).mockResolvedValue([]);
    mocked(api.listDevices).mockResolvedValue([matterLight]);

    renderDevices();

    expect(await screen.findByText("Living Room Light")).toBeTruthy();
    expect(screen.getByText("Unassigned")).toBeTruthy();
  });

  it("creating a room calls createRoom and reloads", async () => {
    mocked(api.listDevices).mockResolvedValue([]);
    mocked(api.createRoom).mockResolvedValue({ ...livingRoom, id: "room-new", name: "Office" });

    renderDevices();
    await waitFor(() => expect(api.listRooms).toHaveBeenCalled());

    fireEvent.click(screen.getByText("New room"));
    const nameInput = await screen.findByPlaceholderText("Kitchen");
    fireEvent.change(nameInput, { target: { value: "Office" } });
    fireEvent.click(screen.getByText("Save"));

    await waitFor(() => expect(api.createRoom).toHaveBeenCalledWith("Office"));
  });

  it("toggling guest access calls setRoomGuestAccess, and reverts on failure", async () => {
    mocked(api.listRooms).mockResolvedValue([livingRoom]);
    mocked(api.listDevices).mockResolvedValue([]);
    mocked(api.setRoomGuestAccess).mockRejectedValue(new Error("offline"));

    renderDevices();
    await screen.findByText("Living Room");

    const toggle = screen.getByRole("switch", {
      name: /let a guest use devices in living room/i,
    }) as HTMLInputElement;
    fireEvent.click(toggle);

    await waitFor(() =>
      expect(api.setRoomGuestAccess).toHaveBeenCalledWith(livingRoom.id, true),
    );
    // Reverted: the call failed, so the switch should read as it did before the click.
    await waitFor(() => expect(toggle.checked).toBe(false));
  });

  it("deleting a room asks for confirmation, then calls deleteRoom", async () => {
    mocked(api.listRooms).mockResolvedValue([livingRoom]);
    mocked(api.listDevices).mockResolvedValue([]);
    mocked(api.deleteRoom).mockResolvedValue(undefined);

    renderDevices();
    await screen.findByText("Living Room");

    fireEvent.click(screen.getByText("Delete"));
    const dialog = await screen.findByRole("dialog");
    fireEvent.click(within(dialog).getByRole("button", { name: "Delete" }));

    await waitFor(() => expect(api.deleteRoom).toHaveBeenCalledWith(livingRoom.id));
  });
});
