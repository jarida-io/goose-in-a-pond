// Home says, quietly, that the pond is getting agents ready. The grid is shared by the hub's Home
// and the classic Dashboard, so this covers both.

import { render, screen, waitFor, cleanup } from "@testing-library/react";
import { describe, expect, it, beforeEach, afterEach, vi } from "vitest";
import { api } from "../../api/PondApiClient";
import { ApiError } from "../../api/types";
import { agentRow, readyReport, reportOf } from "../../agents/prefillFixtures";
import { __resetLayoutCache } from "../state/dashboardLayout";
import { DashboardGrid } from "./DashboardGrid";

function renderGrid() {
  return render(<DashboardGrid sessionId="s1" onNavigate={() => {}} onTalk={() => {}} />);
}

beforeEach(() => {
  cleanup();
  vi.restoreAllMocks();
  localStorage.clear();
  __resetLayoutCache();
  vi.spyOn(api, "listAgentSuggestions").mockResolvedValue([]);
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
});

describe("Home while the pond builds agents", () => {
  it("says how far along it is, and which agent it is on", async () => {
    vi.spyOn(api, "getAgentPrefills").mockResolvedValue(
      reportOf([agentRow("study-planner", { status: "building" })], { active: true, current: "study-planner", done: 2, total: 5 }),
    );
    renderGrid();
    expect(await screen.findByText("Getting agents ready (2 of 5): study-planner")).toBeTruthy();
  });

  it("says nothing when the pond is not building", async () => {
    const asked = vi.spyOn(api, "getAgentPrefills").mockResolvedValue(readyReport());
    renderGrid();
    await waitFor(() => expect(asked).toHaveBeenCalled());
    expect(screen.queryByText(/Getting agents ready/)).toBeNull();
  });

  it("says nothing on a pond that has no such route", async () => {
    const asked = vi.spyOn(api, "getAgentPrefills").mockRejectedValue(new ApiError(404, "Not Found"));
    renderGrid();
    await waitFor(() => expect(asked).toHaveBeenCalled());
    expect(screen.queryByText(/Getting agents ready/)).toBeNull();
  });
});
