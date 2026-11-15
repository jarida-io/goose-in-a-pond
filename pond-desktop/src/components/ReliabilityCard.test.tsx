import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { api } from "../api/PondApiClient";
import { ApiError } from "../api/types";
import { ReliabilityCard } from "./ReliabilityCard";

vi.mock("../api/PondApiClient", () => ({ api: { getReliability: vi.fn() } }));

const report = (days: number) => ({
  days,
  notices: { queued: 20, woken: 12, fetched: 18, fetched_within_a_minute: 9, fetched_after_an_hour: 0, shown: 16, opened: 8, spoken: 1 },
  turns: { asked: 5, answered: 4 },
  phones: { paired: 2, test: 0, unseen: 0, with_push_token: 2 },
  stops: 0,
  android_push_key: true,
  apple_push_key: false,
});

beforeEach(() => vi.clearAllMocks());
afterEach(cleanup);

describe("ReliabilityCard", () => {
  it("reads 7 days, then 30 when switched", async () => {
    vi.mocked(api.getReliability).mockImplementation(async (d) => report(d));
    render(<ReliabilityCard />);
    expect(await screen.findByText("9 of 20 (45%)")).toBeTruthy();
    expect(screen.getByText("missing")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "30 days" }));
    await waitFor(() => expect(api.getReliability).toHaveBeenCalledWith(30));
  });

  it("says so when My Pond is locked", async () => {
    vi.mocked(api.getReliability).mockRejectedValue(new ApiError(403, "admin_locked"));
    render(<ReliabilityCard />);
    expect(await screen.findByText(/whose My Pond is unlocked/)).toBeTruthy();
  });

  it("says so when the pond cannot read it", async () => {
    vi.mocked(api.getReliability).mockRejectedValue(new ApiError(500, "x"));
    render(<ReliabilityCard />);
    expect(await screen.findByText(/could not read its record/)).toBeTruthy();
  });
});
