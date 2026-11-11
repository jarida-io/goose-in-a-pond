// A schedule's corner card: the status as a word and a mark on the tile, never a stripe or a pill,
// a moving foot line only while it runs, and its words from the strings file.
import { render, screen, cleanup, fireEvent } from "@testing-library/react";
import { afterEach, describe, expect, it, vi } from "vitest";
import "../i18n";
import type { ScheduleToast } from "../state/reducer";

const appState = { scheduleToasts: [] as ScheduleToast[] };
const dispatch = vi.fn();
vi.mock("../state/useAppContext", () => ({
  useAppState: () => appState,
  useAppDispatch: () => dispatch,
}));

import { FINISHED_SHOWN, ToastContainer } from "./Toast";

function toast(over: Partial<ScheduleToast> = {}): ScheduleToast {
  return { id: "t1", schedule_id: "s1", schedule_label: "News", status: "completed", timestamp: 0, ...over };
}

afterEach(() => {
  cleanup();
  dispatch.mockReset();
  appState.scheduleToasts = [];
});

describe("ToastContainer", () => {
  it("says a finished run is done in words, with a mark on its tile and no stripe or pill", () => {
    appState.scheduleToasts = [toast({ result: "Schedule triggered **successfully**" })];
    const { container } = render(<ToastContainer />);
    const card = container.querySelector(".toast-card")!;
    expect(card.getAttribute("data-status")).toBe("completed");
    expect(card.textContent).toContain("Schedule");
    expect(card.textContent).toContain("Done");
    expect(card.textContent).toContain("Schedule triggered successfully");
    expect(card.querySelector(".toast-card__mark")).toBeTruthy();
    expect(card.querySelector(".toast-card__bar, .toast-card__badge")).toBeNull();
    expect(card.querySelector(".toast-card__progress")).toBeNull();
  });

  it("shows a moving foot line while running, and no link or mark yet", () => {
    appState.scheduleToasts = [toast({ id: "routine-1", status: "running" })];
    const { container } = render(<ToastContainer />);
    const card = container.querySelector(".toast-card")!;
    expect(card.textContent).toContain("Routine");
    expect(card.textContent).toContain("Running");
    expect(card.querySelector(".toast-card__progress")).toBeTruthy();
    expect(card.querySelector(".toast-card__mark")).toBeNull();
    expect(screen.queryByText("View in Notifications")).toBeNull();
  });

  it("shows a failure's error, and opens the notifications or dismisses on request", () => {
    appState.scheduleToasts = [toast({ status: "failed", error: "The model timed out" })];
    render(<ToastContainer />);
    expect(screen.getByText("Failed")).toBeTruthy();
    expect(screen.getByText("The model timed out")).toBeTruthy();
    fireEvent.click(screen.getByText("View in Notifications"));
    expect(dispatch).toHaveBeenCalledWith({ type: "SET_SECTION", payload: "notifications" });
    fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    expect(dispatch).toHaveBeenCalledWith({ type: "DISMISS_TOAST", payload: "t1" });
  });

  it("folds several running schedules into one card that names each", () => {
    appState.scheduleToasts = ["News", "Daily Brief", "Arsenal Results"].map((label, i) =>
      toast({ id: `t${i}`, schedule_label: label, status: "running", timestamp: i }),
    );
    const { container } = render(<ToastContainer />);
    const cards = container.querySelectorAll(".toast-card");
    expect(cards).toHaveLength(1);
    expect(cards[0].textContent).toContain("3 running");
    for (const label of ["News", "Daily Brief", "Arsenal Results"]) expect(cards[0].textContent).toContain(label);
    // Dismissing the group dismisses every run in it.
    fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    expect(dispatch).toHaveBeenCalledTimes(3);
  });

  it("shows the newest finished runs and counts the rest on one line", () => {
    appState.scheduleToasts = Array.from({ length: FINISHED_SHOWN + 2 }, (_, i) =>
      toast({ id: `f${i}`, schedule_label: `Run ${i}`, timestamp: i }),
    );
    const { container } = render(<ToastContainer />);
    const titles = [...container.querySelectorAll(".toast-card__title")].map((n) => n.textContent);
    expect(titles).toEqual(["Run 4", "Run 3", "Run 2"]);
    fireEvent.click(screen.getByText("2 more finished"));
    expect(dispatch).toHaveBeenCalledWith({ type: "SET_SECTION", payload: "notifications" });
  });
});
