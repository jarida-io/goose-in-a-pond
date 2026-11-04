import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, cleanup, act } from "@testing-library/react";

vi.mock("../../../api/PondApiClient", () => ({
  api: {
    getSettings: vi.fn(),
    listDevices: vi.fn(),
    getSystemInfo: vi.fn(),
    updateSettings: vi.fn(),
    setToken: vi.fn(),
    resetOnboarding: vi.fn(),
  },
}));

vi.mock("../../../state/useAppContext", () => ({
  useAppDispatch: () => vi.fn(),
}));

import { AccountDetail } from "./Account";
import { api } from "../../../api/PondApiClient";

async function renderScreen() {
  render(<AccountDetail go={() => {}} />);
  await act(async () => {});
}

afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(api.getSettings).mockResolvedValue({} as never);
  vi.mocked(api.listDevices).mockResolvedValue([]);
});

describe("Account settings, About", () => {
  it("shows the version the Pond reports, and claims no update check", async () => {
    vi.mocked(api.getSystemInfo).mockResolvedValue({ version: "9.9.9" } as never);
    await renderScreen();
    expect(screen.getByText("Version 9.9.9")).toBeTruthy();
    expect(screen.queryByText("Up to date")).toBeNull();
  });

  it("shows no version when the Pond cannot be asked", async () => {
    vi.mocked(api.getSystemInfo).mockRejectedValue(new Error("offline"));
    await renderScreen();
    expect(screen.getByText("Goose In A Pond")).toBeTruthy();
    expect(screen.queryByText(/^Version /)).toBeNull();
  });
});
