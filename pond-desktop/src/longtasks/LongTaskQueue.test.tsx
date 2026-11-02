import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { api } from "../api/PondApiClient";
import { ApiError, type LongTask } from "../api/types";
import { LongTaskCard } from "./LongTaskCard";
import { fakeLongTasks, OTHER_ID, TASK_ID, taskOf } from "./longTaskFixtures";

vi.mock("../api/PondApiClient", () => ({
  api: { cancelLongTask: vi.fn(), getLongTaskArtifact: vi.fn(), getLongTaskReport: vi.fn(), getLongTaskExport: vi.fn() },
}));

vi.mock("../components/shared", async () => {
  const actual = (await vi.importActual("../components/shared")) as Record<string, unknown>;
  return { ...actual, useConfirm: () => vi.fn(async () => true) };
});

const FRONT_ID = "5f0a3c2e-9b1d-4e6a-8c7b-2d4e6f8a0b1c";

beforeEach(() => {
  vi.resetAllMocks();
});

afterEach(() => {
  cleanup();
});

/** A research task asked for behind two of the member's own and one of another member's. */
function waiting(over: Partial<LongTask> = {}): LongTask {
  return taskOf({
    goal: "Plan a trip for 4 to Arusha",
    state: "planning",
    steps: [],
    queue: {
      position: 4,
      of: 4,
      ahead: [
        { id: FRONT_ID, goal: "Plan a four-day trip to Arusha", state: "running" },
        { another_member: true },
        { id: OTHER_ID, goal: "Plan for a trip for 4 to Arusha", state: "planning" },
      ],
    },
    ...over,
  });
}

function show(task: LongTask, variant: "classic" | "hub" = "classic") {
  const longTasks = fakeLongTasks({ tasks: { [task.id]: task }, retry: vi.fn() });
  render(<LongTaskCard id={task.id} longTasks={longTasks} variant={variant} />);
  return longTasks;
}

const line = () => within(screen.getByRole("region", { name: "Tasks in line" }));

describe("a task waiting behind others", () => {
  it("says it is waiting its turn and who is ahead, naming only the member's own tasks", () => {
    show(waiting());
    expect(screen.getByText("Waiting its turn: 4th in line")).toBeTruthy();
    expect(line().getByText("Ahead of this one: 2 of your tasks and 1 for another member.")).toBeTruthy();
    expect(line().getByRole("group", { name: /Plan a four-day trip to Arusha.*Working now/ })).toBeTruthy();
    expect(line().getByRole("group", { name: /Plan for a trip for 4 to Arusha.*3rd in line/ })).toBeTruthy();
    expect(line().getByRole("group", { name: /Plan a trip for 4 to Arusha \(this one\).*4th in line/ })).toBeTruthy();
    expect(line().getAllByRole("group")).toHaveLength(3);
  });

  it("offers one way to stop this task: its row in the line, not a second Stop on the card", () => {
    show(waiting());
    expect(screen.queryByRole("button", { name: /^Stop the task/ })).toBeNull();
    cleanup();
    show(waiting({ queue: { position: 2, of: 2, ahead: [{ another_member: true }] } }));
    expect(screen.getByRole("button", { name: /^Stop the task/ })).toBeTruthy();
  });

  it("starts with everything kept going, and offers no stop until one is picked", () => {
    show(waiting());
    for (const keep of line().getAllByRole("radio", { name: "Keep going" })) expect((keep as HTMLInputElement).checked).toBe(true);
    expect(line().getByText("Everything here keeps going.")).toBeTruthy();
    expect(line().queryByRole("button", { name: /^Stop \d/ })).toBeNull();
  });

  it("stops only the tasks picked, then says how many it stopped", async () => {
    vi.mocked(api.cancelLongTask).mockResolvedValue(undefined);
    const longTasks = show(waiting());
    const front = line().getByRole("group", { name: /Plan a four-day trip/ });
    const older = line().getByRole("group", { name: /Plan for a trip for 4/ });
    fireEvent.click(within(front).getByRole("radio", { name: "Stop" }));
    fireEvent.click(within(older).getByRole("radio", { name: "Stop" }));
    fireEvent.click(within(older).getByRole("radio", { name: "Keep going" }));
    fireEvent.click(within(older).getByRole("radio", { name: "Stop" }));

    fireEvent.click(line().getByRole("button", { name: "Stop 2 tasks" }));
    await waitFor(() => expect(line().getByRole("status").textContent).toBe("Stopped 2 tasks."));
    expect(vi.mocked(api.cancelLongTask).mock.calls.map((c) => c[0]).sort()).toEqual([FRONT_ID, OTHER_ID].sort());
    // Only this card's task is read again: the others belong to other conversations.
    expect(longTasks.retry).toHaveBeenCalledWith(TASK_ID);
    expect(longTasks.retry).not.toHaveBeenCalledWith(FRONT_ID);
  });

  it("counts a task that had already ended as stopped, and names one that could not be", async () => {
    vi.mocked(api.cancelLongTask).mockImplementation(async (id: string) => {
      if (id === FRONT_ID) throw new ApiError(409, "the task has already completed");
      throw new Error("the pond did not answer");
    });
    show(waiting());
    fireEvent.click(within(line().getByRole("group", { name: /Plan a four-day trip/ })).getByRole("radio", { name: "Stop" }));
    fireEvent.click(within(line().getByRole("group", { name: /Plan for a trip for 4/ })).getByRole("radio", { name: "Stop" }));
    fireEvent.click(line().getByRole("button", { name: "Stop 2 tasks" }));
    await waitFor(() => expect(line().getByRole("alert").textContent).toContain("Could not stop Plan for a trip for 4 to Arusha: the pond did not answer"));
    expect(line().getByRole("status").textContent).toBe("Stopped 1 task.");
  });

  it("offers no choice when only another member's task is ahead", () => {
    show(waiting({ queue: { position: 2, of: 2, ahead: [{ another_member: true }] } }));
    expect(line().getByText("Ahead of this one: 1 for another member.")).toBeTruthy();
    expect(line().queryAllByRole("radio")).toHaveLength(0);
  });

  it("shows no line at the front, once ended, or from an older pond", () => {
    show(waiting({ queue: { position: 1, of: 2, ahead: [] } }));
    expect(screen.queryByRole("region", { name: "Tasks in line" })).toBeNull();
    cleanup();
    show(waiting({ state: "cancelled", queue: null }));
    expect(screen.queryByRole("region", { name: "Tasks in line" })).toBeNull();
    cleanup();
    show(waiting({ queue: undefined }));
    expect(screen.queryByRole("region", { name: "Tasks in line" })).toBeNull();
  });

  it("gives every choice the hub's touch height", () => {
    show(waiting(), "hub");
    expect(document.querySelector(".lt--hub .lt__queueopt")).toBeTruthy();
  });
});
