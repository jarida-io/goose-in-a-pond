import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, cleanup } from "@testing-library/react";
import "../i18n";
import { RemoteProvider } from "./RemoteProvider";
import { api } from "../api/PondApiClient";
import { ApiError } from "../api/types";

vi.mock("../api/PondApiClient", () => ({
  api: { ngrokTunnel: vi.fn(), remoteStatus: vi.fn(), disableRemoteAccess: vi.fn() },
}));
vi.mock("./NgrokTunnel", () => ({ NgrokTunnel: () => <p>ngrok panel</p> }));
vi.mock("./RemoteAccess", () => ({ RemoteAccess: () => <p>built-in panel</p> }));

const radio = (name: RegExp) => screen.getByRole("radio", { name }) as HTMLInputElement;

describe("RemoteProvider", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("shows the running tunnel as the choice", async () => {
    vi.mocked(api.ngrokTunnel).mockResolvedValue({ config: { enabled: true, endpoint: "tcp" }, tokenSet: true, status: { state: "running", address: "0.tcp.ngrok.io:1" } });
    render(<RemoteProvider />);
    expect(await screen.findByText("ngrok panel")).toBeTruthy();
    expect(radio(/ngrok/i).checked).toBe(true);
    expect(radio(/Built-in/i).checked).toBe(false);
  });

  it("chooses neither, and says why, when this browser is signed out", async () => {
    vi.mocked(api.ngrokTunnel).mockRejectedValue(new ApiError(401, "unauthorized"));
    vi.spyOn(console, "warn").mockImplementation(() => {});
    render(<RemoteProvider />);
    expect((await screen.findByRole("status")).textContent).toMatch(/no longer signed in/);
    expect(radio(/Built-in/i).checked).toBe(false);
    expect(radio(/ngrok/i).checked).toBe(false);
    expect(screen.queryByText("built-in panel")).toBeNull();
  });

  it("says the choice could not be read on any other failure", async () => {
    vi.mocked(api.ngrokTunnel).mockRejectedValue(new Error("network down"));
    vi.spyOn(console, "warn").mockImplementation(() => {});
    render(<RemoteProvider />);
    expect((await screen.findByRole("status")).textContent).toMatch(/could not be read/);
  });
});
