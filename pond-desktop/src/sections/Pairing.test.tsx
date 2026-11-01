import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import "../i18n";
import { ApiError } from "../api/types";

const shell = vi.hoisted(() => ({ desktop: false }));
vi.mock("../shell", () => ({ isDesktopShell: () => shell.desktop }));

vi.mock("../api/PondApiClient", () => ({
  api: {
    getPairingCode: vi.fn(),
    issuePairingCode: vi.fn(),
    listProfiles: vi.fn(async () => ({ profiles: [] })),
    // Remote access sits on this page and asks for these as soon as it mounts.
    remoteStatus: vi.fn(async () => ({ state: "Stopped" })),
    remoteRecoveryRequests: vi.fn(async () => []),
    remoteDevice: vi.fn(async () => ({ provisioned: false, registered: true })),
  },
}));

import { api } from "../api/PondApiClient";
import { Pairing } from "./Pairing";

describe("Pairing when the Pond refuses the host credential", () => {
  beforeEach(() => {
    vi.mocked(api.getPairingCode).mockRejectedValue(new ApiError(403, "host_credential_required"));
  });
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("sends a browser to a new sign-in link", async () => {
    shell.desktop = false;
    render(<Pairing />);
    expect(await screen.findByText(/no longer signed in/)).toBeTruthy();
    expect(screen.getByText(/pond-server dashboard/)).toBeTruthy();
  });

  it("tells the desktop shell its credential does not match, with no link to open", async () => {
    shell.desktop = true;
    render(<Pairing />);
    expect(await screen.findByText(/does not match the Pond/)).toBeTruthy();
    expect(screen.queryByText(/pond-server dashboard/)).toBeNull();
    expect(screen.queryByText(/no longer signed in/)).toBeNull();
  });

  it("shows any other failure as it is", async () => {
    shell.desktop = true;
    vi.mocked(api.getPairingCode).mockRejectedValue(new Error("connection refused"));
    render(<Pairing />);
    expect(await screen.findByText(/connection refused/)).toBeTruthy();
    expect(screen.queryByText(/does not match the Pond/)).toBeNull();
  });
});

const MATERIAL = {
  hostname: "pond",
  lan_address: "192.168.1.7",
  tailnet_address: null,
  https_port: 8443,
  tls_spki_sha256: "pin",
};

function code(n: string, profile_id: string | null) {
  return {
    code: n,
    expires_at: new Date(Date.now() + 600_000).toISOString(),
    profile_id,
    pairing: MATERIAL,
  };
}

describe("Pairing for a household member", () => {
  beforeEach(() => {
    vi.mocked(api.listProfiles).mockResolvedValue({
      profiles: [
        { id: "emmanuel", display_name: "Emmanuel Charles Oyugi", avatar_emoji: "*" },
        { id: "jerry", display_name: "Jerry", avatar_emoji: "*" },
      ],
    });
  });
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("says a code bound to nobody makes the device nobody's", async () => {
    vi.mocked(api.getPairingCode).mockResolvedValue(code("111111", null));
    render(<Pairing />);
    expect(await screen.findByText(/belongs to nobody/)).toBeTruthy();
  });

  it("issues a fresh code for the member chosen, and says whose it makes the device", async () => {
    vi.mocked(api.getPairingCode).mockResolvedValue(code("111111", null));
    vi.mocked(api.issuePairingCode).mockResolvedValue(code("222222", "emmanuel"));
    render(<Pairing />);
    const picker = (await screen.findByLabelText("Who will use this device?")) as HTMLSelectElement;
    await waitFor(() => expect(picker.disabled).toBe(false));

    fireEvent.change(picker, { target: { value: "emmanuel" } });

    await waitFor(() =>
      expect(api.issuePairingCode).toHaveBeenCalledWith({ profile_id: "emmanuel" }),
    );
    expect(await screen.findByText(/becomes Emmanuel Charles Oyugi's/)).toBeTruthy();
    expect(screen.getByText("222222")).toBeTruthy();
  });

  it("asks for nobody explicitly, so a household of one can still pair a guest", async () => {
    vi.mocked(api.getPairingCode).mockResolvedValue(code("111111", "emmanuel"));
    vi.mocked(api.issuePairingCode).mockResolvedValue(code("333333", null));
    render(<Pairing />);
    const picker = (await screen.findByLabelText("Who will use this device?")) as HTMLSelectElement;
    await waitFor(() => expect(picker.value).toBe("emmanuel"));

    fireEvent.change(picker, { target: { value: "" } });

    await waitFor(() => expect(api.issuePairingCode).toHaveBeenCalledWith({ unattributed: true }));
  });

  it("says so when the members cannot be read", async () => {
    vi.mocked(api.getPairingCode).mockResolvedValue(code("111111", null));
    vi.mocked(api.listProfiles).mockRejectedValue(new Error("offline"));
    vi.spyOn(console, "warn").mockImplementation(() => {});
    render(<Pairing />);
    expect(await screen.findByText(/household members could not be read/)).toBeTruthy();
  });
});
