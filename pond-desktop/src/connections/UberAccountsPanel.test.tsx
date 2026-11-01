import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup } from "@testing-library/react";
import { UberAccountsPanel } from "./UberAccountsPanel";
import { api } from "../api/PondApiClient";
import { ApiError } from "../api/types";
import { openExternal } from "../api/followSignIn";

vi.mock("../api/PondApiClient", () => ({
  api: {
    listProfiles: vi.fn(),
    uberAccounts: vi.fn(),
    connectUber: vi.fn(),
    disconnectUber: vi.fn(),
    getOAuthStatus: vi.fn(),
  },
}));

vi.mock("../api/followSignIn", async (importActual) => ({
  ...(await importActual<typeof import("../api/followSignIn")>()),
  openExternal: vi.fn(),
  // Settle on the first check, so the test does not wait out the polling interval.
  followSignIn: (state: string, { getStatus }: { getStatus: (s: string) => Promise<{ status: string; error?: string }> }) => ({
    stop: () => {},
    done: getStatus(state).then((s) =>
      s.status === "completed" ? { outcome: "completed" } : { outcome: "failed", error: s.error },
    ),
  }),
}));

const m = vi.mocked(api);

function household(connected: string[]) {
  const base = {
    avatar_emoji: "*",
    preferences: {},
    role: "member" as const,
    guest_until: null,
    created_at: "2024-01-01T00:00:00Z",
    updated_at: "2024-01-01T00:00:00Z",
  };
  m.listProfiles.mockResolvedValue({
    profiles: [
      { ...base, id: "p-liz", display_name: "Liz" },
      { ...base, id: "p-jerry", display_name: "Jerry" },
    ],
  });
  m.uberAccounts.mockResolvedValue({ connected });
}

describe("UberAccountsPanel", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("shows each member with their own Uber status", async () => {
    household(["p-liz"]);
    render(<UberAccountsPanel />);
    await screen.findByText("Liz");
    expect(screen.getByText("Connected")).toBeTruthy();
    expect(screen.getByText("Not connected")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Disconnect" })).toBeTruthy();
    expect(screen.getByRole("button", { name: "Connect Uber" })).toBeTruthy();
  });

  it("connects a member: opens Uber's page, follows the sign-in, then shows them connected", async () => {
    household([]);
    m.connectUber.mockResolvedValue({ auth_url: "https://auth.uber.com/oauth/v2/authorize?x", state: "n-1" });
    m.getOAuthStatus.mockResolvedValue({ status: "completed" });
    render(<UberAccountsPanel />);
    const buttons = await screen.findAllByRole("button", { name: "Connect Uber" });

    m.uberAccounts.mockResolvedValue({ connected: ["p-jerry"] });
    fireEvent.click(buttons[1]);

    await waitFor(() => expect(screen.getByText("Connected")).toBeTruthy());
    expect(m.connectUber).toHaveBeenCalledWith("p-jerry");
    expect(vi.mocked(openExternal)).toHaveBeenCalledWith("https://auth.uber.com/oauth/v2/authorize?x");
    expect(m.getOAuthStatus).toHaveBeenCalledWith("n-1");
  });

  it("says why when Uber does not connect", async () => {
    household([]);
    m.connectUber.mockResolvedValue({ auth_url: "https://auth.uber.com/x", state: "n-2" });
    m.getOAuthStatus.mockResolvedValue({ status: "failed", error: "Uber sign-in failed: invalid_grant" });
    render(<UberAccountsPanel />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Connect Uber" }))[0]);
    expect((await screen.findByRole("alert")).textContent).toContain("invalid_grant");
  });

  it("disconnects only the chosen member", async () => {
    household(["p-liz", "p-jerry"]);
    m.disconnectUber.mockResolvedValue(undefined);
    render(<UberAccountsPanel />);
    const buttons = await screen.findAllByRole("button", { name: "Disconnect" });
    m.uberAccounts.mockResolvedValue({ connected: ["p-jerry"] });
    fireEvent.click(buttons[0]);
    await waitFor(() => expect(m.disconnectUber).toHaveBeenCalledWith("p-liz"));
    await waitFor(() => expect(screen.getAllByText("Connected")).toHaveLength(1));
  });

  it("keeps the household, status unknown, when the pond will not say who is connected", async () => {
    household([]);
    m.uberAccounts.mockRejectedValue(new ApiError(403, "host_credential_required"));
    render(<UberAccountsPanel />);
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("only be changed on the pond itself");
    expect(alert.textContent).not.toContain("host_credential_required");
    expect(screen.getByText("Liz")).toBeTruthy();
    expect(screen.getAllByText("Status unknown")).toHaveLength(2);
    expect(screen.queryByText("Add a household member first.")).toBeNull();
    for (const button of screen.getAllByRole("button", { name: "Connect Uber" })) {
      expect((button as HTMLButtonElement).disabled).toBe(true);
    }
  });

  it("says what a refusal means instead of a raw code", async () => {
    household([]);
    m.uberAccounts.mockRejectedValue(new ApiError(503, "host_credential_unavailable"));
    render(<UberAccountsPanel />);
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toContain("Try again");
    expect(alert.textContent).not.toContain("host_credential_unavailable");
  });

  it("names an older pond that answers with its web page", async () => {
    household([]);
    m.uberAccounts.mockResolvedValue(undefined as unknown as { connected: string[] });
    render(<UberAccountsPanel />);
    expect((await screen.findByRole("alert")).textContent).toContain("does not know about Uber accounts yet");
    expect(screen.getAllByText("Status unknown")).toHaveLength(2);
  });

  it("reads again on Try again", async () => {
    household(["p-liz"]);
    m.uberAccounts.mockRejectedValueOnce(new ApiError(503, "could not read Uber connections", "uber_accounts_unreadable"));
    render(<UberAccountsPanel />);
    fireEvent.click(await screen.findByRole("button", { name: "Try again" }));
    await screen.findByText("Connected");
    expect(screen.queryByRole("alert")).toBeNull();
    expect(screen.queryByRole("button", { name: "Try again" })).toBeNull();
  });

  it("says so when the household itself cannot be read", async () => {
    m.listProfiles.mockRejectedValue(new ApiError(500, "database is locked"));
    m.uberAccounts.mockResolvedValue({ connected: [] });
    render(<UberAccountsPanel />);
    expect((await screen.findByRole("alert")).textContent).toContain("Could not read who is in the household");
    expect(screen.queryByText("Add a household member first.")).toBeNull();
  });

  it("explains that sign-in is turned off on this pond", async () => {
    household([]);
    m.connectUber.mockRejectedValue(
      new ApiError(503, "Uber sign-in needs Jarida's credentials service", "uber_sign_in_off"),
    );
    render(<UberAccountsPanel />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Connect Uber" }))[0]);
    expect((await screen.findByRole("alert")).textContent).toContain("Uber sign-in is turned off on this pond");
  });

  it("reports a browser that would not open, and does not wait for a sign-in", async () => {
    household([]);
    m.connectUber.mockResolvedValue({ auth_url: "https://auth.uber.com/x", state: "n-3" });
    vi.mocked(openExternal).mockRejectedValueOnce(new Error("no handler for https"));
    render(<UberAccountsPanel />);
    fireEvent.click((await screen.findAllByRole("button", { name: "Connect Uber" }))[0]);
    expect((await screen.findByRole("alert")).textContent).toContain("Could not open your browser");
    expect(m.getOAuthStatus).not.toHaveBeenCalled();
    await waitFor(() =>
      expect(
        screen.getAllByRole("button", { name: "Connect Uber" }).every((b) => !(b as HTMLButtonElement).disabled),
      ).toBe(true),
    );
  });
});
