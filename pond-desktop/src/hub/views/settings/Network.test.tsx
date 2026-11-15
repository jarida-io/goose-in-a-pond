import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup, waitFor, act, within } from "@testing-library/react";

vi.mock("../../../api/PondApiClient", () => ({
  api: {
    getSettings: vi.fn(),
    updateSettings: vi.fn(),
    getConnections: vi.fn(),
    getSetupStatus: vi.fn(),
    getSystemInfo: vi.fn(),
    scanWifi: vi.fn(),
    joinWifi: vi.fn(),
    forgetWifi: vi.fn(),
  },
}));

import { NetworkDetail } from "./Network";
import { api } from "../../../api/PondApiClient";
import { SETUP_CLAIMED, SETUP_WAITING, makeConnections } from "../../../network/connectionsFixtures";

const getConnections = vi.mocked(api.getConnections);

async function renderScreen(go: (route: string) => void = () => {}) {
  render(<NetworkDetail go={go} />);
  await act(async () => {});
}

afterEach(() => {
  cleanup();
  vi.useRealTimers();
});
beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(api.getSettings).mockResolvedValue({} as never);
  vi.mocked(api.updateSettings).mockImplementation(async (patch) => patch as never);
  getConnections.mockResolvedValue(makeConnections());
  vi.mocked(api.getSetupStatus).mockResolvedValue(SETUP_CLAIMED);
  vi.mocked(api.getSystemInfo).mockResolvedValue({ platform: "linux" } as never);
});

describe("Hub Connections screen", () => {
  it("shows Wi-Fi, Ethernet, Bluetooth and Pairing from one read of the pond", async () => {
    await renderScreen();
    expect(screen.getByRole("heading", { name: "Connections" })).toBeTruthy();
    expect(Array.from(document.querySelectorAll(".setcard__title"), (n) => n.textContent)).toEqual([
      "Wi-Fi",
      "Ethernet",
      "Bluetooth",
      "Pairing",
    ]);

    expect(screen.getByText("192.168.1.11")).toBeTruthy();
    expect(screen.getByText("eth0")).toBeTruthy();
    expect(screen.getByText("Visible to phones")).toBeTruthy();
    expect(screen.getByText(/^Claimed on /)).toBeTruthy();
    expect(getConnections).toHaveBeenCalledTimes(1);
  });

  it("says a pond waiting to be claimed is waiting, with its card", async () => {
    vi.mocked(api.getSetupStatus).mockResolvedValue(SETUP_WAITING);
    await renderScreen();
    expect(screen.getByText("Waiting to be claimed (card 4e81)")).toBeTruthy();
  });

  it("goes back to Settings", async () => {
    const go = vi.fn();
    await renderScreen(go);
    fireEvent.click(screen.getByRole("button", { name: "Back to Settings" }));
    expect(go).toHaveBeenCalledWith("settings");
  });

  it("says there is no wired connection, quietly, when there is none", async () => {
    getConnections.mockResolvedValue(makeConnections({ ethernet: [] }));
    await renderScreen();
    expect(screen.getByText("No wired connection")).toBeTruthy();
  });
});

// The Hub's Toggle keeps its own state, so assert what is drawn after the async settings load.
function spokenCodesSwitch(): HTMLButtonElement {
  return screen.getByRole("switch", { name: "Say pairing codes aloud" }) as HTMLButtonElement;
}
function bleSetupSwitch(): HTMLButtonElement {
  return screen.getByRole("switch", { name: "Set up over Bluetooth" }) as HTMLButtonElement;
}

describe("Hub Connections: spoken pairing codes", () => {
  it("draws ON when the key is absent, as the server ships it", async () => {
    await renderScreen();
    expect(spokenCodesSwitch().getAttribute("aria-checked")).toBe("true");
  });

  it("draws OFF when the household turned it off", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ spoken_pairing_codes: false } as never);
    await renderScreen();
    await waitFor(() => expect(spokenCodesSwitch().getAttribute("aria-checked")).toBe("false"));
  });

  it("saves the switch as spoken_pairing_codes", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ spoken_pairing_codes: true } as never);
    await renderScreen();
    fireEvent.click(spokenCodesSwitch());
    await waitFor(() => expect(api.updateSettings).toHaveBeenCalledWith({ spoken_pairing_codes: false }));
  });
});

describe("Hub Connections: setup over Bluetooth", () => {
  it("draws ON when the key is absent, as the server ships it", async () => {
    await renderScreen();
    expect(bleSetupSwitch().getAttribute("aria-checked")).toBe("true");
  });

  it("draws OFF when the household turned it off", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ ble_setup_enabled: false } as never);
    await renderScreen();
    await waitFor(() => expect(bleSetupSwitch().getAttribute("aria-checked")).toBe("false"));
  });

  it("saves the switch as ble_setup_enabled", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ ble_setup_enabled: true } as never);
    await renderScreen();
    fireEvent.click(bleSetupSwitch());
    await waitFor(() => expect(api.updateSettings).toHaveBeenCalledWith({ ble_setup_enabled: false }));
  });

  it("reads the radio's state again after the switch is saved, once it has had a moment to follow", async () => {
    vi.useFakeTimers();
    await renderScreen();
    expect(getConnections).toHaveBeenCalledTimes(1);

    await act(async () => {
      fireEvent.click(bleSetupSwitch());
    });
    expect(api.updateSettings).toHaveBeenCalledWith({ ble_setup_enabled: false });
    expect(getConnections).toHaveBeenCalledTimes(1);

    getConnections.mockResolvedValue(
      makeConnections({ bluetooth: { enabled: false, state: "off", reason: null, advertising_as: null, session_open: false } }),
    );
    await act(() => vi.advanceTimersByTimeAsync(1_300));
    expect(getConnections).toHaveBeenCalledTimes(2);
    expect(screen.getByText(/Setup over Bluetooth is switched off/)).toBeTruthy();
  });

  it("puts the switch back and says so when it could not be saved", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ ble_setup_enabled: true } as never);
    vi.mocked(api.updateSettings).mockRejectedValue(new Error("boom"));
    await renderScreen();
    fireEvent.click(bleSetupSwitch());
    await screen.findByText("That switch was not saved. Try again.");
    await waitFor(() => expect(bleSetupSwitch().getAttribute("aria-checked")).toBe("true"));
  });

  it("keeps the switch inside the Bluetooth card, under its status", async () => {
    await renderScreen();
    const card = screen.getByText("Bluetooth", { selector: ".setcard__title" }).closest(".setcard") as HTMLElement;
    expect(within(card).getByText("Visible to phones")).toBeTruthy();
    expect(within(card).getByRole("switch", { name: "Set up over Bluetooth" })).toBeTruthy();
  });
});
