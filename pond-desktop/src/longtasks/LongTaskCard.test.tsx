import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { api } from "../api/PondApiClient";
import { LongTaskCard } from "./LongTaskCard";
import { LongTaskCards, LongTaskStrays } from "./LongTaskCards";
import { artifactOf, doneTask, fakeLongTasks, OTHER_ID, stepOf, TASK_ID, taskOf } from "./longTaskFixtures";
import { ApiError } from "../api/types";
import type { LongTask, LongTaskReport } from "../api/types";

vi.mock("../api/PondApiClient", () => ({
  api: { getLongTaskArtifact: vi.fn(), getLongTaskReport: vi.fn(), getLongTaskExport: vi.fn() },
}));

vi.mock("../lib/saveFile", async (importOriginal) => ({ ...(await importOriginal<typeof import("../lib/saveFile")>()), saveBlob: vi.fn() }));
vi.mock("../lib/printHtml", () => ({ printHtml: vi.fn() }));

const confirmMock = vi.hoisted(() => vi.fn());
vi.mock("../components/shared", async () => {
  const actual = (await vi.importActual("../components/shared")) as Record<string, unknown>;
  return { ...actual, useConfirm: () => confirmMock };
});

beforeEach(() => {
  vi.resetAllMocks();
  confirmMock.mockResolvedValue(true);
});

afterEach(() => {
  cleanup();
});

function show(task: LongTask, over: Parameters<typeof fakeLongTasks>[0] = {}, variant: "classic" | "hub" = "classic") {
  const longTasks = fakeLongTasks({ tasks: { [task.id]: task }, stop: vi.fn(async () => {}), retry: vi.fn(), ...over });
  render(<LongTaskCard id={task.id} longTasks={longTasks} variant={variant} />);
  return longTasks;
}

const stopButton = () => screen.queryByRole("button", { name: /^Stop the task/ });

describe("a long task under way", () => {
  it("says what it is for and how far it has got", () => {
    show(taskOf());
    expect(screen.getByText("Plan a trip to Lamu")).toBeTruthy();
    expect(screen.getByText("Working: step 2 of 4")).toBeTruthy();
    expect(screen.getByRole("group", { name: "Long task: Plan a trip to Lamu" })).toBeTruthy();
  });

  it("lets only the state line speak inside the thread's live region", () => {
    show(taskOf());
    expect(document.querySelector(".lt")?.getAttribute("aria-live")).toBe("off");
    expect(screen.getByText("Working: step 2 of 4").getAttribute("aria-live")).toBe("polite");
  });

  it("shows each step with its agent, its state and its summary", () => {
    show(taskOf());
    expect(screen.getByText("Point Gander")).toBeTruthy();
    expect(screen.getAllByText("Forager Gosling")).toHaveLength(2);
    expect(screen.getByText("Sentinel Goose")).toBeTruthy();
    expect(screen.getByText("Split it into three parts.")).toBeTruthy();
    expect(screen.getAllByText("Waiting")).toHaveLength(2);
    expect(screen.getByText("Working")).toBeTruthy();
    expect(screen.getByText("Done")).toBeTruthy();
  });

  it("names an agent of the household's by the name it was saved under", () => {
    show(taskOf({ steps: [stepOf("a", { role: "study-planner", state: "running", attempts: 1 })] }));
    expect(screen.getByText("study-planner")).toBeTruthy();
  });

  it("says when a step is on another attempt, and why one could not finish", () => {
    show(
      taskOf({
        steps: [
          stepOf("a", { state: "running", attempts: 2 }),
          stepOf("b", { state: "failed", attempts: 3, failure: "The ferry site did not answer." }),
        ],
      }),
    );
    expect(screen.getByText("Working (attempt 2)")).toBeTruthy();
    expect(screen.getByText("Couldn't finish (attempt 3)")).toBeTruthy();
    expect(screen.getByText("The ferry site did not answer.")).toBeTruthy();
  });

  it("says Planning before it has any steps, and draws no step list", () => {
    show(taskOf({ state: "planning", steps: [] }));
    expect(screen.getByText("Planning")).toBeTruthy();
    expect(document.querySelector("ol")).toBeNull();
  });

  it("says so when it is paused or waiting on a person", () => {
    show(taskOf({ state: "paused" }));
    expect(screen.getByText("Paused")).toBeTruthy();
    cleanup();
    show(taskOf({ state: "awaiting_approval" }));
    expect(screen.getByText("Waiting for your approval")).toBeTruthy();
  });

  it("is drawn the same on the hub, with its own size class", () => {
    show(taskOf(), {}, "hub");
    expect(document.querySelector(".lt.lt--hub")).not.toBeNull();
  });
});

describe("a deep research task", () => {
  it("is labelled Deep research where a general task says it is a long task", () => {
    show(taskOf({ kind: "research", goal: "What changed in Kenya's tax rules?" }));
    expect(screen.getByRole("group", { name: "Deep research: What changed in Kenya's tax rules?" })).toBeTruthy();
    expect(screen.queryByRole("group", { name: /^Long task:/ })).toBeNull();
    expect(document.querySelector(".lt")?.getAttribute("data-kind")).toBe("research");
  });

  it("says Deep research on the card, above what it is for", () => {
    show(taskOf({ kind: "research" }));
    const kicker = document.querySelector(".lt__kind");
    expect(kicker?.textContent).toBe("Deep research");
    expect(kicker?.nextElementSibling?.className).toBe("lt__head");
  });

  it("keeps the same steps, state line and Stop as any other task", () => {
    show(taskOf({ kind: "research" }));
    expect(screen.getByText("Working: step 2 of 4")).toBeTruthy();
    expect(stopButton()).not.toBeNull();
  });

  it("is drawn the same on the hub", () => {
    show(taskOf({ kind: "research" }), {}, "hub");
    expect(document.querySelector(".lt.lt--hub .lt__kind")).not.toBeNull();
  });

  it.each([{ kind: "general" as const }, {}])("says nothing extra for a task of kind %o", (over) => {
    show(taskOf(over));
    expect(document.querySelector(".lt__kind")).toBeNull();
    expect(screen.getByRole("group", { name: "Long task: Plan a trip to Lamu" })).toBeTruthy();
    expect(document.querySelector(".lt")?.getAttribute("data-kind")).toBe("general");
  });
});

describe("stopping a task", () => {
  it("asks first, in words that say what happens to a step under way", async () => {
    const longTasks = show(taskOf());
    fireEvent.click(stopButton() as HTMLElement);
    await waitFor(() => expect(longTasks.stop).toHaveBeenCalledWith(TASK_ID));
    expect(confirmMock).toHaveBeenCalledWith("Stop this task? A step that is already under way finishes first.", {
      title: "Stop task",
      confirmLabel: "Stop task",
      destructive: true,
    });
  });

  it("leaves the task alone when the person says no", async () => {
    confirmMock.mockResolvedValue(false);
    const longTasks = show(taskOf());
    fireEvent.click(stopButton() as HTMLElement);
    await waitFor(() => expect(confirmMock).toHaveBeenCalled());
    expect(longTasks.stop).not.toHaveBeenCalled();
  });

  it("shows Stopping while the pond is told, and cannot be pressed twice", () => {
    show(taskOf(), { stopping: { [TASK_ID]: true } });
    const button = screen.getByRole("button", { name: /^Stop the task/ }) as HTMLButtonElement;
    expect(button.textContent).toBe("Stopping...");
    expect(button.disabled).toBe(true);
  });

  it("says so, as an alert, when the pond could not stop it", () => {
    show(taskOf(), { stopErrors: { [TASK_ID]: "Could not stop it. boom" } });
    expect(screen.getByRole("alert").textContent).toContain("Could not stop it. boom");
    expect(stopButton()).not.toBeNull();
  });

  it("is offered for a paused task and one waiting on approval", () => {
    show(taskOf({ state: "paused" }));
    expect(stopButton()).not.toBeNull();
    cleanup();
    show(taskOf({ state: "awaiting_approval" }));
    expect(stopButton()).not.toBeNull();
  });
});

describe("a long task that has ended", () => {
  it.each([
    ["completed", "Done"],
    ["failed", "Couldn't finish"],
    ["cancelled", "Stopped"],
  ] as const)("says %s as %s, with nothing to stop", (state, words) => {
    show(taskOf({ state, steps: [] }));
    expect(screen.getByText(words)).toBeTruthy();
    expect(stopButton()).toBeNull();
    expect(document.querySelector(".lt")?.getAttribute("data-state")).toBe(state);
  });

  it("offers no result for a task that failed or was stopped", () => {
    show(taskOf({ state: "failed", artifacts: [artifactOf()], result_artifact_id: "art-1" }));
    expect(screen.queryByRole("button", { name: "Read the result" })).toBeNull();
  });

  it("offers none for a finished task that made no result", () => {
    show(doneTask({ artifacts: [], result_artifact_id: null }));
    expect(screen.queryByRole("button", { name: "Read the result" })).toBeNull();
  });
});

const REPORT: LongTaskReport = {
  id: TASK_ID,
  title: "Kenya tax rules",
  state: "completed",
  kind: "research",
  markdown:
    "# Kenya tax rules\n\nA short summary.\n\n## Income tax\n\n| Band | Rate |\n|---|---|\n| First | 10% |\n\n## Packing\n\n- [ ] passport\n\n## Sources\n\n- [KRA](https://www.kra.go.ke/)\n",
  sections: 2,
  sources: [{ title: "KRA", url: "https://www.kra.go.ke/" }],
  calendar_events: 0,
};

const readButton = () => screen.getByRole("button", { name: "Read the result" });

describe("reading a result", () => {
  it("shows the result's summary, and reads nothing until asked", () => {
    show(doneTask());
    expect(screen.getByText("Three days in Lamu, with ferry times.")).toBeTruthy();
    expect(readButton()).toBeTruthy();
    expect(api.getLongTaskReport).not.toHaveBeenCalled();
    expect(api.getLongTaskArtifact).not.toHaveBeenCalled();
    expect(screen.queryByRole("dialog")).toBeNull();
  });
});

describe("reading the whole report", () => {
  it("opens it on its own sheet, titled, with its sections and what it cites", async () => {
    vi.mocked(api.getLongTaskReport).mockResolvedValue(REPORT);
    show(doneTask());
    fireEvent.click(readButton());

    const dialog = await screen.findByRole("dialog", { name: "Kenya tax rules" });
    expect(api.getLongTaskReport).toHaveBeenCalledWith(TASK_ID);
    expect(api.getLongTaskArtifact).not.toHaveBeenCalled();
    expect(within(dialog).getByRole("heading", { name: "Income tax" }).tagName).toBe("H3");
    expect(within(dialog).getByRole("table")).toBeTruthy();
    expect(within(dialog).getByRole("checkbox", { name: "passport" }).getAttribute("aria-checked")).toBe("false");
    expect(within(dialog).getByRole("link", { name: "KRA" }).getAttribute("target")).toBe("_blank");
    expect(within(dialog).getByText("2 sections, 1 source")).toBeTruthy();
  });

  it("keeps the card's own preview to the one line", async () => {
    vi.mocked(api.getLongTaskReport).mockResolvedValue(REPORT);
    show(doneTask());
    fireEvent.click(readButton());
    await screen.findByRole("dialog");
    const card = document.querySelector(".lt") as HTMLElement;
    expect(card.querySelector(".lt__result")).toBeNull();
    expect(card.textContent).not.toContain("A short summary.");
    expect(card.querySelector(".lt__resultline")?.textContent).toBe("Three days in Lamu, with ferry times.");
  });

  it("shows a finished short answer whole, with no sheet to open", () => {
    show(
      doneTask({
        answer: "short",
        answer_text: "Jerry was at the front door at 16:10. [Camera events](https://pond.local/events)",
      }),
    );
    const card = document.querySelector(".lt") as HTMLElement;
    expect(card.querySelector(".lt__answer")?.textContent).toContain("Jerry was at the front door at 16:10.");
    expect(card.querySelector(".lt__resultline")).toBeNull();
    expect(screen.queryByRole("button", { name: "Read the result" })).toBeNull();
  });

  it("keeps a report, or a short answer from an older pond, to its line and its sheet", () => {
    show(doneTask({ answer: "short", answer_text: null }));
    expect(document.querySelector(".lt__resultline")).toBeTruthy();
    expect(readButton()).toBeTruthy();
  });

  it("shows Opening while the report is on its way", async () => {
    let arrive: (r: LongTaskReport) => void = () => {};
    vi.mocked(api.getLongTaskReport).mockReturnValue(new Promise<LongTaskReport>((resolve) => (arrive = resolve)));
    show(doneTask());
    fireEvent.click(readButton());
    const opening = await screen.findByRole("button", { name: "Opening..." });
    // Not disabled, which would drop focus; a second press does nothing.
    expect(opening.getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(opening);
    expect(api.getLongTaskReport).toHaveBeenCalledTimes(1);
    arrive(REPORT);
    await screen.findByRole("dialog");
  });

  it("closes on its Close button, and the card gets its focus back", async () => {
    vi.mocked(api.getLongTaskReport).mockResolvedValue(REPORT);
    show(doneTask());
    readButton().focus();
    fireEvent.click(readButton());
    await screen.findByRole("dialog");
    fireEvent.click(screen.getByRole("button", { name: "Close the report" }));
    expect(screen.queryByRole("dialog")).toBeNull();
    expect(document.activeElement).toBe(readButton());
  });

  it("closes on Escape", async () => {
    vi.mocked(api.getLongTaskReport).mockResolvedValue(REPORT);
    show(doneTask());
    fireEvent.click(readButton());
    await screen.findByRole("dialog");
    fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
    expect(screen.queryByRole("dialog")).toBeNull();
  });

  it("says why it could not be opened, and the next press tries again", async () => {
    vi.mocked(api.getLongTaskReport).mockRejectedValueOnce(new ApiError(403, "nobody identified")).mockResolvedValue(REPORT);
    show(doneTask());
    fireEvent.click(readButton());
    expect((await screen.findByRole("alert")).textContent).toContain("Couldn't open the result. nobody identified");
    expect(screen.queryByRole("dialog")).toBeNull();
    fireEvent.click(readButton());
    await screen.findByRole("dialog");
    expect(screen.queryByRole("alert")).toBeNull();
  });
});

describe("reading a result on a pond that has no report route", () => {
  const BODY = "# Plan\n\n- ferry at **7:00**\n- [the map](https://example.com/map)\n\n```\nraw <b>text</b>\n```";

  it("opens the Result itself on the same sheet, as markdown and never as html", async () => {
    vi.mocked(api.getLongTaskReport).mockResolvedValue(null);
    vi.mocked(api.getLongTaskArtifact).mockResolvedValue({ id: "art-1", title: "Trip plan", kind: "result", summary: "", body: BODY });
    show(doneTask());
    fireEvent.click(readButton());

    const dialog = await screen.findByRole("dialog", { name: "Trip plan" });
    expect(api.getLongTaskArtifact).toHaveBeenCalledWith(TASK_ID, "art-1");
    const strong = within(dialog).getByText("7:00");
    expect(strong.tagName).toBe("STRONG");
    expect(within(dialog).getByRole("link", { name: "the map" }).getAttribute("href")).toBe("https://example.com/map");
    expect(within(dialog).getByRole("heading", { name: "Plan" }).tagName).toBe("H3");
    expect(dialog.querySelector("pre")?.textContent).toBe("raw <b>text</b>");
    expect(dialog.querySelector("b")).toBeNull();
  });

  it("says when the Result could not be fetched, and lets the person try again", async () => {
    vi.mocked(api.getLongTaskReport).mockResolvedValue(null);
    vi.mocked(api.getLongTaskArtifact)
      .mockRejectedValueOnce(new Error("offline"))
      .mockResolvedValue({ id: "art-1", title: "Trip plan", kind: "result", summary: "", body: "Second try" });
    show(doneTask());
    fireEvent.click(readButton());
    expect((await screen.findByRole("alert")).textContent).toContain("Couldn't open the result. offline");
    fireEvent.click(readButton());
    await screen.findByText("Second try");
    expect(screen.queryByRole("alert")).toBeNull();
  });
});

describe("a task that is not on screen yet", () => {
  it("says it is checking, as busy", () => {
    render(<LongTaskCard id={TASK_ID} longTasks={fakeLongTasks()} variant="classic" />);
    expect(screen.getByText("Checking on this task...")).toBeTruthy();
    expect(document.querySelector("[aria-busy='true']")).not.toBeNull();
  });

  it("says the pond no longer has it, without offering anything", () => {
    render(<LongTaskCard id={TASK_ID} longTasks={fakeLongTasks({ gone: { [TASK_ID]: true } })} variant="classic" />);
    expect(screen.getByText("This task is no longer on the pond.")).toBeTruthy();
    expect(screen.queryByRole("button")).toBeNull();
  });

  it("says it could not be read, and Try again reads it once more", () => {
    const retry = vi.fn();
    render(<LongTaskCard id={TASK_ID} longTasks={fakeLongTasks({ unreadable: { [TASK_ID]: "offline" }, retry })} variant="classic" />);
    expect(screen.getByText("Couldn't check on this task just now.")).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(retry).toHaveBeenCalledWith(TASK_ID);
  });
});

describe("the cards of a conversation", () => {
  it("draws one card per id a message points at", () => {
    const longTasks = fakeLongTasks({ tasks: { [TASK_ID]: taskOf(), [OTHER_ID]: doneTask({ id: OTHER_ID, goal: "Book the ferry" }) } });
    render(<LongTaskCards ids={[TASK_ID, OTHER_ID]} longTasks={longTasks} variant="classic" />);
    expect(screen.getByText("Plan a trip to Lamu")).toBeTruthy();
    expect(screen.getByText("Book the ferry")).toBeTruthy();
  });

  it("draws nothing for a message with no task, or on a pond that has none", () => {
    const longTasks = fakeLongTasks({ tasks: { [TASK_ID]: taskOf() } });
    const { container, rerender } = render(<LongTaskCards ids={undefined} longTasks={longTasks} variant="classic" />);
    expect(container.textContent).toBe("");
    rerender(<LongTaskCards ids={[]} longTasks={longTasks} variant="classic" />);
    expect(container.textContent).toBe("");
    rerender(<LongTaskCards ids={[TASK_ID]} longTasks={{ ...longTasks, supported: false }} variant="classic" />);
    expect(container.textContent).toBe("");
  });

  it("gathers the tasks no message points at under one label, and only those", () => {
    const longTasks = fakeLongTasks({ tasks: { [TASK_ID]: taskOf(), [OTHER_ID]: doneTask({ id: OTHER_ID, goal: "Book the ferry" }) } });
    render(<LongTaskStrays anchored={new Set([TASK_ID])} longTasks={longTasks} variant="classic" />);
    expect(screen.getByRole("region", { name: "Long tasks in this conversation" })).toBeTruthy();
    expect(screen.getByText("Book the ferry")).toBeTruthy();
    expect(screen.queryByText("Plan a trip to Lamu")).toBeNull();
  });

  it("names the group for deep research when that is all it holds", () => {
    const longTasks = fakeLongTasks({ tasks: { [TASK_ID]: taskOf({ kind: "research" }) } });
    render(<LongTaskStrays anchored={new Set()} longTasks={longTasks} variant="classic" />);
    expect(screen.getByRole("region", { name: "Deep research in this conversation" })).toBeTruthy();
    expect(screen.queryByText("Long tasks in this conversation")).toBeNull();
  });

  it("keeps the long-task name for a group that mixes kinds", () => {
    const longTasks = fakeLongTasks({
      tasks: { [TASK_ID]: taskOf({ kind: "research" }), [OTHER_ID]: doneTask({ id: OTHER_ID, goal: "Book the ferry" }) },
    });
    render(<LongTaskStrays anchored={new Set()} longTasks={longTasks} variant="classic" />);
    expect(screen.getByRole("region", { name: "Long tasks in this conversation" })).toBeTruthy();
  });

  it("draws no group when every task is anchored, or on a pond that has none", () => {
    const longTasks = fakeLongTasks({ tasks: { [TASK_ID]: taskOf() } });
    const { container, rerender } = render(<LongTaskStrays anchored={new Set([TASK_ID])} longTasks={longTasks} variant="classic" />);
    expect(container.textContent).toBe("");
    rerender(<LongTaskStrays anchored={new Set()} longTasks={{ ...longTasks, supported: false }} variant="classic" />);
    expect(container.textContent).toBe("");
  });
});
