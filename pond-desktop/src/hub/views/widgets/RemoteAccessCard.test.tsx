import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, cleanup } from "@testing-library/react";
import { RemoteAccessCard } from "./RemoteAccessCard";
import { readReach } from "../../state/remoteReach";
import { api } from "../../../api/PondApiClient";
import type { NgrokTunnelState } from "../../../api/types";

vi.mock("../../../api/PondApiClient", () => ({
  api: { ngrokTunnel: vi.fn(), remoteStatus: vi.fn(), remoteRecoveryRequests: vi.fn() },
}));

const ngrok = (status: NgrokTunnelState["status"], enabled = true): NgrokTunnelState => ({
  config: { enabled, endpoint: "tcp" },
  tokenSet: true,
  status,
});

describe("readReach", () => {
  it("takes a running tunnel over the built-in network, with its address", () => {
    expect(readReach(ngrok({ state: "running", address: "0.tcp.in.ngrok.io:25293" }), { state: "Stopped" })).toEqual({
      reach: "on",
      route: "ngrok",
      address: "0.tcp.in.ngrok.io:25293",
    });
  });

  it("says the tunnel is down when it failed, not that the house is home only", () => {
    expect(readReach(ngrok({ state: "failed", reason: "x" }), { state: "Stopped" }).reach).toBe("problem");
  });

  it("falls back to the built-in network when no tunnel is on", () => {
    expect(readReach(ngrok({ state: "off" }, false), { state: "Running" })).toMatchObject({ reach: "on", route: "built-in" });
    expect(readReach(ngrok({ state: "off" }, false), { state: "Stopped" }).reach).toBe("off");
    expect(readReach(null, { state: "connecting" })).toMatchObject({ reach: "connecting", route: "built-in" });
  });

  it("is unknown only when neither way could be read", () => {
    expect(readReach(null, null).reach).toBe("unknown");
    expect(readReach(ngrok({ state: "off" }, false), null).reach).toBe("off");
  });
});

describe("RemoteAccessCard", () => {
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("names the tunnel and its address while phones can reach the Pond", async () => {
    vi.mocked(api.ngrokTunnel).mockResolvedValue(ngrok({ state: "running", address: "0.tcp.in.ngrok.io:25293" }));
    vi.mocked(api.remoteStatus).mockResolvedValue({ state: "Stopped" });
    vi.mocked(api.remoteRecoveryRequests).mockResolvedValue([]);
    render(<RemoteAccessCard onNavigate={() => {}} />);
    expect((await screen.findByRole("status")).textContent).toBe("Reachable away from home");
    expect(screen.getByText("Through your ngrok account")).toBeTruthy();
    expect(screen.getByText("0.tcp.in.ngrok.io:25293")).toBeTruthy();
  });

  it("says home only when nothing lets phones in", async () => {
    vi.mocked(api.ngrokTunnel).mockResolvedValue(ngrok({ state: "off" }, false));
    vi.mocked(api.remoteStatus).mockResolvedValue({ state: "Stopped" });
    vi.mocked(api.remoteRecoveryRequests).mockResolvedValue([]);
    const { container } = render(<RemoteAccessCard onNavigate={() => {}} />);
    expect((await screen.findByRole("status")).textContent).toBe("Home only");
    expect(container.querySelector(".rac")?.getAttribute("data-state")).toBe("off");
  });
});
