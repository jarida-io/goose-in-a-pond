import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup, waitFor } from "@testing-library/react";

vi.mock("../../../api/PondApiClient", () => ({
  api: {
    getSettings: vi.fn(),
    updateSettings: vi.fn(),
    listSessions: vi.fn(),
  },
}));

import { PrivacyDetail } from "./Privacy";
import { api } from "../../../api/PondApiClient";

function spokenCodesSwitch(): HTMLButtonElement {
  return screen.getByRole("button", { name: "Say pairing codes aloud" }) as HTMLButtonElement;
}

async function renderPrivacy() {
  render(<PrivacyDetail go={() => {}} />);
  await waitFor(() => expect(api.listSessions).toHaveBeenCalled());
  await screen.findByText("Say pairing codes aloud");
}

afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(api.listSessions).mockResolvedValue([] as never);
  vi.mocked(api.updateSettings).mockImplementation(async (patch) => patch as never);
});

// The Hub's Toggle keeps its own state, so assert what is drawn after the async settings load.
describe("Hub privacy settings: spoken pairing codes", () => {
  it("draws ON when the key is absent, as the server ships it", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({} as never);
    await renderPrivacy();
    expect(spokenCodesSwitch().getAttribute("aria-pressed")).toBe("true");
  });

  it("draws OFF when the household turned it off", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ spoken_pairing_codes: false } as never);
    await renderPrivacy();
    await waitFor(() => expect(spokenCodesSwitch().getAttribute("aria-pressed")).toBe("false"));
  });

  it("saves the switch as spoken_pairing_codes", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ spoken_pairing_codes: true } as never);
    await renderPrivacy();
    fireEvent.click(spokenCodesSwitch());
    await waitFor(() =>
      expect(api.updateSettings).toHaveBeenCalledWith({ spoken_pairing_codes: false }),
    );
  });
});

function bleSetupSwitch(): HTMLButtonElement {
  return screen.getByRole("button", { name: "Set up over Bluetooth" }) as HTMLButtonElement;
}

describe("Hub privacy settings: setup over Bluetooth", () => {
  it("draws ON when the key is absent, as the server ships it", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({} as never);
    await renderPrivacy();
    expect(bleSetupSwitch().getAttribute("aria-pressed")).toBe("true");
  });

  it("draws OFF when the household turned it off", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ ble_setup_enabled: false } as never);
    await renderPrivacy();
    await waitFor(() => expect(bleSetupSwitch().getAttribute("aria-pressed")).toBe("false"));
  });

  it("saves the switch as ble_setup_enabled", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ ble_setup_enabled: true } as never);
    await renderPrivacy();
    fireEvent.click(bleSetupSwitch());
    await waitFor(() =>
      expect(api.updateSettings).toHaveBeenCalledWith({ ble_setup_enabled: false }),
    );
  });
});
