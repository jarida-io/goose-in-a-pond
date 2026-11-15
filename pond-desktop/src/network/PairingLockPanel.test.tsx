import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { api } from "../api/PondApiClient";
import { ApiError } from "../api/types";
import type { PairingLock } from "../api/types";
import { PairingLockPanel } from "./PairingLockPanel";

vi.mock("../api/PondApiClient", () => ({
  api: {
    getPairingLock: vi.fn(), setPairingLock: vi.fn(), setRecoveryPin: vi.fn(), clearRecoveryPin: vi.fn(),
    listPairingRequests: vi.fn(), approvePairingRequest: vi.fn(), declinePairingRequest: vi.fn(), listProfiles: vi.fn(),
  },
}));

const lock = (over: Partial<PairingLock> = {}): PairingLock => ({
  locked: true, enforced: true, owner_profile_id: "p1", has_recovery_pin: false,
  owner_has_a_phone: true, min_pin_digits: 6, can_change: false, ...over,
});

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(api.listProfiles).mockResolvedValue({ profiles: [{ id: "p1", display_name: "Amina" }] } as never);
  vi.mocked(api.listPairingRequests).mockResolvedValue({ requests: [] });
});
afterEach(cleanup);

describe("PairingLockPanel", () => {
  it("shows status only, with the owner's name, when this device cannot change the lock", async () => {
    vi.mocked(api.getPairingLock).mockResolvedValue(lock());
    render(<PairingLockPanel variant="catalogue" />);
    expect(await screen.findByText("Main member: Amina")).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Unlock pairing" })).toBeNull();
    expect(api.listPairingRequests).not.toHaveBeenCalled();
  });

  it("explains a lock that is not holding", async () => {
    vi.mocked(api.getPairingLock).mockResolvedValue(lock({ enforced: false, owner_has_a_phone: false }));
    render(<PairingLockPanel variant="hub" />);
    expect(await screen.findByText(/not holding yet/)).toBeTruthy();
  });

  it("lets a member accept a waiting phone", async () => {
    vi.mocked(api.getPairingLock).mockResolvedValue(lock({ can_change: true }));
    vi.mocked(api.listPairingRequests).mockResolvedValue({ requests: [{ id: "r1", device_name: "Sam's phone", client_type: "gotg", created_at: "", expires_at: "" }] });
    vi.mocked(api.approvePairingRequest).mockResolvedValue(undefined);
    render(<PairingLockPanel variant="catalogue" />);
    fireEvent.click(await screen.findByRole("button", { name: /Accept/ }));
    await waitFor(() => expect(api.approvePairingRequest).toHaveBeenCalledWith("r1"));
  });

  it("goes read-only when the requests answer 403", async () => {
    vi.mocked(api.getPairingLock).mockResolvedValue(lock({ can_change: true }));
    vi.mocked(api.listPairingRequests).mockRejectedValue(new ApiError(403, "pairing_main_member_only"));
    render(<PairingLockPanel variant="catalogue" />);
    await waitFor(() => expect(screen.queryByRole("button", { name: "Unlock pairing" })).toBeNull());
    expect(screen.getByText("Pairing lock")).toBeTruthy();
  });

  it("refuses a short PIN before asking the pond", async () => {
    vi.mocked(api.getPairingLock).mockResolvedValue(lock({ can_change: true }));
    render(<PairingLockPanel variant="catalogue" />);
    fireEvent.change(await screen.findByLabelText("Set a recovery PIN"), { target: { value: "123" } });
    fireEvent.click(screen.getByRole("button", { name: /Save PIN/ }));
    expect(await screen.findByText(/at least 6/)).toBeTruthy();
    expect(api.setRecoveryPin).not.toHaveBeenCalled();
  });
});
