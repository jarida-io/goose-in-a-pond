// The hub reaches the agent builder the way the classic shell does: through the Recipes entry in
// the drawer, and from Home. The drawer is real; Home's grid is a stub, as in Hub.test.tsx.

import { render, screen, fireEvent, waitFor, within, cleanup } from "@testing-library/react";
import { describe, expect, it, beforeEach, afterEach, vi } from "vitest";
import { api } from "../api/PondApiClient";
import { ApiError, type AgentRecipe, type AgentSuggestion, type RecipeValidation } from "../api/types";
import { ConfirmProvider } from "../components/shared";
import { clearOpenAgent, peekOpenAgent, requestOpenAgent } from "../agents/openAgent";
import { readyReport } from "../agents/prefillFixtures";

const dispatch = vi.fn();

vi.mock("../state/useAppContext", () => ({
  useAppState: () => ({ sessionId: "s1", unreadRunCount: 0, serverOnline: true, scheduleRuns: [] }),
  useAppDispatch: () => dispatch,
}));

vi.mock("./views/DashboardGrid", () => ({
  DashboardGrid: ({ onNavigate }: { onNavigate: (s: string) => void }) => (
    <div data-hook="grid-stub">
      <button type="button" onClick={() => onNavigate("recipes")}>
        Open Recipes
      </button>
    </div>
  ),
}));

vi.mock("./overlays/HubOverlay", () => ({ HubOverlay: () => null }));

import { Hub } from "./Hub";
import { AgentsView } from "./views/AgentsHub";

const OK: RecipeValidation = {
  ok: true,
  role: { name: "draft", tool_groups: [], activations: [], hooks: { before: [], after: [] }, preamble_chars: 800 },
  errors: [],
};

const GREETER: AgentRecipe = {
  id: "r1",
  name: "greeter",
  title: "Greeter",
  description: "Greets whoever arrives",
  yaml: "title: Greeter\ngiap_role:\n  tool_groups: []\n  instructions: Greet whoever arrives.\n  activations: [delegated]\n",
  active: true,
  owner_profile_id: "amina",
};

const DAILY: AgentRecipe = {
  id: "r2",
  name: "daily_summary",
  title: "Daily summary",
  description: "Summarize the day",
  yaml: 'prompt: "Summarize the day"\n',
  active: true,
};

const MORNING: AgentSuggestion = {
  template: "morning-brief",
  title: "Morning brief",
  description: "Tells the house the weather each weekday.",
  why: "This pond has a weather source.",
  needs: ["weather"],
  activations_summary: "Every weekday at 7:00",
};

const MORNING_COPY: AgentRecipe = {
  id: "r5",
  name: "morning-brief",
  description: "Tells the house the weather each weekday.",
  yaml: "title: Morning brief\ngiap_role:\n  tool_groups: []\n  instructions: Brief the house.\n",
  active: true,
  owner_profile_id: null,
};

function stubPond(recipes: AgentRecipe[], suggestions: AgentSuggestion[] = []) {
  vi.spyOn(api, "listRecipes").mockResolvedValue(recipes);
  vi.spyOn(api, "listExtensions").mockResolvedValue({ extensions: [] });
  vi.spyOn(api, "listProfiles").mockResolvedValue({ profiles: [{ id: "amina", display_name: "Amina", avatar_emoji: "" }] });
  vi.spyOn(api, "listDevices").mockResolvedValue([]);
  vi.spyOn(api, "getSettings").mockResolvedValue({
    user_name: "Ada",
    assistant_name: "Goose",
    prompt_style: "balanced",
    ext_orchestrator_enabled: true,
  } as never);
  vi.spyOn(api, "validateRecipe").mockResolvedValue(OK);
  vi.spyOn(api, "listAgentSuggestions").mockResolvedValue(suggestions);
  vi.spyOn(api, "getAgentPrefills").mockResolvedValue(readyReport("greeter"));
}

beforeEach(() => {
  cleanup();
  vi.restoreAllMocks();
  dispatch.mockClear();
  clearOpenAgent();
  localStorage.setItem("goosehub_route", "home");
});

afterEach(() => {
  cleanup();
  vi.restoreAllMocks();
  clearOpenAgent();
});

describe("the hub's Recipes screen", () => {
  it("lists the agents, and points at the plain recipes on Routines instead of listing them twice", async () => {
    stubPond([GREETER, DAILY]);
    const go = vi.fn();
    render(
      <ConfirmProvider>
        <AgentsView go={go} />
      </ConfirmProvider>,
    );
    expect(screen.getByRole("heading", { level: 1, name: "Recipes" })).toBeTruthy();
    const agents = await screen.findByRole("region", { name: /^Agents/ });
    expect(within(agents).getByRole("article", { name: "Greeter" })).toBeTruthy();
    expect(screen.queryByText("Daily summary")).toBeNull();
    expect(screen.getByText("One other recipe is on the Routines screen, one tap each.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Open Routines" }));
    expect(go).toHaveBeenCalledWith("routines");
  });

  it("says nothing about other recipes when there are none", async () => {
    stubPond([GREETER]);
    render(
      <ConfirmProvider>
        <AgentsView go={() => {}} />
      </ConfirmProvider>,
    );
    await screen.findByRole("article", { name: "Greeter" });
    expect(screen.queryByRole("button", { name: "Open Routines" })).toBeNull();
  });

  it("invites a first agent when there are none", async () => {
    stubPond([]);
    render(
      <ConfirmProvider>
        <AgentsView go={() => {}} />
      </ConfirmProvider>,
    );
    expect(await screen.findByText(/No agents yet/)).toBeTruthy();
  });

  it("opens the builder from New agent, with the touch sizes, and from an agent's card", async () => {
    stubPond([GREETER]);
    const { container } = render(
      <ConfirmProvider>
        <AgentsView go={() => {}} />
      </ConfirmProvider>,
    );
    await screen.findByRole("article", { name: "Greeter" });
    fireEvent.click(screen.getByRole("button", { name: /New agent/ }));
    expect(await screen.findByRole("heading", { name: "New agent" })).toBeTruthy();
    expect(container.querySelector(".ag--hub")).toBeTruthy();
    expect(screen.getByRole("tab", { name: "YAML" })).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Agents" }));
    fireEvent.click(await screen.findByRole("button", { name: "Edit Greeter" }));
    expect(await screen.findByRole("heading", { name: "Greeter" })).toBeTruthy();
    expect((screen.getByLabelText("Instructions") as HTMLTextAreaElement).value).toBe("Greet whoever arrives.");
  });

  it("offers a suggested agent, and turning it on opens the builder on the new copy", async () => {
    stubPond([GREETER], [MORNING]);
    vi.spyOn(api, "enableAgentSuggestion").mockResolvedValue({ recipe: MORNING_COPY });
    render(
      <ConfirmProvider>
        <AgentsView go={() => {}} />
      </ConfirmProvider>,
    );
    await screen.findByRole("article", { name: "Morning brief" });
    fireEvent.click(screen.getByRole("button", { name: "Turn on Morning brief" }));
    expect(await screen.findByRole("heading", { level: 1, name: "Morning brief" })).toBeTruthy();
    expect(api.enableAgentSuggestion).toHaveBeenCalledWith("morning-brief");
  });

  it("opens the builder on a copy Home just turned on", async () => {
    stubPond([GREETER]);
    requestOpenAgent(MORNING_COPY);
    render(
      <ConfirmProvider>
        <AgentsView go={() => {}} />
      </ConfirmProvider>,
    );
    expect(await screen.findByRole("heading", { level: 1, name: "Morning brief" })).toBeTruthy();
    await waitFor(() => expect(peekOpenAgent()).toBeNull());
  });
});

describe("start-up copies on the hub's Recipes screen", () => {
  it("shows an agent's copy on its card, and the built-in agents apart, read-only", async () => {
    stubPond([GREETER]);
    render(
      <ConfirmProvider>
        <AgentsView go={() => {}} />
      </ConfirmProvider>,
    );
    const card = await screen.findByRole("article", { name: "Greeter" });
    expect(await within(card).findByText("Ready")).toBeTruthy();
    const group = await screen.findByRole("region", { name: "Built-in agents" });
    expect(within(group).getByRole("listitem", { name: "Point Gander" })).toBeTruthy();
    expect(within(group).queryByRole("button", { name: /edit|delete/i })).toBeNull();
  });

  it("asks for one rebuild from the card", async () => {
    stubPond([GREETER]);
    const rebuild = vi.spyOn(api, "rebuildAgentPrefill").mockResolvedValue(undefined);
    render(
      <ConfirmProvider>
        <AgentsView go={() => {}} />
      </ConfirmProvider>,
    );
    const card = await screen.findByRole("article", { name: "Greeter" });
    fireEvent.click(await within(card).findByRole("button", { name: "Rebuild greeter" }));
    await waitFor(() => expect(rebuild).toHaveBeenCalledWith("greeter"));
  });

  it("rebuilds everything only after the question, which says agents will be slower for a while", async () => {
    stubPond([GREETER]);
    const rebuildAll = vi.spyOn(api, "rebuildAllPrefills").mockResolvedValue(undefined);
    render(
      <ConfirmProvider>
        <AgentsView go={() => {}} />
      </ConfirmProvider>,
    );
    fireEvent.click(await screen.findByRole("button", { name: "Rebuild all" }));
    const dialog = await screen.findByRole("dialog", { name: "Rebuild all" });
    expect(within(dialog).getByText(/slower until the pond is quiet again/)).toBeTruthy();
    expect(rebuildAll).not.toHaveBeenCalled();
    fireEvent.click(within(dialog).getByRole("button", { name: "Rebuild all" }));
    await waitFor(() => expect(rebuildAll).toHaveBeenCalledTimes(1));
  });

  it("shows nothing about them on a pond that has no such route", async () => {
    stubPond([GREETER]);
    vi.spyOn(api, "getAgentPrefills").mockRejectedValue(new ApiError(404, "Not Found"));
    render(
      <ConfirmProvider>
        <AgentsView go={() => {}} />
      </ConfirmProvider>,
    );
    await screen.findByRole("article", { name: "Greeter" });
    await waitFor(() => expect(api.getAgentPrefills).toHaveBeenCalled());
    expect(screen.queryByText("Start-up copy")).toBeNull();
    expect(screen.queryByRole("region", { name: "Built-in agents" })).toBeNull();
  });
});

describe("getting there from the rest of the hub", () => {
  it("keeps the Recipes section inside the hub, from Home", async () => {
    stubPond([GREETER]);
    render(
      <ConfirmProvider>
        <Hub />
      </ConfirmProvider>,
    );
    fireEvent.click(screen.getByText("Open Recipes"));
    expect(await screen.findByRole("heading", { level: 1, name: "Recipes" })).toBeTruthy();
    expect(dispatch).not.toHaveBeenCalledWith({ type: "SET_SECTION", payload: "recipes" });
    expect(localStorage.getItem("goosehub_route")).toBe("agents");
    fireEvent.click(await screen.findByRole("button", { name: /New agent/ }));
    expect(await screen.findByRole("heading", { name: "New agent" })).toBeTruthy();
  });

  it("reaches it from the Recipes chip in the drawer", async () => {
    stubPond([GREETER]);
    render(
      <ConfirmProvider>
        <Hub />
      </ConfirmProvider>,
    );
    fireEvent.click(screen.getByRole("button", { name: "Open menu" }));
    const drawer = screen.getByRole("dialog", { name: "Menu" });
    fireEvent.click(within(drawer).getByRole("button", { name: "Manage" }));
    fireEvent.click(within(drawer).getByRole("button", { name: "Recipes" }));
    expect(await screen.findByRole("heading", { level: 1, name: "Recipes" })).toBeTruthy();
    expect(await screen.findByRole("article", { name: "Greeter" })).toBeTruthy();
    expect(localStorage.getItem("goosehub_route")).toBe("agents");
    expect(dispatch).not.toHaveBeenCalledWith({ type: "SET_SECTION", payload: "recipes" });
  });

  it("opens straight onto the screen when it was the last one left open", async () => {
    stubPond([GREETER]);
    localStorage.setItem("goosehub_route", "agents");
    render(
      <ConfirmProvider>
        <Hub />
      </ConfirmProvider>,
    );
    expect(await screen.findByRole("article", { name: "Greeter" })).toBeTruthy();
  });
});
