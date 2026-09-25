// The device tiles, and the one thing they are not allowed to do.
//
// This is the audit's largest fix, held down by a test: the old Home read a
// real device id out of a mock map that answered every id with a hardcoded
// {on:false, locked:true, target:70, brightness:40, watts:42}. Every tile on
// the screen therefore reported a switch position, a setpoint and a wattage
// that came from a literal in the source.
//
// The rule the card now follows is narrow and worth pinning: a device that did
// not answer reads "Not reporting" and never "Off". "Off" is a claim about the
// world, and a failed read is a claim about the read.

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

// Mocked rather than exercised: the real module fires a full dashboard load on
// import in a browser-like environment, and this card's own reads are the
// subject here. `devicesAreReal` is part of the fixture because the card reads
// it — it is the store's own flag for "these devices came off the wire, not out
// of mockHome", and the tests below drive it in both positions.
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

  /**
   * A reply this cannot parse is the same answer as no reply. `request<T>` hands
   * back `undefined` or `index.html` for an empty or misrouted response, and
   * both would reach `powerStateOf` as something that is not a state.
   */
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
    // Present as a tile, absent from the buttons: a contact sensor has no
    // switch, and a switch it cannot perform is the thing DESIGN.md §3 forbids.
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
  /**
   * The write's own result is never displayed. A dispatch that returns without
   * throwing says the tool ran, not that the lamp moved.
   */
  it("shows the re-read, not the write", async () => {
    invokeTool.mockResolvedValue({ tool: "get_device_state", success: true, content: "power: off" });
    mount();
    const tile = await screen.findByRole("button", { name: "Hall Lamp, off" });

    // The write succeeds and the device still says off — a lamp that did not
    // move. The tile must keep saying off.
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

/**
 * The three non-happy states, which used to be one.
 *
 * `capabilities === null` covered both "has not answered" and "threw", and
 * neither reached the empty state or the grid, so both rendered a card holding
 * an icon and the word "Devices" — in a 196-280px frame, with no retry. The
 * distinction matters to a household: one of those sentences says wait and the
 * other says the pond was not reachable.
 */
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

  /**
   * The store seeds `state.data` with mockHome, whose ids are demo strings
   * ("driveway", "thermo") while a real one is a UUID. The card intersects the
   * store's list with its own, so before the store settles the intersection is
   * empty by construction — and calling that "no devices" is a false statement
   * about a house that has a lamp in it.
   */
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
    // Not "you have no devices", and not the silent blank: both are claims the
    // failed read did not earn.
    expect(screen.queryByRole("button", { name: /Add your first device/ })).toBeNull();
    expect(screen.queryByText(/Checking what the house holds/)).toBeNull();
  });

  /**
   * The read fired once on mount with `[]` deps, so a shell that painted before
   * the sidecar was serving stayed blank for the life of the mount even after
   * the pond came back. The retry is the only way back without a reload.
   */
  it("offers the read again, and takes it", async () => {
    listDevices.mockRejectedValueOnce(new Error("connection refused"));

    mount();

    const retry = await screen.findByRole("button", { name: "Try again" });
    fireEvent.click(retry);

    // The press answers immediately rather than leaving the failure standing for
    // however long the second request takes.
    expect(screen.getByText(/Checking what the house holds/)).toBeTruthy();
    expect(await screen.findByText("Hall Lamp")).toBeTruthy();
    expect(screen.queryByText(/Could not reach the pond/)).toBeNull();
    expect(listDevices.mock.calls.length).toBeGreaterThan(1);
  });
});
