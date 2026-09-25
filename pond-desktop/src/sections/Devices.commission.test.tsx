import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { Devices } from "./Devices";
import { api } from "../api/PondApiClient";
import { ApiError, type MatterStatus } from "../api/types";

vi.mock("../api/PondApiClient", () => ({
  api: {
    listDevices: vi.fn(),
    registerDevice: vi.fn(),
    unregisterDevice: vi.fn(),
    commissionDevice: vi.fn(),
    getMatterStatus: vi.fn(),
    updateSettings: vi.fn(),
    invokeTool: vi.fn(),
  },
}));

const mocked = (fn: unknown) => fn as ReturnType<typeof vi.fn>;

/** A Matter runtime in the given state, as `GET /matter/status` reports it. */
function matterStatus(state: MatterStatus["state"], error?: string): MatterStatus {
  return { enabled: state !== "disabled", url: "ws://127.0.0.1:5580/giap", state, error };
}

beforeEach(() => {
  vi.clearAllMocks();
  mocked(api.listDevices).mockResolvedValue([]);
  // Commissioning needs a live controller, so that's the baseline; other states are set per test.
  mocked(api.getMatterStatus).mockResolvedValue(matterStatus("connected"));
  mocked(api.updateSettings).mockResolvedValue({});
});
afterEach(() => cleanup());

/** Open the add-device modal, once the Matter state has settled. */
async function openModal() {
  render(<Devices />);
  // The empty state and the header both offer it; take the header button.
  const buttons = await screen.findAllByText("Register device");
  await waitFor(() => expect(api.getMatterStatus).toHaveBeenCalled());
  fireEvent.click(buttons[0]);
}

describe("Add device — Matter vs other", () => {
  it("asks for a setup code, not a description, when adding a Matter device", async () => {
    await openModal();

    expect(await screen.findByText("Setup code")).toBeTruthy();
    // No manual-entry fields: a commissioned device reports its own name and type.
    expect(screen.queryByText("Device type")).toBeNull();
    expect(screen.queryByText("Hostname (optional)")).toBeNull();
  });

  it("commissions with the entered code and refreshes the list", async () => {
    mocked(api.commissionDevice).mockResolvedValue({
      id: "matter-2",
      name: "Virtual OnOff Light",
      node_id: 2,
    });
    await openModal();

    const input = await screen.findByPlaceholderText(/20202021/);
    // The Matter Virtual Device's passcode, exactly as its app shows it.
    fireEvent.change(input, { target: { value: "20202021" } });
    fireEvent.click(screen.getByText("Commission"));

    await waitFor(() => expect(api.commissionDevice).toHaveBeenCalledWith("20202021", undefined));
    // Never a manual registry write: the bridge registers what it commissions.
    expect(api.registerDevice).not.toHaveBeenCalled();
    await waitFor(() => expect(mocked(api.listDevices).mock.calls.length).toBeGreaterThan(1));
  });

  it("passes a chosen name through so the device is named on commission", async () => {
    mocked(api.commissionDevice).mockResolvedValue({
      id: "matter-2",
      name: "Living Room Light",
      node_id: 2,
    });
    await openModal();

    fireEvent.change(await screen.findByPlaceholderText(/20202021/), {
      target: { value: "20202021" },
    });
    // The optional name field, alongside the code — not a separate mode.
    fireEvent.change(screen.getByPlaceholderText("Living Room Light"), {
      target: { value: "Living Room Light" },
    });
    fireEvent.click(screen.getByText("Commission"));

    await waitFor(() =>
      expect(api.commissionDevice).toHaveBeenCalledWith("20202021", "Living Room Light"),
    );
  });

  it("surfaces a commissioning failure instead of closing silently", async () => {
    mocked(api.commissionDevice).mockRejectedValue(new Error("device not found on the network"));
    await openModal();

    fireEvent.change(await screen.findByPlaceholderText(/20202021/), {
      target: { value: "20202021" },
    });
    fireEvent.click(screen.getByText("Commission"));

    expect(await screen.findByText(/device not found on the network/)).toBeTruthy();
    // Modal stays open so the code can be corrected.
    expect(screen.getByText("Setup code")).toBeTruthy();
  });

  it("shows the pairing-mode guidance verbatim, since it is the fix as well as the reason", async () => {
    // The server's copy for the commonest failure: a closed 15-minute pairing window.
    const guidance =
      "No device found in pairing mode. Put the device into pairing mode and try again — " +
      "a Matter device stops accepting new connections about 15 minutes after it starts.";
    mocked(api.commissionDevice).mockRejectedValue(new Error(guidance));
    await openModal();

    fireEvent.change(await screen.findByPlaceholderText(/20202021/), {
      target: { value: "20202021" },
    });
    fireEvent.click(screen.getByText("Commission"));

    // In fragments: the copy has an em dash and wraps, so an exact match would test layout.
    expect(await screen.findByText(/No device found in pairing mode/)).toBeTruthy();
    expect(screen.getByText(/15 minutes after it starts/)).toBeTruthy();
    // The code stays put: the device is what needs attention, not the input.
    expect((screen.getByPlaceholderText(/20202021/) as HTMLInputElement).value).toBe("20202021");
  });

  it("offers no path but Matter, because there is no other kind to add", async () => {
    await openModal();

    expect(await screen.findByText("Setup code")).toBeTruthy();
    expect(screen.queryByText("Other device")).toBeNull();
    expect(screen.queryByText("Device type")).toBeNull();
    expect(screen.queryByText("Register")).toBeNull();
  });
});

describe("Matter section — turning the fabric on", () => {
  it("shows the runtime's state, not just the saved setting", async () => {
    mocked(api.getMatterStatus).mockResolvedValue(matterStatus("connecting"));
    render(<Devices />);

    expect((await screen.findByTestId("matter-state")).textContent).toBe("Starting…");
  });

  it("offers nothing to switch on, because there is nothing to switch on", async () => {
    render(<Devices />);
    await screen.findByTestId("matter-state");

    // The controller address is an operator setting, in Settings.
    expect(screen.queryByLabelText("Enable Matter")).toBeNull();
    expect(screen.queryByLabelText("Controller address")).toBeNull();
  });

  it("blocks commissioning until the controller is up, and says which it is", async () => {
    mocked(api.getMatterStatus).mockResolvedValue(matterStatus("connecting"));
    await openModal();

    fireEvent.change(await screen.findByPlaceholderText(/20202021/), {
      target: { value: "20202021" },
    });
    expect(screen.getByText("Commission").closest("button")?.disabled).toBe(true);
    expect(screen.getByTestId("matter-not-ready").textContent).toMatch(/still starting up/);
  });

  it("reports an unreachable controller as its own problem, and retries in place", async () => {
    mocked(api.getMatterStatus).mockResolvedValue(
      matterStatus("unreachable", "connection refused"),
    );
    render(<Devices />);

    expect((await screen.findByTestId("matter-state")).textContent).toBe(
      "Cannot reach controller",
    );
    expect(screen.getByText(/connection refused/)).toBeTruthy();

    // Retry re-sends the same address: the runtime compares against what's running, so that is a retry.
    fireEvent.click(screen.getByText("Retry"));
    await waitFor(() =>
      expect(api.updateSettings).toHaveBeenCalledWith({
        matter_ws_url: "ws://127.0.0.1:5580/giap",
      }),
    );
  });

  it("shows the server's message without the ApiError class name", async () => {
    // A real 503 body from `routes.rs`; keep it one the server can still produce.
    const serverMessage =
      "Matter is still starting up — the controller is not ready yet.";
    mocked(api.commissionDevice).mockRejectedValue(
      new ApiError(503, serverMessage),
    );
    await openModal();

    fireEvent.change(await screen.findByPlaceholderText(/20202021/), {
      target: { value: "20202021" },
    });
    fireEvent.click(screen.getByText("Commission"));

    const message = await screen.findByText(/Matter is still starting up/);
    expect(message.textContent).toBe(serverMessage);
    expect(message.textContent).not.toMatch(/ApiError/);
  });
});
