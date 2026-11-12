import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, cleanup, fireEvent, waitFor } from "@testing-library/react";
import "../i18n";
import { RemoteProvider } from "./RemoteProvider";
import { api } from "../api/PondApiClient";
import { ApiError, type RemoteRouteState } from "../api/types";

vi.mock("../api/PondApiClient", () => ({
  api: { ngrokTunnel: vi.fn(), configureNgrokTunnel: vi.fn(), remoteRoute: vi.fn(), chooseRemoteRoute: vi.fn() },
}));
vi.mock("./NgrokTunnel", () => ({
  NgrokTunnel: ({ onStopped }: { onStopped?: () => void }) => <button onClick={onStopped}>ngrok panel stop</button>,
}));
vi.mock("./RemoteAccess", () => ({ RemoteAccess: () => <p>built-in panel</p> }));

const running = { config: { enabled: true, endpoint: "tcp" as const }, tokenSet: true, status: { state: "running" as const, address: "0.tcp.ngrok.io:1" } };
const route = (over: Partial<RemoteRouteState>): RemoteRouteState => ({
  route: "built_in", tokenSet: true, reserved: false, status: { state: "off" }, fallback: null, builtInRunning: true, ...over,
});

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
    vi.mocked(api.remoteRoute).mockRejectedValue(new Error("an older Pond"));
    vi.spyOn(console, "warn").mockImplementation(() => {});
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
    vi.mocked(api.remoteRoute).mockRejectedValue(new Error("an older Pond"));
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
    vi.mocked(api.remoteRoute).mockResolvedValue(route({ builtInRunning: false }));
    render(<RemoteProvider />);
    fireEvent.click(await screen.findByText("ngrok panel stop"));
    fireEvent.click(radio(/Built-in/i));
    await waitFor(() => expect(screen.getByText("built-in panel")).toBeTruthy());
  });

  it("moves remote access to the built-in network when it is chosen while the tunnel runs", async () => {
    vi.mocked(api.ngrokTunnel).mockResolvedValue(running);
    vi.mocked(api.remoteRoute).mockResolvedValue(route({ route: "ngrok", builtInRunning: false, status: running.status }));
    vi.mocked(api.chooseRemoteRoute).mockResolvedValue(route({ route: "built_in", builtInRunning: true }));
    render(<RemoteProvider />);
    await screen.findByText("ngrok panel stop");
    fireEvent.click(radio(/Built-in/i));
    expect(await screen.findByText("built-in panel")).toBeTruthy();
    expect(api.chooseRemoteRoute).toHaveBeenCalledWith("built_in");
    expect(screen.queryByRole("button", { name: "Stop the tunnel and use the built-in network" })).toBeNull();
  });

  it("moves remote access to ngrok when it is chosen while the built-in network is on", async () => {
    vi.mocked(api.ngrokTunnel).mockResolvedValue({ config: { enabled: false, endpoint: "tcp" }, tokenSet: true, status: { state: "off" } });
    vi.mocked(api.remoteRoute).mockResolvedValue(route({ builtInRunning: true }));
    vi.mocked(api.chooseRemoteRoute).mockResolvedValue(route({ route: "ngrok", builtInRunning: true, status: { state: "starting" } }));
    render(<RemoteProvider />);
    await screen.findByText("built-in panel");
    fireEvent.click(radio(/ngrok/i));
    expect(await screen.findByText("ngrok panel stop")).toBeTruthy();
    expect(api.chooseRemoteRoute).toHaveBeenCalledWith("ngrok");
  });

  it("only shows the other panel when remote access is off", async () => {
    vi.mocked(api.ngrokTunnel).mockResolvedValue({ config: { enabled: false, endpoint: "tcp" }, tokenSet: true, status: { state: "off" } });
    vi.mocked(api.remoteRoute).mockResolvedValue(route({ builtInRunning: false }));
    render(<RemoteProvider />);
    await screen.findByText("built-in panel");
    fireEvent.click(radio(/ngrok/i));
    expect(await screen.findByText("ngrok panel stop")).toBeTruthy();
    await waitFor(() => expect(api.remoteRoute).toHaveBeenCalled());
    expect(api.chooseRemoteRoute).not.toHaveBeenCalled();
  });

  it("asks for the authtoken before moving remote access to ngrok", async () => {
    vi.mocked(api.ngrokTunnel).mockResolvedValue({ config: { enabled: false, endpoint: "tcp" }, tokenSet: false, status: { state: "off" } });
    vi.mocked(api.remoteRoute).mockResolvedValue(route({ builtInRunning: true, tokenSet: false }));
    render(<RemoteProvider />);
    await screen.findByText("built-in panel");
    fireEvent.click(radio(/ngrok/i));
    expect(await screen.findByText("ngrok panel stop")).toBeTruthy();
    await waitFor(() => expect(api.remoteRoute).toHaveBeenCalled());
    expect(api.chooseRemoteRoute).not.toHaveBeenCalled();
  });

  it("says so, and keeps the route that was on, when the switch fails", async () => {
    vi.mocked(api.ngrokTunnel).mockResolvedValue({ config: { enabled: false, endpoint: "tcp" }, tokenSet: true, status: { state: "off" } });
    vi.mocked(api.remoteRoute).mockResolvedValue(route({ builtInRunning: true }));
    vi.mocked(api.chooseRemoteRoute).mockRejectedValue(new Error("down"));
    vi.spyOn(console, "warn").mockImplementation(() => {});
    render(<RemoteProvider />);
    await screen.findByText("built-in panel");
    fireEvent.click(radio(/ngrok/i));
    expect((await screen.findByRole("alert")).textContent).toMatch(/still on/);
  });
});
