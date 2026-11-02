import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { render, screen, fireEvent, waitFor, within, cleanup } from "@testing-library/react";
import { api } from "../api/PondApiClient";
import { ApiError, type AgentRecipe, type Extension, type RecipeValidation } from "../api/types";
import { AgentBuilder, type AgentBuilderProps } from "./AgentBuilder";
import { agentRow, fakePrefills, reportOf } from "./prefillFixtures";
import type { AgentEnvironment } from "./useAgentEnvironment";

vi.mock("../api/PondApiClient", () => ({
  api: {
    validateRecipe: vi.fn(),
    createRecipe: vi.fn(),
    updateRecipe: vi.fn(),
    removeRecipe: vi.fn(),
    runRecipe: vi.fn(),
    cancelRun: vi.fn(),
    deleteSession: vi.fn(),
  },
}));

const confirmMock = vi.hoisted(() => vi.fn());
vi.mock("../components/shared", async () => {
  const actual = (await vi.importActual("../components/shared")) as Record<string, unknown>;
  return { ...actual, useConfirm: () => confirmMock };
});

const OK: RecipeValidation = {
  ok: true,
  role: { name: "draft", tool_groups: [], activations: [], hooks: { before: [], after: [] }, preamble_chars: 1240 },
  errors: [],
};

function extension(name: string): Extension {
  return { name, kind: "builtin", description: `${name} tools`, tools: ["a", "b"], enabled: true };
}

const ENV: AgentEnvironment = {
  extensions: [extension("giap-weather"), extension("giap-knowledge")],
  members: [
    { id: "amina", name: "Amina" },
    { id: "kofi", name: "Kofi" },
  ],
  devices: [],
  gate: { delegation: true, activations: true, longTasks: true },
  timezone: "Africa/Nairobi",
  enableDelegation: vi.fn(async () => {}),
  prefills: fakePrefills(),
};

const GREETER_YAML = `title: Greeter
giap_role:
  tool_groups: [giap-weather]
  personal_data: inherit
  instructions: |
    Greet whoever arrives.
  activations:
    - delegated
    - schedule: { cron: "0 0 7 * * MON-FRI", brief: "Brief the house." }
  a2a:
    expose: false
`;

function greeter(over: Partial<AgentRecipe> = {}): AgentRecipe {
  return { id: "r1", name: "greeter", description: "Greets people", yaml: GREETER_YAML, active: true, owner_profile_id: "amina", ...over };
}

function setup(over: Partial<AgentBuilderProps> = {}) {
  const props: AgentBuilderProps = {
    variant: "classic",
    recipe: null,
    recipes: [],
    env: ENV,
    onBack: vi.fn(),
    onSaved: vi.fn(),
    onDeleted: vi.fn(),
    ...over,
  };
  const view = render(<AgentBuilder {...props} />);
  return { props, ...view };
}

const lastChecked = () => (vi.mocked(api.validateRecipe).mock.calls.at(-1)?.[0] as string | undefined) ?? "";

function type(label: string, value: string) {
  fireEvent.change(screen.getByLabelText(label), { target: { value } });
}

function openTab(name: "Form" | "YAML") {
  fireEvent.click(screen.getByRole("tab", { name }));
}

function yamlText(): string {
  openTab("YAML");
  const text = (screen.getByLabelText("Agent YAML") as HTMLTextAreaElement).value;
  openTab("Form");
  return text;
}

beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(api.validateRecipe).mockResolvedValue(OK);
  confirmMock.mockResolvedValue(true);
  vi.mocked(ENV.enableDelegation).mockResolvedValue(undefined);
});

afterEach(() => {
  cleanup();
});

describe("opening the builder", () => {
  it("starts a new agent on the form, with a YAML tab and nothing to save yet", () => {
    setup();
    expect(screen.getByRole("heading", { name: "New agent" })).toBeTruthy();
    expect(screen.getByRole("tab", { name: "Form" }).getAttribute("aria-selected")).toBe("true");
    expect(screen.getByRole("tab", { name: "YAML" }).getAttribute("aria-selected")).toBe("false");
    expect((screen.getByRole("button", { name: "Save agent" }) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByRole("status").textContent).toBe("Not saved yet.");
  });

  it("opens an existing agent on its own title, with its fields filled in", () => {
    setup({ recipe: greeter(), recipes: [greeter()] });
    expect(screen.getByRole("heading", { name: "Greeter" })).toBeTruthy();
    expect((screen.getByLabelText("Instructions") as HTMLTextAreaElement).value).toBe("Greet whoever arrives.");
    expect((screen.getByRole("checkbox", { name: /^Weather/ }) as HTMLInputElement).checked).toBe(true);
    expect(screen.getByText("greeter")).toBeTruthy();
  });

  it("takes the touch sizes in the hub and keeps the same parts", () => {
    const { container } = setup({ variant: "hub" });
    expect(container.querySelector(".ag--hub")).toBeTruthy();
    expect(container.querySelector("h1.view-title")).toBeTruthy();
    expect(screen.getByRole("tab", { name: "YAML" })).toBeTruthy();
  });

  it("moves between the tabs with the arrow keys", () => {
    setup();
    const form = screen.getByRole("tab", { name: "Form" });
    form.focus();
    fireEvent.keyDown(form, { key: "ArrowRight" });
    expect(screen.getByRole("tab", { name: "YAML" }).getAttribute("aria-selected")).toBe("true");
    fireEvent.keyDown(screen.getByRole("tab", { name: "YAML" }), { key: "Home" });
    expect(screen.getByRole("tab", { name: "Form" }).getAttribute("aria-selected")).toBe("true");
  });
});

describe("the form and the YAML stay one agent", () => {
  it("writes what is typed in the form into the YAML", () => {
    setup();
    type("Name", "Morning brief");
    type("Instructions", "Brief the house.\nKeep it short.");
    const yaml = yamlText();
    expect(yaml).toContain('title: "Morning brief"');
    expect(yaml).toContain("instructions: |\n    Brief the house.\n    Keep it short.\n");
  });

  it("reads what is typed in the YAML back into the form", () => {
    setup();
    openTab("YAML");
    fireEvent.change(screen.getByLabelText("Agent YAML"), {
      target: {
        value: "title: Typed\ngiap_role:\n  tool_groups: [giap-weather]\n  instructions: From the YAML.\n  activations: [delegated]\n",
      },
    });
    openTab("Form");
    expect((screen.getByLabelText("Name") as HTMLInputElement).value).toBe("Typed");
    expect((screen.getByLabelText("Instructions") as HTMLTextAreaElement).value).toBe("From the YAML.");
    expect((screen.getByRole("checkbox", { name: /^Weather/ }) as HTMLInputElement).checked).toBe(true);
    expect((screen.getByRole("checkbox", { name: /When the assistant hands it a job/ }) as HTMLInputElement).checked).toBe(true);
    expect((screen.getByRole("checkbox", { name: /As a step in a long task/ }) as HTMLInputElement).checked).toBe(false);
  });

  it("says plainly that the form cannot read YAML it cannot read, and takes it back when fixed", () => {
    setup();
    openTab("YAML");
    fireEvent.change(screen.getByLabelText("Agent YAML"), { target: { value: "giap_role: [unclosed\n" } });
    openTab("Form");
    expect(screen.getByText(/The form cannot show this agent yet/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Open the YAML" }));
    fireEvent.change(screen.getByLabelText("Agent YAML"), { target: { value: "giap_role:\n  tool_groups: []\n" } });
    openTab("Form");
    expect(screen.queryByText(/The form cannot show this agent yet/)).toBeNull();
    expect(screen.getByLabelText("Instructions")).toBeTruthy();
  });

  it("keeps what the form has no field for", async () => {
    setup({
      recipe: greeter({ yaml: GREETER_YAML.replace("  personal_data: inherit\n", "  personal_data: inherit\n  model: gemma-4\n") + "parameters:\n  - key: city\n" }),
    });
    type("Instructions", "Greet them warmly.");
    const yaml = yamlText();
    expect(yaml).toContain("model: gemma-4");
    expect(yaml).toContain("parameters:");
    expect(yaml).toContain("key: city");
  });
});

describe("choosing tools", () => {
  it("offers what the pond reports and writes the choice", () => {
    setup();
    fireEvent.click(screen.getByRole("checkbox", { name: /^Look things up and search the web/ }));
    fireEvent.click(screen.getByRole("checkbox", { name: /^Weather/ }));
    expect(yamlText()).toMatch(/tool_groups:\n\s+- giap-knowledge\n\s+- giap-weather/);
  });

  it("says why an agent is never given some tools", () => {
    setup();
    expect(screen.getByText(/never given tools that act outside the pond/)).toBeTruthy();
  });
});

describe("the activation pickers", () => {
  it("write a valid cron as the person chooses, and the pond checks each change", async () => {
    setup();
    fireEvent.click(screen.getByRole("button", { name: "Run on a schedule" }));
    await waitFor(() => expect(lastChecked()).toContain('cron: "0 0 7 * * *"'), { timeout: 2000 });

    fireEvent.change(screen.getByLabelText("Repeats"), { target: { value: "days" } });
    fireEvent.click(screen.getByRole("checkbox", { name: "Wed" }));
    await waitFor(() => expect(lastChecked()).toContain('cron: "0 0 7 * * MON,WED"'), { timeout: 2000 });

    fireEvent.change(screen.getByLabelText("Repeats"), { target: { value: "weekdays" } });
    await waitFor(() => expect(lastChecked()).toContain('cron: "0 0 7 * * MON-FRI"'), { timeout: 2000 });

    fireEvent.change(screen.getByLabelText("Repeats"), { target: { value: "hours" } });
    fireEvent.change(screen.getByLabelText("Every"), { target: { value: "4" } });
    await waitFor(() => expect(lastChecked()).toContain('cron: "0 0 */4 * * *"'), { timeout: 2000 });
  });

  it("change the time of day without losing the pattern", async () => {
    setup();
    fireEvent.click(screen.getByRole("button", { name: "Run on a schedule" }));
    fireEvent.change(screen.getByLabelText("Repeats"), { target: { value: "weekdays" } });
    fireEvent.change(screen.getByLabelText("Activation 3: hour"), { target: { value: "18" } });
    fireEvent.change(screen.getByLabelText("Activation 3: minute"), { target: { value: "30" } });
    await waitFor(() => expect(lastChecked()).toContain('cron: "0 30 18 * * MON-FRI"'), { timeout: 2000 });
    expect(screen.getByText("Every weekday at 18:30")).toBeTruthy();
  });

  it("keep Advanced text exactly as typed", async () => {
    setup();
    fireEvent.click(screen.getByRole("button", { name: "Run on a schedule" }));
    fireEvent.change(screen.getByLabelText("Repeats"), { target: { value: "cron" } });
    fireEvent.change(screen.getByLabelText("Cron"), { target: { value: "0 0 8 * * MON" } });
    await waitFor(() => expect(lastChecked()).toContain('cron: "0 0 8 * * MON"'), { timeout: 2000 });
  });

  it("write an event of each kind with its own fields", async () => {
    setup();
    fireEvent.click(screen.getByRole("button", { name: "Run when something happens" }));
    fireEvent.change(screen.getByLabelText("When"), { target: { value: "sensor" } });
    fireEvent.change(screen.getByLabelText("This number"), { target: { value: "30" } });
    await waitFor(() => expect(lastChecked()).toMatch(/on: sensor\.temperature[\s\S]*above: 30/), { timeout: 2000 });

    fireEvent.change(screen.getByLabelText("When"), { target: { value: "time.daily" } });
    await waitFor(() => expect(lastChecked()).toMatch(/on: time\.daily\n\s+at: "07:30"/), { timeout: 2000 });
  });

  it("let a person turn off what is handed to the agent", async () => {
    setup();
    fireEvent.click(screen.getByRole("checkbox", { name: /As a step in a long task/ }));
    await waitFor(() => expect(lastChecked()).toContain("- delegated"), { timeout: 2000 });
    expect(lastChecked()).not.toContain("task_step");
  });
});

describe("what the pond says is wrong, beside the field it is about", () => {
  const BROKEN = `title: Greeter
giap_role:
  tool_groups: [giap-system]
  activations:
    - delegated
    - event: { on: presence.arivee, brief: "Hi" }
`;

  const ERRORS: RecipeValidation = {
    ok: false,
    role: null,
    errors: [
      { path: "giap_role.activations[1].event.on", message: "`presence.arivee` is not an event; did you mean `presence.arrive`?" },
      { path: "giap_role.instructions", message: "missing; say in plain words what the agent is for (or give the recipe a prompt)" },
      { path: "giap_role.tool_groups[0]", message: "an agent is never given `giap-system`: it acts outside the pond" },
      { path: "giap_role.model", message: "must be a model's name" },
    ],
  };

  it("puts each message by its path, not in a list", async () => {
    vi.mocked(api.validateRecipe).mockResolvedValue(ERRORS);
    setup({ recipe: greeter({ yaml: BROKEN }) });
    await screen.findByText(/did you mean/);

    const activation = screen.getByRole("group", { name: /^Activation 2:/ });
    expect(within(activation).getByText(/did you mean `presence.arrive`/)).toBeTruthy();
    // Activation 1 is `delegated`, a toggle with no group of its own: the message is shown once,
    // by the activation it is about, and nowhere else.
    expect(screen.getAllByText(/did you mean `presence.arrive`/)).toHaveLength(1);

    const instructions = screen.getByLabelText("Instructions");
    expect(instructions.getAttribute("aria-invalid")).toBe("true");
    const described = (instructions.getAttribute("aria-describedby") ?? "")
      .split(" ")
      .map((id) => document.getElementById(id)?.textContent ?? "")
      .join(" ");
    expect(described).toMatch(/Missing; say in plain words what the agent is for/);

    const row = screen.getByRole("checkbox", { name: /^System/ }).closest("li") as HTMLElement;
    expect(within(row).getByText(/never given `giap-system`/)).toBeTruthy();
  });

  it("counts the problems in each section", async () => {
    vi.mocked(api.validateRecipe).mockResolvedValue(ERRORS);
    setup({ recipe: greeter({ yaml: BROKEN }) });
    await screen.findByText(/did you mean/);
    expect(screen.getAllByText("1 problem")).toHaveLength(3);
  });

  it("lists an error the form has no field for, with its path, so none is lost", async () => {
    vi.mocked(api.validateRecipe).mockResolvedValue(ERRORS);
    setup({ recipe: greeter({ yaml: BROKEN }) });
    await screen.findByText("giap_role.model");
    expect(screen.getByText("This must be a model's name.")).toBeTruthy();
  });

  it("shows the whole list with paths on the YAML tab", async () => {
    vi.mocked(api.validateRecipe).mockResolvedValue(ERRORS);
    setup({ recipe: greeter({ yaml: BROKEN }) });
    await screen.findByText(/did you mean/);
    openTab("YAML");
    expect(screen.getByText("giap_role.activations[1].event.on")).toBeTruthy();
    expect(screen.getByText("giap_role.instructions")).toBeTruthy();
  });

  it("keeps a new agent quiet about a section nobody has touched", async () => {
    vi.mocked(api.validateRecipe).mockResolvedValue({
      ok: false,
      role: null,
      errors: [{ path: "giap_role.instructions", message: "missing; say in plain words what the agent is for" }],
    });
    setup();
    await waitFor(() => expect(api.validateRecipe).toHaveBeenCalled(), { timeout: 2000 });
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(screen.queryByText(/Missing; say in plain words/)).toBeNull();
    type("Instructions", "x");
    await waitFor(() => expect(screen.getByText(/Missing; say in plain words/)).toBeTruthy(), { timeout: 2000 });
  });
});

describe("the cost to start", () => {
  it("shows the size and the first compile as estimates, and says it is paid once", async () => {
    setup({ recipe: greeter() });
    await screen.findByText("310 tokens");
    expect(screen.getByText("0.6 s")).toBeTruthy();
    expect(screen.getByText(/Both are estimates/)).toBeTruthy();
    expect(screen.getByText(/1,240 characters at about 4 to a token/)).toBeTruthy();
    expect(screen.getByText(/about 530 tokens a second, as measured on an Orin Nano/)).toBeTruthy();
    expect(screen.getByText(/You pay this once, when the pond compiles the agent/)).toBeTruthy();
  });

  it("asks for the problems to be fixed rather than showing a size that is not true", async () => {
    vi.mocked(api.validateRecipe).mockResolvedValue({
      ok: false,
      role: null,
      errors: [{ path: "giap_role.instructions", message: "missing; say what it is for" }],
    });
    setup({ recipe: greeter() });
    await screen.findByText(/Fix the problems on this page to see how big the agent is/);
  });
});

describe("saving", () => {
  it("creates a new agent under the name made from its title", async () => {
    const saved = greeter({ id: "r9", name: "morning-brief", description: "" });
    vi.mocked(api.createRecipe).mockResolvedValue(saved);
    const { props } = setup();
    type("Name", "Morning brief");
    type("Instructions", "Brief the house.");
    const save = screen.getByRole("button", { name: "Save agent" }) as HTMLButtonElement;
    await waitFor(() => expect(save.disabled).toBe(false), { timeout: 2000 });
    fireEvent.click(save);
    await waitFor(() => expect(props.onSaved).toHaveBeenCalledWith(saved));
    expect(api.createRecipe).toHaveBeenCalledWith({
      name: "morning-brief",
      description: "",
      yaml: expect.stringContaining('title: "Morning brief"'),
    });
    expect(screen.getByRole("status").textContent).toBe("Saved.");
    expect(screen.queryByLabelText("Saved as")).toBeNull();
  });

  it("refuses a name that is taken before asking the pond, because saving would replace that recipe", async () => {
    const taken = greeter({ id: "x", name: "morning-brief" });
    setup({ recipes: [taken] });
    type("Name", "Morning brief");
    type("Instructions", "Brief the house.");
    expect(screen.getByText(/A recipe named "morning-brief" already exists/)).toBeTruthy();
    const save = screen.getByRole("button", { name: "Save agent" }) as HTMLButtonElement;
    await waitFor(() => expect(save.disabled).toBe(false), { timeout: 2000 });
    fireEvent.click(save);
    expect(api.createRecipe).not.toHaveBeenCalled();
  });

  it("updates an existing agent by its id", async () => {
    vi.mocked(api.updateRecipe).mockResolvedValue(greeter());
    const { props } = setup({ recipe: greeter(), recipes: [greeter()] });
    type("In one line", "Greets everyone");
    const save = screen.getByRole("button", { name: "Save agent" }) as HTMLButtonElement;
    await waitFor(() => expect(save.disabled).toBe(false), { timeout: 2000 });
    fireEvent.click(save);
    await waitFor(() => expect(props.onSaved).toHaveBeenCalled());
    expect(api.updateRecipe).toHaveBeenCalledWith("r1", { description: "Greets everyone", yaml: expect.stringContaining("Greets everyone") });
    expect(api.createRecipe).not.toHaveBeenCalled();
  });

  it("does not save a block the pond refused, and focuses the first field to fix", async () => {
    vi.mocked(api.validateRecipe).mockResolvedValue({
      ok: false,
      role: null,
      errors: [{ path: "giap_role.instructions", message: "missing; say in plain words what the agent is for" }],
    });
    setup({ recipe: greeter({ yaml: "giap_role:\n  tool_groups: []\n" }) });
    await screen.findByText(/Missing; say in plain words/);
    type("In one line", "x");
    const save = screen.getByRole("button", { name: "Save agent" }) as HTMLButtonElement;
    await waitFor(() => expect(save.disabled).toBe(false), { timeout: 2000 });
    fireEvent.click(save);
    expect(api.updateRecipe).not.toHaveBeenCalled();
    await waitFor(() => expect(document.activeElement).toBe(screen.getByLabelText("Instructions")));
  });

  it("says what the pond answered when a save fails, and keeps the edits", async () => {
    vi.mocked(api.updateRecipe).mockRejectedValue(new ApiError(500, "database is locked"));
    setup({ recipe: greeter(), recipes: [greeter()] });
    type("In one line", "Greets everyone");
    const save = screen.getByRole("button", { name: "Save agent" }) as HTMLButtonElement;
    await waitFor(() => expect(save.disabled).toBe(false), { timeout: 2000 });
    fireEvent.click(save);
    const alert = await screen.findByRole("alert");
    expect(alert.textContent).toMatch(/Could not save the agent. The pond said: database is locked/);
    expect((screen.getByLabelText("In one line") as HTMLInputElement).value).toBe("Greets everyone");
  });

  it("saves an agent unchecked, and says so, on a pond with no validate route", async () => {
    vi.mocked(api.validateRecipe).mockRejectedValue(new ApiError(404, "Not Found"));
    vi.mocked(api.updateRecipe).mockResolvedValue(greeter());
    setup({ recipe: greeter(), recipes: [greeter()] });
    await screen.findByText(/This pond cannot check agents yet/);
    type("In one line", "x");
    const save = screen.getByRole("button", { name: "Save agent" }) as HTMLButtonElement;
    await waitFor(() => expect(save.disabled).toBe(false), { timeout: 2000 });
    fireEvent.click(save);
    await waitFor(() => expect(api.updateRecipe).toHaveBeenCalled());
  });
});

describe("leaving and deleting", () => {
  it("leaves at once when nothing has changed", async () => {
    const { props } = setup({ recipe: greeter() });
    fireEvent.click(screen.getByRole("button", { name: "Agents" }));
    await waitFor(() => expect(props.onBack).toHaveBeenCalled());
    expect(confirmMock).not.toHaveBeenCalled();
  });

  it("asks before throwing changes away", async () => {
    const { props } = setup({ recipe: greeter() });
    type("In one line", "changed");
    confirmMock.mockResolvedValueOnce(false);
    fireEvent.click(screen.getByRole("button", { name: "Agents" }));
    await waitFor(() => expect(confirmMock).toHaveBeenCalledWith(expect.stringMatching(/Leave without saving/), expect.anything()));
    expect(props.onBack).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole("button", { name: "Agents" }));
    await waitFor(() => expect(props.onBack).toHaveBeenCalled());
  });

  it("deletes a saved agent after confirming, and says what stops", async () => {
    vi.mocked(api.removeRecipe).mockResolvedValue(undefined);
    const { props } = setup({ recipe: greeter() });
    fireEvent.click(screen.getByRole("button", { name: /Delete agent/ }));
    await waitFor(() => expect(props.onDeleted).toHaveBeenCalled());
    expect(confirmMock).toHaveBeenCalledWith(expect.stringMatching(/schedules and events stop/), expect.objectContaining({ destructive: true }));
    expect(api.removeRecipe).toHaveBeenCalledWith("r1");
  });

  it("offers no delete for an agent that is not saved", () => {
    setup();
    expect(screen.queryByRole("button", { name: /Delete agent/ })).toBeNull();
  });
});

describe("the owner, and whether anything will start", () => {
  it("names the owner", () => {
    setup({ recipe: greeter() });
    expect(screen.getByText("Owner: Amina")).toBeTruthy();
  });

  it("says plainly that an agent with no owner, in a household of several, will not run its schedules", () => {
    setup({ recipe: greeter({ owner_profile_id: null }) });
    expect(screen.getByText("No owner. It was saved from a device that is not paired to a household member.")).toBeTruthy();
    expect(screen.getByText(/more than one member, so its scheduled and event runs will not start/)).toBeTruthy();
    expect(screen.getByText("There is no way to set an owner from here yet.")).toBeTruthy();
  });

  it("does not warn a household of one", () => {
    setup({ recipe: greeter({ owner_profile_id: null }), env: { ...ENV, members: [{ id: "amina", name: "Amina" }] } });
    expect(screen.getByText(/^No owner\./)).toBeTruthy();
    expect(screen.queryByText(/will not start/)).toBeNull();
  });

  it("says an unsaved agent gets its owner when first saved", () => {
    setup();
    expect(screen.getByText(/gets an owner when it is first saved/)).toBeTruthy();
  });

  it("says nothing here will start while Delegation is off, and offers to turn it on", async () => {
    const enable = vi.fn(async () => {});
    setup({ env: { ...ENV, gate: { delegation: false, activations: true, longTasks: true }, enableDelegation: enable } });
    expect(screen.getByText(/Nothing here will start by itself on this pond right now/)).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Turn on Delegation" }));
    await waitFor(() => expect(enable).toHaveBeenCalled());
  });

  it("stays quiet about the gate when it is open or unknown", () => {
    setup({ env: { ...ENV, gate: null } });
    expect(screen.queryByText(/Nothing here will start by itself/)).toBeNull();
  });
});

describe("hooks and sharing", () => {
  it("writes the hooks that are ticked, and hands a result to another agent", async () => {
    const other = greeter({ id: "r2", name: "trip-planner" });
    setup({ recipes: [other] });
    fireEvent.click(screen.getByRole("checkbox", { name: /^The weather/ }));
    fireEvent.click(screen.getByRole("checkbox", { name: /^Tell its owner/ }));
    fireEvent.click(screen.getByRole("checkbox", { name: /^Hand the result to another agent/ }));
    await waitFor(() => expect(lastChecked()).toMatch(/hand_to: trip-planner/), { timeout: 2000 });
    expect(lastChecked()).toMatch(/before:\n\s+- weather/);
    expect(lastChecked()).toMatch(/- notify_owner/);
  });

  it("cannot give a denied-data agent the presence hook", () => {
    setup({ recipe: greeter({ yaml: GREETER_YAML.replace("personal_data: inherit", "personal_data: deny") }) });
    const presence = screen.getByRole("checkbox", { name: /^Who is home/ }) as HTMLInputElement;
    expect(presence.disabled).toBe(true);
    expect(screen.getByText("Not available while personal data is denied.")).toBeTruthy();
  });

  it("keeps sharing off, says what it would mean, and asks before turning it on", async () => {
    setup();
    const share = screen.getByRole("switch", { name: "Offer this agent to other devices" });
    expect(share.getAttribute("aria-checked")).toBe("false");
    expect(screen.getByText(/This version of the pond only records the choice: nothing is offered yet/)).toBeTruthy();

    confirmMock.mockResolvedValueOnce(false);
    fireEvent.click(share);
    await waitFor(() => expect(confirmMock).toHaveBeenCalled());
    expect(share.getAttribute("aria-checked")).toBe("false");

    fireEvent.click(share);
    await waitFor(() => expect(share.getAttribute("aria-checked")).toBe("true"));
    await waitFor(() => expect(lastChecked()).toMatch(/expose: true/), { timeout: 2000 });
  });
});

describe("a test run", () => {
  function stream(events: Array<Record<string, unknown>>) {
    vi.mocked(api.runRecipe).mockImplementation((async function* () {
      for (const event of events) yield event;
    }) as never);
  }

  it("cannot start before the agent is saved, and says why", () => {
    setup();
    expect((screen.getByRole("button", { name: "Run test" }) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByText("Save the agent first, so the pond has it to run.")).toBeTruthy();
  });

  it("cannot start with unsaved changes, because it runs the saved agent", () => {
    setup({ recipe: greeter() });
    type("In one line", "changed");
    expect((screen.getByRole("button", { name: "Run test" }) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByText("Save your changes first: a test runs the saved agent.")).toBeTruthy();
  });

  it("says what it does and does not do", () => {
    setup({ recipe: greeter() });
    expect(screen.getByText(/It does not use this agent's own tool list, schedule or hooks/)).toBeTruthy();
  });

  it("streams the run into a panel, names the tools it used, and offers to remove the conversation", async () => {
    stream([
      { type: "run_started", run_id: "run1" },
      { type: "text", content: "Good " },
      { type: "tool_call", tool: "giap-weather__get_current_weather" },
      { type: "text", content: "morning." },
      { type: "done", session_id: "s1" },
    ]);
    vi.mocked(api.deleteSession).mockResolvedValue(undefined);
    setup({ recipe: greeter() });
    fireEvent.click(screen.getByRole("button", { name: "Run test" }));

    const log = await screen.findByRole("log", { name: "Test run output" });
    await waitFor(() => expect(log.textContent).toContain("Good morning."));
    expect(api.runRecipe).toHaveBeenCalledWith("greeter");
    expect(log.textContent).toContain("Used: Weather");
    await waitFor(() => expect(log.textContent).toMatch(/Finished in/));

    fireEvent.click(screen.getByRole("button", { name: "Remove this test from chat history" }));
    await waitFor(() => expect(api.deleteSession).toHaveBeenCalledWith("s1"));
    await screen.findByText("Removed from chat history.");
  });

  it("hides the model's thinking from the output", async () => {
    stream([
      { type: "text", content: "<think>private " },
      { type: "text", content: "thoughts</think>Hello." },
      { type: "done" },
    ]);
    setup({ recipe: greeter() });
    fireEvent.click(screen.getByRole("button", { name: "Run test" }));
    const log = await screen.findByRole("log", { name: "Test run output" });
    await waitFor(() => expect(log.textContent).toContain("Hello."));
    expect(log.textContent).not.toContain("private");
  });

  it("says when the run ends with an error", async () => {
    stream([{ type: "text", content: "Starting" }, { type: "error", error: "the model is not loaded" }]);
    setup({ recipe: greeter() });
    fireEvent.click(screen.getByRole("button", { name: "Run test" }));
    const log = await screen.findByRole("log", { name: "Test run output" });
    await waitFor(() => expect(log.textContent).toContain("The test run ended with an error."));
    expect(log.textContent).toContain("the model is not loaded");
  });

  it("stops a run and cancels it when it has an id", async () => {
    vi.mocked(api.cancelRun).mockResolvedValue(undefined);
    let release: () => void = () => {};
    vi.mocked(api.runRecipe).mockImplementation((async function* () {
      yield { type: "run_started", run_id: "run7" };
      await new Promise<void>((resolve) => {
        release = resolve;
      });
      yield { type: "text", content: "late" };
    }) as never);
    setup({ recipe: greeter() });
    fireEvent.click(screen.getByRole("button", { name: "Run test" }));
    await screen.findByRole("button", { name: "Stop" });
    await waitFor(() => expect(api.runRecipe).toHaveBeenCalled());
    await new Promise((r) => setTimeout(r, 10));
    fireEvent.click(screen.getByRole("button", { name: "Stop" }));
    await waitFor(() => expect(api.cancelRun).toHaveBeenCalledWith("run7"));
    release();
    expect((await screen.findByRole("log", { name: "Test run output" })).textContent).toContain("Stopped.");
    expect(screen.queryByText("late")).toBeNull();
  });

  it("keeps the test prompt with the agent", async () => {
    setup({ recipe: greeter() });
    type("Test prompt", "Greet Amina.");
    expect(yamlText()).toContain('prompt: "Greet Amina."');
  });
});

describe("the examples", () => {
  it("adds, writes and removes an example, up to the pond's cap", () => {
    setup();
    fireEvent.click(screen.getByRole("button", { name: "Add an example" }));
    fireEvent.change(screen.getByLabelText("A job it might be given"), { target: { value: "Brief me" } });
    fireEvent.change(screen.getByLabelText("The answer you would want"), { target: { value: "It is sunny." } });
    expect(yamlText()).toMatch(/examples:\n\s+- brief: "Brief me"\n\s+answer: "It is sunny\."/);
    fireEvent.click(screen.getByRole("button", { name: "Remove example 1" }));
    expect(screen.queryByLabelText("A job it might be given")).toBeNull();
    for (let i = 0; i < 8; i++) fireEvent.click(screen.getByRole("button", { name: "Add an example" }));
    expect((screen.getByRole("button", { name: "Add an example" }) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getByText("8 of 8")).toBeTruthy();
  });
});

describe("the start-up copy", () => {
  function envWith(status: Parameters<typeof agentRow>[1] = {}) {
    const prefills = fakePrefills({
      report: reportOf([agentRow("greeter", status)]),
      rebuild: vi.fn(async () => true),
      refresh: vi.fn(),
    });
    return { env: { ...ENV, prefills }, prefills };
  }

  it("says whether a saved agent's copy is ready, and when it was built and last used", () => {
    setup({ recipe: greeter(), recipes: [greeter()], env: envWith().env });
    expect(screen.getByText("Start-up copy")).toBeTruthy();
    expect(screen.getByText("Ready")).toBeTruthy();
    expect(screen.getByText("Built 2 days ago. Last used 3 hours ago.")).toBeTruthy();
  });

  it("says in plain words when it is resting", () => {
    const { env } = envWith({ status: "idle", last_used_at: new Date(Date.now() - 20 * 86_400_000).toISOString() });
    setup({ recipe: greeter(), recipes: [greeter()], env });
    expect(screen.getByText("Resting: unused for 20 days, rebuilt on its next run")).toBeTruthy();
  });

  it("asks for a rebuild of this agent", () => {
    const { env, prefills } = envWith();
    setup({ recipe: greeter(), recipes: [greeter()], env });
    fireEvent.click(screen.getByRole("button", { name: "Rebuild greeter" }));
    expect(prefills.rebuild).toHaveBeenCalledWith("greeter");
  });

  it("shows nothing for an agent that is not saved yet", () => {
    setup({ env: envWith().env });
    expect(screen.queryByText("Start-up copy")).toBeNull();
  });

  it("asks the pond how the copy stands again once the agent is saved, since saving rebuilds it", async () => {
    vi.mocked(api.updateRecipe).mockResolvedValue(greeter());
    const { env, prefills } = envWith();
    setup({ recipe: greeter(), recipes: [greeter()], env });
    type("In one line", "Greets everyone");
    const save = screen.getByRole("button", { name: "Save agent" }) as HTMLButtonElement;
    await waitFor(() => expect(save.disabled).toBe(false), { timeout: 2000 });
    fireEvent.click(save);
    await waitFor(() => expect(prefills.refresh).toHaveBeenCalledTimes(1));
  });
});
