import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, cleanup, act, fireEvent } from "@testing-library/react";

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

vi.mock("../../../api/followSignIn", () => ({
  openExternal: vi.fn(),
}));

vi.mock("../../../state/useAppContext", () => ({
  useAppDispatch: () => vi.fn(),
}));

import { AccountDetail } from "./Account";
import { api } from "../../../api/PondApiClient";
import { openExternal } from "../../../api/followSignIn";

afterEach(cleanup);
beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(api.getSettings).mockResolvedValue({} as never);
  vi.mocked(api.listDevices).mockResolvedValue([]);
  vi.mocked(api.getSystemInfo).mockResolvedValue({ version: "1.0.0" } as never);
});

describe("Account settings, Help & feedback", () => {
  it("opens the project's issues page in the browser", async () => {
    render(<AccountDetail go={() => {}} />);
    await act(async () => {});
    fireEvent.click(screen.getByRole("button", { name: /Help & feedback/ }));
    expect(vi.mocked(openExternal)).toHaveBeenCalledWith("https://github.com/jarida-io/goose-in-a-pond/issues");
  });
});
