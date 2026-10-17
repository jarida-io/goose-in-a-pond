import { describe, it, expect, vi, afterEach } from "vitest";
import { render, screen, fireEvent, cleanup } from "@testing-library/react";
import type { ReactNode } from "react";
import { StateCtx } from "../state/useAppContext";
import type { AppState } from "../state/reducer";
import { api } from "../api/PondApiClient";
import { Context } from "../sections/Context";
import { ConnectionsDetail } from "../hub/views/settings/Connections";

vi.mock("../api/PondApiClient", () => ({
  api: {
    getWebSearch: vi.fn(),
    listContextSources: vi.fn(),
    listProfiles: vi.fn(),
    uberAccounts: vi.fn(),
    listMemories: vi.fn(),
    getContextIndexHealth: vi.fn(),
  },
}));

const m = vi.mocked(api);

function withState(ui: ReactNode) {
  return <StateCtx.Provider value={{ sessionId: "s1" } as AppState}>{ui}</StateCtx.Provider>;
}

function pondAnswers() {
  m.getWebSearch.mockResolvedValue({ provider: "brave", configured: false, status: "not_configured" });
  m.listContextSources.mockResolvedValue({ sources: [] });
  m.listProfiles.mockResolvedValue({ profiles: [] });
  m.uberAccounts.mockResolvedValue({ connected: [] });
  m.listMemories.mockResolvedValue([]);
  m.getContextIndexHealth.mockResolvedValue(null as never);
}

afterEach(() => {
  cleanup();
  vi.clearAllMocks();
});

describe("web search is on both surfaces", () => {
  it("classic: under Context, Sources, beside the accounts", async () => {
    pondAnswers();
    render(withState(<Context />));
    fireEvent.click(screen.getByRole("button", { name: "Sources" }));

    expect(await screen.findByRole("heading", { name: "Web search" })).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Your accounts" })).toBeTruthy();
    expect(await screen.findByText("Off")).toBeTruthy();
    expect(screen.getByLabelText("Brave Search key")).toBeTruthy();
  });

  it("hub: on the Accounts screen, whose subtitle says so", async () => {
    pondAnswers();
    render(withState(<ConnectionsDetail go={() => {}} />));

    expect(await screen.findByRole("heading", { name: "Web search" })).toBeTruthy();
    expect(screen.getByRole("heading", { name: "Your accounts" })).toBeTruthy();
    expect(
      screen.getByText("Calendar and mail the pond can read, each person's Uber, and web search"),
    ).toBeTruthy();
    expect(await screen.findByText("Off")).toBeTruthy();
    expect(screen.getByLabelText("Brave Search key")).toBeTruthy();
  });
});
