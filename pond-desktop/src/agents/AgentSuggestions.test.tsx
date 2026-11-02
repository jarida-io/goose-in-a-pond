import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, cleanup, within } from "@testing-library/react";
import { api } from "../api/PondApiClient";
import { ApiError, type AgentRecipe, type AgentSuggestion } from "../api/types";
import { AgentSuggestions } from "./AgentSuggestions";

vi.mock("../api/PondApiClient", () => ({
  api: {
    listAgentSuggestions: vi.fn(),
    enableAgentSuggestion: vi.fn(),
    dismissAgentSuggestion: vi.fn(),
  },
}));

const MORNING: AgentSuggestion = {
  template: "morning-brief",
  title: "Morning brief",
  description: "Tells the house the weather and the day's plans each weekday.",
  why: "This pond has a weather source and a calendar.",
  needs: ["weather", "schedules"],
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

const NEW_COPY: AgentRecipe = {
  id: "r5",
  name: "morning-brief",
  description: "Tells the house the weather.",
  yaml: "giap_role:\n  tool_groups: []\n",
  active: true,
  owner_profile_id: null,
};

function card(title: string): HTMLElement {
  return screen.getByRole("article", { name: title });
}

beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(api.listAgentSuggestions).mockResolvedValue([MORNING, WELCOME]);
});

afterEach(() => {
  cleanup();
});

describe("a suggested agent", () => {
  it("says what it does, why it fits this pond, and when it would run", async () => {
    render(<AgentSuggestions variant="recipes" onEnabled={() => {}} />);
    const morning = await screen.findByRole("article", { name: "Morning brief" });
    expect(within(morning).getByText("Tells the house the weather and the day's plans each weekday.")).toBeTruthy();
    expect(within(morning).getByText("Why it fits this pond")).toBeTruthy();
    expect(within(morning).getByText("This pond has a weather source and a calendar.")).toBeTruthy();
    expect(within(morning).getByText("When it would run")).toBeTruthy();
    expect(within(morning).getByText("Every weekday at 7:00")).toBeTruthy();
    expect(within(morning).getByText("weather, schedules")).toBeTruthy();
  });

  it("says plainly that nothing runs until it is turned on, and that schedules and events start only after", async () => {
    render(<AgentSuggestions variant="recipes" onEnabled={() => {}} />);
    const morning = await screen.findByRole("article", { name: "Morning brief" });
    expect(
      within(morning).getByText("Nothing runs until you turn it on. A schedule or an event starts only after you do."),
    ).toBeTruthy();
    expect(screen.getByText(/Ready-made agents this pond is able to run/)).toBeTruthy();
  });

  it("shows nothing at all when there is nothing to offer, or the pond cannot say", async () => {
    vi.mocked(api.listAgentSuggestions).mockResolvedValue([]);
    const empty = render(<AgentSuggestions variant="home" onEnabled={() => {}} />);
    await waitFor(() => expect(api.listAgentSuggestions).toHaveBeenCalled());
    expect(empty.container.textContent).toBe("");
    empty.unmount();

    vi.mocked(api.listAgentSuggestions).mockRejectedValue(new ApiError(404, "Not Found"));
    const missing = render(<AgentSuggestions variant="home" onEnabled={() => {}} />);
    await waitFor(() => expect(api.listAgentSuggestions).toHaveBeenCalledTimes(2));
    expect(missing.container.textContent).toBe("");
  });
});

describe("where it shows", () => {
  it("offers one card on Home, and points at the rest on Recipes", async () => {
    const onSeeAll = vi.fn();
    render(<AgentSuggestions variant="home" onEnabled={() => {}} onSeeAll={onSeeAll} />);
    await screen.findByRole("article", { name: "Morning brief" });
    expect(screen.queryByRole("article", { name: "Welcome back" })).toBeNull();
    expect(screen.getByText("A helper you could turn on")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "1 more in Recipes" }));
    expect(onSeeAll).toHaveBeenCalled();
  });

  it("offers every card on the Recipes screen", async () => {
    render(<AgentSuggestions variant="recipes" onEnabled={() => {}} />);
    await screen.findByRole("article", { name: "Morning brief" });
    expect(card("Welcome back")).toBeTruthy();
    expect(screen.getByText("Suggested for this pond")).toBeTruthy();
    expect(screen.queryByText(/more in Recipes/)).toBeNull();
  });
});

describe("turning one on", () => {
  it("copies it with one tap, then hands the new copy over to open in the builder", async () => {
    vi.mocked(api.enableAgentSuggestion).mockResolvedValue({ recipe: NEW_COPY });
    const onEnabled = vi.fn();
    render(<AgentSuggestions variant="recipes" onEnabled={onEnabled} />);
    await screen.findByRole("article", { name: "Morning brief" });
    fireEvent.click(screen.getByRole("button", { name: "Turn on Morning brief" }));
    await waitFor(() => expect(onEnabled).toHaveBeenCalledWith(NEW_COPY));
    expect(api.enableAgentSuggestion).toHaveBeenCalledTimes(1);
    expect(api.enableAgentSuggestion).toHaveBeenCalledWith("morning-brief");
    await waitFor(() => expect(screen.queryByRole("article", { name: "Morning brief" })).toBeNull());
    expect(card("Welcome back")).toBeTruthy();
  });

  it("says Turning on while it works, and cannot be tapped twice", async () => {
    let finish: (value: { recipe: AgentRecipe }) => void = () => {};
    vi.mocked(api.enableAgentSuggestion).mockReturnValue(
      new Promise((resolve) => {
        finish = resolve;
      }),
    );
    render(<AgentSuggestions variant="recipes" onEnabled={() => {}} />);
    await screen.findByRole("article", { name: "Morning brief" });
    const turnOn = screen.getByRole("button", { name: "Turn on Morning brief" }) as HTMLButtonElement;
    fireEvent.click(turnOn);
    await waitFor(() => expect(turnOn.textContent).toBe("Turning on..."));
    expect(turnOn.disabled).toBe(true);
    fireEvent.click(turnOn);
    expect(api.enableAgentSuggestion).toHaveBeenCalledTimes(1);
    finish({ recipe: NEW_COPY });
    await waitFor(() => expect(screen.queryByRole("article", { name: "Morning brief" })).toBeNull());
  });

  it("keeps the card and says what happened when it could not be turned on", async () => {
    vi.mocked(api.enableAgentSuggestion).mockRejectedValue(new ApiError(500, "the store is read-only"));
    const onEnabled = vi.fn();
    render(<AgentSuggestions variant="recipes" onEnabled={onEnabled} />);
    await screen.findByRole("article", { name: "Morning brief" });
    fireEvent.click(screen.getByRole("button", { name: "Turn on Morning brief" }));
    const alert = await within(card("Morning brief")).findByRole("alert");
    expect(alert.textContent).toContain("Could not turn it on. the store is read-only");
    expect(onEnabled).not.toHaveBeenCalled();
    expect((screen.getByRole("button", { name: "Turn on Morning brief" }) as HTMLButtonElement).disabled).toBe(false);
  });

  it("says so when it was already turned on elsewhere, and does not open anything", async () => {
    vi.mocked(api.enableAgentSuggestion).mockRejectedValue(new ApiError(409, "already enabled"));
    const onEnabled = vi.fn();
    render(<AgentSuggestions variant="recipes" onEnabled={onEnabled} />);
    await screen.findByRole("article", { name: "Morning brief" });
    fireEvent.click(screen.getByRole("button", { name: "Turn on Morning brief" }));
    await screen.findByText("Morning brief is already turned on. Find it with your agents.");
    expect(screen.queryByRole("article", { name: "Morning brief" })).toBeNull();
    expect(onEnabled).not.toHaveBeenCalled();
  });
});

describe("dismissing one", () => {
  it("hides the card at once and tells the pond, so it stays gone", async () => {
    vi.mocked(api.dismissAgentSuggestion).mockResolvedValue(undefined);
    render(<AgentSuggestions variant="recipes" onEnabled={() => {}} />);
    await screen.findByRole("article", { name: "Morning brief" });
    fireEvent.click(screen.getByRole("button", { name: "Do not suggest Morning brief again" }));
    expect(screen.queryByRole("article", { name: "Morning brief" })).toBeNull();
    await waitFor(() => expect(api.dismissAgentSuggestion).toHaveBeenCalledWith("morning-brief"));
    expect(card("Welcome back")).toBeTruthy();
  });

  it("brings the card back, with the reason, if the pond could not record it", async () => {
    vi.mocked(api.dismissAgentSuggestion).mockRejectedValue(new ApiError(500, "disk full"));
    render(<AgentSuggestions variant="recipes" onEnabled={() => {}} />);
    await screen.findByRole("article", { name: "Morning brief" });
    fireEvent.click(screen.getByRole("button", { name: "Do not suggest Morning brief again" }));
    const alert = await within(await screen.findByRole("article", { name: "Morning brief" })).findByRole("alert");
    expect(alert.textContent).toContain("Could not hide it. disk full");
  });

  it("goes away entirely once the last card is dismissed", async () => {
    vi.mocked(api.listAgentSuggestions).mockResolvedValue([MORNING]);
    vi.mocked(api.dismissAgentSuggestion).mockResolvedValue(undefined);
    const { container } = render(<AgentSuggestions variant="home" onEnabled={() => {}} />);
    await screen.findByRole("article", { name: "Morning brief" });
    fireEvent.click(screen.getByRole("button", { name: "Do not suggest Morning brief again" }));
    await waitFor(() => expect(container.textContent).toBe(""));
  });
});
