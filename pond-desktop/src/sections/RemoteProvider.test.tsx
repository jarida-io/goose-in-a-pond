import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, cleanup, fireEvent, waitFor } from "@testing-library/react";
import "../i18n";
import { RemoteProvider } from "./RemoteProvider";
import { api } from "../api/PondApiClient";
import { ApiError } from "../api/types";

vi.mock("../api/PondApiClient", () => ({
  api: { ngrokTunnel: vi.fn(), remoteStatus: vi.fn(), disableRemoteAccess: vi.fn(), configureNgrokTunnel: vi.fn() },
}));
vi.mock("./NgrokTunnel", () => ({
  NgrokTunnel: ({ onStopped }: { onStopped?: () => void }) => <button onClick={onStopped}>ngrok panel stop</button>,
}));
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
    expect(await screen.findByText("ngrok panel stop")).toBeTruthy();
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

  it("stops the tunnel and switches when the built-in network is chosen while it runs", async () => {
    vi.mocked(api.ngrokTunnel).mockResolvedValue({ config: { enabled: true, endpoint: "tcp" }, tokenSet: true, status: { state: "running", address: "0.tcp.ngrok.io:1" } });
    vi.mocked(api.configureNgrokTunnel).mockResolvedValue({ config: { enabled: false, endpoint: "tcp" }, tokenSet: true, status: { state: "off" } });
    render(<RemoteProvider />);
    await screen.findByText("ngrok panel stop");
    fireEvent.click(radio(/Built-in/i));

    fireEvent.click(await screen.findByRole("button", { name: "Stop the tunnel and use the built-in network" }));
    expect(await screen.findByText("built-in panel")).toBeTruthy();
    expect(api.configureNgrokTunnel).toHaveBeenCalledWith({ enabled: false, endpoint: "tcp" });
  });

  it("keeps the tunnel and says so when it would not stop", async () => {
    vi.mocked(api.ngrokTunnel).mockResolvedValue({ config: { enabled: true, endpoint: "tcp" }, tokenSet: true, status: { state: "running", address: "0.tcp.ngrok.io:1" } });
    vi.mocked(api.configureNgrokTunnel).mockRejectedValue(new Error("down"));
    vi.spyOn(console, "warn").mockImplementation(() => {});
    render(<RemoteProvider />);
    await screen.findByText("ngrok panel stop");
    fireEvent.click(radio(/Built-in/i));
    fireEvent.click(await screen.findByRole("button", { name: "Stop the tunnel and use the built-in network" }));
    expect((await screen.findByRole("alert")).textContent).toMatch(/did not stop/);
    expect(screen.queryByText("built-in panel")).toBeNull();
  });

  it("goes straight to the built-in network once the tunnel was stopped in its own panel", async () => {
    vi.mocked(api.ngrokTunnel).mockResolvedValue({ config: { enabled: true, endpoint: "tcp" }, tokenSet: true, status: { state: "running", address: "0.tcp.ngrok.io:1" } });
    render(<RemoteProvider />);
    fireEvent.click(await screen.findByText("ngrok panel stop"));
    fireEvent.click(radio(/Built-in/i));
    await waitFor(() => expect(screen.getByText("built-in panel")).toBeTruthy());
  });
});
