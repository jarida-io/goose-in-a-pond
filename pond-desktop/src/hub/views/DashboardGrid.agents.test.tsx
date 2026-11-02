// Home shows a suggested agent, and turning it on hands the new copy to the Recipes screen. The
// grid is shared by the hub's Home and the classic Dashboard, so this covers both.

import { render, screen, fireEvent, waitFor, within, cleanup } from "@testing-library/react";
import { describe, expect, it, beforeEach, afterEach, vi } from "vitest";
import { api } from "../../api/PondApiClient";
import type { AgentRecipe, AgentSuggestion } from "../../api/types";
import { clearOpenAgent, peekOpenAgent } from "../../agents/openAgent";
import { __resetLayoutCache } from "../state/dashboardLayout";
import { DashboardGrid } from "./DashboardGrid";

const MORNING: AgentSuggestion = {
  template: "morning-brief",
  title: "Morning brief",
  description: "Tells the house the weather each weekday.",
  why: "This pond has a weather source.",
  needs: ["weather"],
  activations_summary: "Every weekday at 7:00",
};

const WELCOME: AgentSuggestion = {
  template: "welcome-back",
  title: "Welcome back",
  description: "Greets a member when they arrive.",
  why: "This pond sees who arrives.",
  needs: ["presence"],
  activations_summary: "When someone arrives home",
};

const COPY: AgentRecipe = {
  id: "r5",
  name: "morning-brief",
  description: "Tells the house the weather each weekday.",
  yaml: "title: Morning brief\ngiap_role:\n  tool_groups: []\n",
  active: true,
  owner_profile_id: null,
};

beforeEach(() => {
  cleanup();
  vi.restoreAllMocks();
  localStorage.clear();
  __resetLayoutCache();
  clearOpenAgent();
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  clearOpenAgent();
});

describe("Home's suggested agent", () => {
  it("shows one card with what it does, why it fits and when it would run, and says nothing runs yet", async () => {
    vi.spyOn(api, "listAgentSuggestions").mockResolvedValue([MORNING, WELCOME]);
    render(<DashboardGrid sessionId="s1" onNavigate={() => {}} onTalk={() => {}} />);
    const card = await screen.findByRole("article", { name: "Morning brief" });
    expect(within(card).getByText("This pond has a weather source.")).toBeTruthy();
    expect(within(card).getByText("Every weekday at 7:00")).toBeTruthy();
    expect(within(card).getByText(/Nothing runs until you turn it on/)).toBeTruthy();
    expect(screen.queryByRole("article", { name: "Welcome back" })).toBeNull();
  });

  it("shows nothing when the pond has no template it can run", async () => {
    const list = vi.spyOn(api, "listAgentSuggestions").mockResolvedValue([]);
    render(<DashboardGrid sessionId="s1" onNavigate={() => {}} onTalk={() => {}} />);
    await waitFor(() => expect(list).toHaveBeenCalled());
    expect(screen.queryByText("A helper you could turn on")).toBeNull();
  });

  it("points at the rest on Recipes", async () => {
    vi.spyOn(api, "listAgentSuggestions").mockResolvedValue([MORNING, WELCOME]);
    const onNavigate = vi.fn();
    render(<DashboardGrid sessionId="s1" onNavigate={onNavigate} onTalk={() => {}} />);
    fireEvent.click(await screen.findByRole("button", { name: "1 more in Recipes" }));
    expect(onNavigate).toHaveBeenCalledWith("recipes");
  });

  it("turns on with one tap, then goes to Recipes with the new copy ready to open in the builder", async () => {
    vi.spyOn(api, "listAgentSuggestions").mockResolvedValue([MORNING]);
    const enable = vi.spyOn(api, "enableAgentSuggestion").mockResolvedValue({ recipe: COPY });
    const onNavigate = vi.fn();
    render(<DashboardGrid sessionId="s1" onNavigate={onNavigate} onTalk={() => {}} />);
    fireEvent.click(await screen.findByRole("button", { name: "Turn on Morning brief" }));
    await waitFor(() => expect(onNavigate).toHaveBeenCalledWith("recipes"));
    expect(enable).toHaveBeenCalledWith("morning-brief");
    expect(peekOpenAgent()).toEqual(COPY);
  });

  it("does not leave the screen when turning on fails", async () => {
    vi.spyOn(api, "listAgentSuggestions").mockResolvedValue([MORNING]);
    vi.spyOn(api, "enableAgentSuggestion").mockRejectedValue(new Error("offline"));
    const onNavigate = vi.fn();
    render(<DashboardGrid sessionId="s1" onNavigate={onNavigate} onTalk={() => {}} />);
    fireEvent.click(await screen.findByRole("button", { name: "Turn on Morning brief" }));
    await screen.findByText(/Could not turn it on/);
    expect(onNavigate).not.toHaveBeenCalled();
    expect(peekOpenAgent()).toBeNull();
  });

  it("hides a dismissed card and tells the pond", async () => {
    vi.spyOn(api, "listAgentSuggestions").mockResolvedValue([MORNING]);
    const dismiss = vi.spyOn(api, "dismissAgentSuggestion").mockResolvedValue(undefined);
    render(<DashboardGrid sessionId="s1" onNavigate={() => {}} onTalk={() => {}} />);
    fireEvent.click(await screen.findByRole("button", { name: "Do not suggest Morning brief again" }));
    expect(screen.queryByRole("article", { name: "Morning brief" })).toBeNull();
    await waitFor(() => expect(dismiss).toHaveBeenCalledWith("morning-brief"));
  });
});
