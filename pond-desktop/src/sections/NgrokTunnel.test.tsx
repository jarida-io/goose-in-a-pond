import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, waitFor, cleanup } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import "../i18n";
import { NgrokTunnel } from "./NgrokTunnel";
import { api } from "../api/PondApiClient";
import { ApiError, type NgrokTunnelState } from "../api/types";

vi.mock("../api/PondApiClient", () => ({
  api: {
    ngrokTunnel: vi.fn(),
    configureNgrokTunnel: vi.fn(),
    setNgrokAuthtoken: vi.fn(),
    forgetNgrokAuthtoken: vi.fn(),
  },
}));

const state = (over: Partial<NgrokTunnelState> = {}): NgrokTunnelState => ({
  config: { enabled: false, endpoint: "tcp" },
  tokenSet: false,
  status: { state: "off" },
  ...over,
});

describe("NgrokTunnel", () => {
  beforeEach(() => {
    vi.mocked(api.ngrokTunnel).mockResolvedValue(state());
  });
  afterEach(() => {
    cleanup();
    vi.clearAllMocks();
  });

  it("cannot start until an authtoken is stored", async () => {
    render(<NgrokTunnel />);
    const reach = await screen.findByRole("switch", { name: "Reach this Pond from outside your home" });
    expect((reach as HTMLInputElement).disabled || reach.getAttribute("aria-disabled") === "true").toBe(true);
    expect(screen.getByText("Save your ngrok authtoken above to turn this on.")).toBeTruthy();
  });

  it("checks the authtoken through the pond and clears the field once stored", async () => {
    vi.mocked(api.setNgrokAuthtoken).mockResolvedValue(state({ tokenSet: true }));
    render(<NgrokTunnel />);
    const field = await screen.findByLabelText("ngrok authtoken");
    await userEvent.type(field, "test-authtoken-not-real");
    await userEvent.click(screen.getByRole("button", { name: "Check and save authtoken" }));
    await waitFor(() => expect(api.setNgrokAuthtoken).toHaveBeenCalledWith("test-authtoken-not-real"));
    await waitFor(() => expect((field as HTMLInputElement).value).toBe(""));
    expect(screen.getByText("An authtoken is stored.")).toBeTruthy();
  });

  it("says what the pond refused", async () => {
    vi.mocked(api.setNgrokAuthtoken).mockRejectedValue(
      new ApiError(400, "refused", "authtoken_refused", { error: "ngrok refused the session: ERR_NGROK_105", code: "authtoken_refused" }),
    );
    render(<NgrokTunnel />);
    await userEvent.type(await screen.findByLabelText("ngrok authtoken"), "wrong");
    await userEvent.click(screen.getByRole("button", { name: "Check and save authtoken" }));
    expect((await screen.findByRole("alert")).textContent).toContain("ERR_NGROK_105");
  });

  it("starts the tunnel, tells its caller, and shows the address phones use", async () => {
    vi.mocked(api.ngrokTunnel).mockResolvedValue(state({ tokenSet: true }));
    vi.mocked(api.configureNgrokTunnel).mockResolvedValue(
      state({ tokenSet: true, config: { enabled: true, endpoint: "tcp" }, status: { state: "running", address: "4.tcp.eu.ngrok.io:12345" } }),
    );
    const started = vi.fn();
    render(<NgrokTunnel onStarted={started} />);
    await userEvent.click(await screen.findByRole("switch", { name: "Reach this Pond from outside your home" }));
    await waitFor(() => expect(api.configureNgrokTunnel).toHaveBeenCalledWith({ enabled: true, endpoint: "tcp" }));
    expect(await screen.findByText("Phones can reach this Pond at 4.tcp.eu.ngrok.io:12345.")).toBeTruthy();
    expect(started).toHaveBeenCalled();
    expect((screen.getByRole("switch", { name: "Reach this Pond from outside your home" }) as HTMLInputElement).checked).toBe(true);
  });

  it("asks for the reserved domain when the endpoint is TLS", async () => {
    render(<NgrokTunnel />);
    await userEvent.click(await screen.findByLabelText("TLS on my reserved domain (paid plans)"));
    expect(screen.getByLabelText("Reserved domain")).toBeTruthy();
    expect(screen.queryByLabelText("Reserved TCP address (optional)")).toBeNull();
  });
});
