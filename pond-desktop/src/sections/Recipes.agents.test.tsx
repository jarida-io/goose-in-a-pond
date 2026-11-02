import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, within, cleanup } from "@testing-library/react";
import { api } from "../api/PondApiClient";
import { ApiError, type AgentRecipe, type AgentSuggestion, type RecipeValidation } from "../api/types";
import { clearOpenAgent, peekOpenAgent, requestOpenAgent } from "../agents/openAgent";
import { agentRow, readyReport, reportOf } from "../agents/prefillFixtures";
import { Recipes } from "./Recipes";

vi.mock("../api/PondApiClient", () => ({
  api: {
    listRecipes: vi.fn(),
    createRecipe: vi.fn(),
    updateRecipe: vi.fn(),
    removeRecipe: vi.fn(),
    runRecipe: vi.fn(),
    cancelRun: vi.fn(),
    deleteSession: vi.fn(),
    validateRecipe: vi.fn(),
    listExtensions: vi.fn(),
    listProfiles: vi.fn(),
    listDevices: vi.fn(),
    getSettings: vi.fn(),
    updateSettings: vi.fn(),
    listAgentSuggestions: vi.fn(),
    enableAgentSuggestion: vi.fn(),
    dismissAgentSuggestion: vi.fn(),
    getAgentPrefills: vi.fn(),
    rebuildAgentPrefill: vi.fn(),
    rebuildAllPrefills: vi.fn(),
  },
}));

const confirmMock = vi.hoisted(() => vi.fn());
vi.mock("../components/shared", async () => {
  const actual = (await vi.importActual("../components/shared")) as Record<string, unknown>;
  return { ...actual, useConfirm: () => confirmMock };
});

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
  yaml: `title: Greeter
giap_role:
  tool_groups: [giap-weather]
  instructions: |
    Greet whoever arrives.
  activations:
    - delegated
    - schedule: { cron: "0 0 7 * * MON-FRI", brief: "Brief the house." }
  hooks:
    before: [today]
    after: [notify_owner]
`,
  active: true,
  owner_profile_id: "amina",
};

const DAILY: AgentRecipe = {
  id: "r2",
  name: "daily_summary",
  title: "Daily summary",
  description: "Summarize the day",
  yaml: 'title: "Daily summary"\nprompt: "Summarize the day"\n',
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

beforeEach(() => {
  vi.resetAllMocks();
  clearOpenAgent();
  vi.mocked(api.listRecipes).mockResolvedValue([GREETER, DAILY]);
  vi.mocked(api.listExtensions).mockResolvedValue({ extensions: [] });
  vi.mocked(api.listProfiles).mockResolvedValue({
    profiles: [
      { id: "amina", display_name: "Amina", avatar_emoji: "" },
      { id: "kofi", display_name: "Kofi", avatar_emoji: "" },
    ],
  });
  vi.mocked(api.listDevices).mockResolvedValue([]);
  vi.mocked(api.getSettings).mockResolvedValue({ ext_orchestrator_enabled: true } as never);
  vi.mocked(api.validateRecipe).mockResolvedValue(OK);
  vi.mocked(api.listAgentSuggestions).mockResolvedValue([]);
  vi.mocked(api.getAgentPrefills).mockResolvedValue(readyReport("greeter"));
  vi.mocked(api.rebuildAgentPrefill).mockResolvedValue(undefined);
  vi.mocked(api.rebuildAllPrefills).mockResolvedValue(undefined);
  confirmMock.mockResolvedValue(true);
});

afterEach(() => {
  cleanup();
  clearOpenAgent();
});

describe("the Recipes screen, split into agents and other recipes", () => {
  it("lists agents in one place and plain recipes in another, each once", async () => {
    render(<Recipes />);
    await screen.findByRole("article", { name: "Greeter" });
    const agents = screen.getByRole("region", { name: /^Agents/ });
    const others = screen.getByRole("region", { name: /^Other recipes/ });
    expect(within(agents).getByRole("article", { name: "Greeter" })).toBeTruthy();
    expect(within(agents).queryByText("Daily summary")).toBeNull();
    expect(within(others).getByText("Daily summary")).toBeTruthy();
    expect(within(others).queryByText("Greeter")).toBeNull();
  });

  it("says how an agent starts and what surrounds a run, in plain words", async () => {
    render(<Recipes />);
    const card = await screen.findByRole("article", { name: "Greeter" });
    expect(within(card).getByText("Greets whoever arrives")).toBeTruthy();
    expect(within(card).getByText("When the assistant hands it a job")).toBeTruthy();
    expect(within(card).getByText("Every weekday at 7:00")).toBeTruthy();
    expect(within(card).getByText("Before it runs, it is given the day's schedule")).toBeTruthy();
    expect(within(card).getByText("Afterwards, it tells its owner")).toBeTruthy();
    expect(await within(card).findByText("Owner: Amina")).toBeTruthy();
  });

  it("warns on the card when nothing will start it by itself", async () => {
    vi.mocked(api.getSettings).mockResolvedValue({ ext_orchestrator_enabled: false } as never);
    render(<Recipes />);
    const card = await screen.findByRole("article", { name: "Greeter" });
    expect(await within(card).findByText("It will not start by itself. Delegation is off in Settings.")).toBeTruthy();
  });

  it("warns on the card when it has no owner in a household of several", async () => {
    vi.mocked(api.listRecipes).mockResolvedValue([{ ...GREETER, owner_profile_id: null }]);
    render(<Recipes />);
    const card = await screen.findByRole("article", { name: "Greeter" });
    expect(within(card).getByText("No owner. It was saved from a device that is not paired to a household member.")).toBeTruthy();
    expect(await within(card).findByText(/will not start/)).toBeTruthy();
  });

  it("invites a first agent when there are none, and says the other recipes are empty too", async () => {
    vi.mocked(api.listRecipes).mockResolvedValue([]);
    render(<Recipes />);
    expect(await screen.findByText(/No agents yet/)).toBeTruthy();
    expect(screen.getByText(/No other recipes yet/)).toBeTruthy();
    expect(screen.getAllByRole("button", { name: /New agent/ })).toHaveLength(2);
  });

  it("shows a skeleton while the recipes load", () => {
    vi.mocked(api.listRecipes).mockReturnValue(new Promise(() => {}));
    render(<Recipes />);
    expect(screen.getByLabelText("Loading")).toBeTruthy();
  });

  it("says what went wrong and offers to try again when the recipes cannot load", async () => {
    vi.mocked(api.listRecipes).mockRejectedValue(new Error("network down"));
    render(<Recipes />);
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toMatch(/Could not reach the server/);
    vi.mocked(api.listRecipes).mockResolvedValue([GREETER]);
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    await screen.findByRole("article", { name: "Greeter" });
    expect(api.listRecipes).toHaveBeenCalledTimes(2);
  });
});

describe("the classic screen reaches the builder", () => {
  it("opens a new agent from New agent, and comes back to the list", async () => {
    render(<Recipes />);
    await screen.findByRole("article", { name: "Greeter" });
    fireEvent.click(screen.getByRole("button", { name: /New agent/ }));
    expect(await screen.findByRole("heading", { name: "New agent" })).toBeTruthy();
    expect(screen.getByRole("tab", { name: "Form" })).toBeTruthy();
    expect(screen.getByRole("tab", { name: "YAML" })).toBeTruthy();
    expect(screen.queryByRole("region", { name: /^Agents/ })).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Agents" }));
    await screen.findByRole("region", { name: /^Agents/ });
    expect(api.listRecipes).toHaveBeenCalledTimes(2);
  });

  it("opens an existing agent in the builder from its card", async () => {
    render(<Recipes />);
    fireEvent.click(await screen.findByRole("button", { name: "Edit Greeter" }));
    expect(await screen.findByRole("heading", { name: "Greeter" })).toBeTruthy();
    expect((screen.getByLabelText("Instructions") as HTMLTextAreaElement).value).toBe("Greet whoever arrives.");
  });

  it("still opens a plain recipe in the recipe form, not the builder", async () => {
    render(<Recipes />);
    fireEvent.click(await screen.findByRole("button", { name: "Edit Daily summary" }));
    expect(await screen.findByText("Edit recipe")).toBeTruthy();
    expect(screen.queryByRole("tab", { name: "YAML" })).toBeNull();
  });

  it("deletes an agent from its card after confirming", async () => {
    vi.mocked(api.removeRecipe).mockResolvedValue(undefined);
    render(<Recipes />);
    fireEvent.click(await screen.findByRole("button", { name: "Delete Greeter" }));
    await waitFor(() => expect(api.removeRecipe).toHaveBeenCalledWith("r1"));
    expect(confirmMock).toHaveBeenCalledWith(expect.stringMatching(/Delete agent "Greeter"/), expect.objectContaining({ destructive: true }));
  });
});

describe("suggested agents on the Recipes screen", () => {
  it("offers a card, and turning it on opens the builder on the new copy", async () => {
    vi.mocked(api.listAgentSuggestions).mockResolvedValue([MORNING]);
    vi.mocked(api.enableAgentSuggestion).mockResolvedValue({ recipe: MORNING_COPY });
    render(<Recipes />);
    const suggestion = await screen.findByRole("article", { name: "Morning brief" });
    expect(within(suggestion).getByText("This pond has a weather source.")).toBeTruthy();
    expect(within(suggestion).getByText(/Nothing runs until you turn it on/)).toBeTruthy();

    fireEvent.click(screen.getByRole("button", { name: "Turn on Morning brief" }));
    expect(await screen.findByRole("heading", { name: "Morning brief" })).toBeTruthy();
    expect(screen.getByRole("tab", { name: "YAML" })).toBeTruthy();
    expect((screen.getByLabelText("Instructions") as HTMLTextAreaElement).value).toBe("Brief the house.");
    expect(api.enableAgentSuggestion).toHaveBeenCalledWith("morning-brief");
    await waitFor(() => expect(api.listRecipes).toHaveBeenCalledTimes(2));
  });

  it("opens the builder on a copy Home just turned on", async () => {
    requestOpenAgent(MORNING_COPY);
    render(<Recipes />);
    expect(await screen.findByRole("heading", { name: "Morning brief" })).toBeTruthy();
    expect(screen.getByRole("tab", { name: "Form" })).toBeTruthy();
    await waitFor(() => expect(peekOpenAgent()).toBeNull());
  });

  it("shows no suggestion section when there is nothing to suggest", async () => {
    render(<Recipes />);
    await screen.findByRole("article", { name: "Greeter" });
    expect(screen.queryByText("Suggested for this pond")).toBeNull();
  });
});

describe("start-up copies on the Recipes screen", () => {
  it("says on an agent's card whether its start-up copy is ready, and when it was built and used", async () => {
    render(<Recipes />);
    const card = await screen.findByRole("article", { name: "Greeter" });
    expect(await within(card).findByText("Start-up copy")).toBeTruthy();
    expect(within(card).getByText("Ready")).toBeTruthy();
    expect(within(card).getByText("Built 2 days ago. Last used 3 hours ago.")).toBeTruthy();
  });

  it("says in plain words when the copy is waiting, resting or failed", async () => {
    vi.mocked(api.getAgentPrefills).mockResolvedValue(
      reportOf([agentRow("greeter", { status: "failed" }), agentRow("long-task-planner", { status: "queued" })]),
    );
    render(<Recipes />);
    const card = await screen.findByRole("article", { name: "Greeter" });
    expect(await within(card).findByText("Couldn't build")).toBeTruthy();
    expect(within(await screen.findByRole("region", { name: "Built-in agents" })).getByText("Waiting to be rebuilt")).toBeTruthy();
  });

  it("asks for one agent to be rebuilt from its card", async () => {
    render(<Recipes />);
    const card = await screen.findByRole("article", { name: "Greeter" });
    fireEvent.click(await within(card).findByRole("button", { name: "Rebuild greeter" }));
    await waitFor(() => expect(api.rebuildAgentPrefill).toHaveBeenCalledWith("greeter"));
    await waitFor(() => expect(api.getAgentPrefills).toHaveBeenCalledTimes(2));
  });

  it("lists the built-in agents apart, with their state, and without edit or delete", async () => {
    render(<Recipes />);
    const group = await screen.findByRole("region", { name: "Built-in agents" });
    expect(within(group).getAllByText("Ready")).toHaveLength(3);
    expect(within(group).getByRole("listitem", { name: "Point Gander" })).toBeTruthy();
    expect(within(group).queryByRole("button", { name: /edit|delete/i })).toBeNull();
    expect(within(group).queryByText("greeter")).toBeNull();
    fireEvent.click(within(group).getByRole("button", { name: "Rebuild Sentinel Goose" }));
    await waitFor(() => expect(api.rebuildAgentPrefill).toHaveBeenCalledWith("long-task-verifier"));
  });

  it("rebuilds everything only after saying agents will be slower until the pond is quiet again", async () => {
    render(<Recipes />);
    fireEvent.click(await screen.findByRole("button", { name: "Rebuild all" }));
    await waitFor(() => expect(api.rebuildAllPrefills).toHaveBeenCalledTimes(1));
    expect(confirmMock).toHaveBeenCalledWith(
      expect.stringMatching(/Agents will be slower until the pond is quiet again/),
      expect.objectContaining({ confirmLabel: "Rebuild all" }),
    );
  });

  it("does not rebuild everything when the question is answered no", async () => {
    confirmMock.mockResolvedValue(false);
    render(<Recipes />);
    fireEvent.click(await screen.findByRole("button", { name: "Rebuild all" }));
    await waitFor(() => expect(confirmMock).toHaveBeenCalled());
    expect(api.rebuildAllPrefills).not.toHaveBeenCalled();
  });

  it("shows an agent's copy in its builder too, with Rebuild", async () => {
    render(<Recipes />);
    fireEvent.click(await screen.findByRole("button", { name: "Edit Greeter" }));
    await screen.findByRole("heading", { name: "Greeter" });
    expect(await screen.findByText("Start-up copy")).toBeTruthy();
    expect(screen.getByText("Ready")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Rebuild greeter" })).toBeTruthy();
  });

  it("shows nothing about start-up copies on a pond that has no such route", async () => {
    vi.mocked(api.getAgentPrefills).mockRejectedValue(new ApiError(404, "Not Found"));
    render(<Recipes />);
    await screen.findByRole("article", { name: "Greeter" });
    await waitFor(() => expect(api.getAgentPrefills).toHaveBeenCalled());
    expect(screen.queryByText("Start-up copy")).toBeNull();
    expect(screen.queryByRole("region", { name: "Built-in agents" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Rebuild all" })).toBeNull();
  });
});
