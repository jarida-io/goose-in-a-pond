// A device that didn't answer reads "Not reporting", never "Off": that's a claim about the world.

import { render, screen, cleanup, waitFor, fireEvent } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

const listDevices = vi.fn();
const invokeTool = vi.fn();

vi.mock("../../../api/PondApiClient", () => ({
  api: {
    listDevices: (...args: unknown[]) => listDevices(...args),
    invokeTool: (...args: unknown[]) => invokeTool(...args),
  },
}));

// hubDataStore is mocked: importing the real one fires a full dashboard load.
const DEVICES = [
  { id: "lamp", name: "Hall Lamp", kind: "light", room: "Hall" },
  { id: "sensor", name: "Back Door", kind: "other", room: "Kitchen" },
];
let home: { devices: typeof DEVICES; devicesAreReal: boolean };
vi.mock("../../state/hubDataStore", () => ({ useHomeData: () => home }));

import { HomeControlsCard } from "./HomeControlsCard";

function mount(limit = 6) {
  return render(<HomeControlsCard limit={limit} onManageDevices={() => {}} />);
}

beforeEach(() => {
  cleanup();
  listDevices.mockReset();
  invokeTool.mockReset();
  home = { devices: DEVICES, devicesAreReal: true };
  listDevices.mockResolvedValue([
    { id: "lamp", name: "Hall Lamp", capabilities: ["power"] },
    { id: "sensor", name: "Back Door", capabilities: [] },
  ]);
});

describe("a device that did not answer", () => {
  it("reads as not reporting, never as off", async () => {
    invokeTool.mockRejectedValue(new Error("tool unavailable"));
    mount();

    expect(await screen.findByText("Not reporting")).toBeTruthy();
    expect(screen.queryByText("Off")).toBeNull();
    expect(screen.queryByText("On")).toBeNull();
  });

  /** `request<T>` returns `undefined` or `index.html` for an empty or misrouted reply. */
  it("treats an unparseable reply the same way", async () => {
    invokeTool.mockResolvedValue({ tool: "get_device_state", success: true, content: "<html>" });
    mount();
    expect(await screen.findByText("Not reporting")).toBeTruthy();
    expect(screen.queryByText("Off")).toBeNull();
  });

  /** A toggle whose direction is unknown cannot be labelled, so it opens the sheet instead. */
  it("offers the control sheet rather than a switch", async () => {
    invokeTool.mockRejectedValue(new Error("tool unavailable"));
    mount();
    const tile = await screen.findByRole("button", {
      name: "Hall Lamp, not reporting — open controls",
    });
    expect(tile.getAttribute("aria-pressed")).toBeNull();
  });
});

describe("a device with no power capability", () => {
  it("is shown, and is offered no control", async () => {
    invokeTool.mockRejectedValue(new Error("tool unavailable"));
    mount();

    expect(await screen.findByText("Back Door")).toBeTruthy();
    // A tile but not a button: a contact sensor has no switch (DESIGN.md §3).
    expect(screen.queryByRole("button", { name: /Back Door/ })).toBeNull();
  });

  it("is never asked what its switch is doing", async () => {
    invokeTool.mockResolvedValue({ tool: "get_device_state", success: true, content: "power: on" });
    mount();

    await screen.findByText("On");
    const asked = invokeTool.mock.calls.map((c) => (c[0] as { args: { device_id: string } }).args.device_id);
    expect(asked).not.toContain("sensor");
  });
});

describe("a toggle", () => {
  /** A dispatch that returns says the tool ran, not that the lamp moved. */
  it("shows the re-read, not the write", async () => {
    invokeTool.mockResolvedValue({ tool: "get_device_state", success: true, content: "power: off" });
    mount();
    const tile = await screen.findByRole("button", { name: "Hall Lamp, off" });

    // The write succeeds but the device still says off.
    fireEvent.click(tile);
    await waitFor(() => {
      const calls = invokeTool.mock.calls.map((c) => (c[0] as { tool: string }).tool);
      expect(calls).toContain("set_device_state");
    });
    await waitFor(() => expect(screen.getByText("Off")).toBeTruthy());
    expect(screen.queryByText("On")).toBeNull();
  });

  it("lands on not reporting when the re-read fails", async () => {
    invokeTool.mockResolvedValue({ tool: "get_device_state", success: true, content: "power: off" });
    mount();
    const tile = await screen.findByRole("button", { name: "Hall Lamp, off" });

    invokeTool.mockRejectedValue(new Error("gone"));
    fireEvent.click(tile);

    expect(await screen.findByText("Not reporting")).toBeTruthy();
    expect(screen.queryByText("Off")).toBeNull();
  });
});

describe("a house with nothing in it", () => {
  it("invites the first device rather than borrowing a demo one", async () => {
    listDevices.mockResolvedValue([]);
    mount();
    expect(await screen.findByRole("button", { name: /Add your first device/ })).toBeTruthy();
  });
});

/** Loading, failed and empty each say something different. */
describe("before the pond has answered", () => {
  it("says it is still looking, and claims nothing about the house", async () => {
    let release: (v: unknown[]) => void = () => {};
    listDevices.mockReturnValue(new Promise((r) => (release = r)));

    mount();

    expect(await screen.findByText(/Checking what the house holds/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: /Add your first device/ })).toBeNull();
    expect(screen.queryByText(/Could not reach the pond/)).toBeNull();

    release([{ id: "lamp", name: "Hall Lamp", capabilities: [] }]);
    expect(await screen.findByText("Hall Lamp")).toBeTruthy();
  });

  /** Until the store settles it holds mockHome, whose demo ids never intersect the wire's. */
  it("does not call a house empty while the store is still holding the demo one", async () => {
    home = { devices: [{ id: "driveway", name: "Driveway Cam", kind: "camera", room: "Outdoor" }], devicesAreReal: false };
    listDevices.mockResolvedValue([{ id: "9f1c-real-uuid", name: "Hall Lamp", capabilities: ["power"] }]);

    const view = mount();

    expect(await screen.findByText(/Checking what the house holds/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: /Add your first device/ })).toBeNull();
    // The demo house is never painted either: its id is not on the wire.
    expect(screen.queryByText("Driveway Cam")).toBeNull();

    home = {
      devices: [{ id: "9f1c-real-uuid", name: "Hall Lamp", kind: "light", room: "Hall" }],
      devicesAreReal: true,
    };
    view.rerender(<HomeControlsCard limit={6} onManageDevices={() => {}} />);

    expect(await screen.findByText("Hall Lamp")).toBeTruthy();
    expect(screen.queryByRole("button", { name: /Add your first device/ })).toBeNull();
  });
});

describe("when the device list cannot be read", () => {
  it("says so, rather than rendering a card with nothing in it", async () => {
    listDevices.mockRejectedValue(new Error("connection refused"));

    mount();

    expect(await screen.findByText(/Could not reach the pond/)).toBeTruthy();
    expect(screen.queryByRole("button", { name: /Add your first device/ })).toBeNull();
    expect(screen.queryByText(/Checking what the house holds/)).toBeNull();
  });

  /** A launch read often fails before the sidecar serves; the retry is the way back without a reload. */
  it("offers the read again, and takes it", async () => {
    listDevices.mockRejectedValueOnce(new Error("connection refused"));

    mount();

    const retry = await screen.findByRole("button", { name: "Try again" });
    fireEvent.click(retry);

    // The press answers at once, not after the second request.
    expect(screen.getByText(/Checking what the house holds/)).toBeTruthy();
    expect(await screen.findByText("Hall Lamp")).toBeTruthy();
    expect(screen.queryByText(/Could not reach the pond/)).toBeNull();
    expect(listDevices.mock.calls.length).toBeGreaterThan(1);
  });
});

describe("the dashboard or the desktop app on Home", () => {
  it("reads as open or not open, never as offline", async () => {
    home = {
      devices: [
        { id: "web", name: "Pond dashboard", kind: "other", room: "Unassigned" },
        { id: "app", name: "Pond Desktop", kind: "other", room: "Unassigned" },
      ],
      devicesAreReal: true,
    };
    listDevices.mockResolvedValue([
      { id: "web", name: "Pond dashboard", device_type: "desktop", is_online: false, capabilities: [] },
      { id: "app", name: "Pond Desktop", device_type: "desktop", is_online: true, capabilities: [] },
    ]);
    mount();

    expect(await screen.findByText("Not open")).toBeTruthy();
    expect(screen.getByText("Open")).toBeTruthy();
    expect(screen.queryByText(/Offline/)).toBeNull();
    expect(screen.queryByText("Online")).toBeNull();
  });
});
